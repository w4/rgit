#![deny(clippy::pedantic)]

use std::{
    borrow::Cow,
    fmt::{Display, Formatter},
    future::IntoFuture,
    net::SocketAddr,
    path::PathBuf,
    str::FromStr,
    sync::{Arc, OnceLock},
    time::Duration,
};

use anyhow::Context;
use askama::Template;
use axum::{
    body::Body,
    http,
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Extension, Router,
};
use clap::Parser;
use const_format::formatcp;
use database::schema::SCHEMA_VERSION;
use rocksdb::{Options, SliceTransform};
use tokio::{
    net::TcpListener,
    signal::unix::{signal, SignalKind},
    sync::mpsc,
};
use tower_http::{cors::CorsLayer, timeout::TimeoutLayer};
use tower_layer::layer_fn;
use tracing::{error, info, instrument, warn};
use tracing_subscriber::{
    fmt::format::FmtSpan, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter,
};
use xxhash_rust::const_xxh3;

use crate::{
    database::schema::prefixes::{
        COMMIT_COUNT_FAMILY, COMMIT_FAMILY, REFERENCE_FAMILY, REPOSITORY_FAMILY, TAG_FAMILY,
    },
    git::Git,
    layers::logger::LoggingMiddleware,
    syntax_highlight::prime_highlighters,
    theme::Theme,
};

mod database;
mod git;
mod layers;
mod methods;
mod syntax_highlight;
mod theme;
mod unified_diff_builder;

const CRATE_VERSION: &str = clap::crate_version!();

const GLOBAL_CSS: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/statics/css/style.css"));
const GLOBAL_CSS_HASH: &str = const_hex::Buffer::<16, false>::new()
    .const_format(&const_xxh3::xxh3_128(GLOBAL_CSS).to_be_bytes())
    .as_str();

static HIGHLIGHT_CSS_HASH: OnceLock<Box<str>> = OnceLock::new();
static DARK_HIGHLIGHT_CSS_HASH: OnceLock<Box<str>> = OnceLock::new();

#[derive(Parser, Debug)]
#[clap(author, version, about)]
pub struct Args {
    /// Path to a directory in which the `RocksDB` database should be stored, will be created if it doesn't already exist
    ///
    /// The `RocksDB` database is very quick to generate, so this can be pointed to temporary storage
    #[clap(short, long, value_parser)]
    db_store: PathBuf,
    /// The socket address to bind to (eg. 0.0.0.0:3333)
    bind_address: SocketAddr,
    /// The path in which your bare Git repositories reside (will be scanned recursively)
    scan_path: PathBuf,
    /// Configures the metadata refresh interval (eg. "never" or "60s")
    #[clap(long, default_value_t = RefreshInterval::Duration(Duration::from_secs(300)))]
    refresh_interval: RefreshInterval,
    /// Configures the request timeout.
    #[clap(long, default_value_t = Duration::from_secs(10).into())]
    request_timeout: humantime::Duration,
}

#[derive(Debug, Clone, Copy)]
pub enum RefreshInterval {
    Never,
    Duration(Duration),
}

impl Display for RefreshInterval {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Never => write!(f, "never"),
            Self::Duration(s) => write!(f, "{}", humantime::format_duration(*s)),
        }
    }
}

impl FromStr for RefreshInterval {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == "never" {
            Ok(Self::Never)
        } else if let Ok(v) = humantime::parse_duration(s) {
            Ok(Self::Duration(v))
        } else {
            Err("must be seconds, a human readable duration (eg. '10m') or 'never'")
        }
    }
}

#[tokio::main]
#[allow(clippy::too_many_lines)]
async fn main() -> Result<(), anyhow::Error> {
    let args: Args = Args::parse();

    if std::env::var_os("RUST_LOG").is_none() {
        std::env::set_var("RUST_LOG", "info");
    }

    let logger_layer = tracing_subscriber::fmt::layer().with_span_events(FmtSpan::CLOSE);
    let env_filter = EnvFilter::from_default_env();

    tracing_subscriber::registry()
        .with(env_filter)
        .with(logger_layer)
        .init();

    let db = open_db(&args)?;

    let indexer_wakeup_task =
        run_indexer(db.clone(), args.scan_path.clone(), args.refresh_interval);

    let css = {
        let theme = toml::from_str::<Theme>(include_str!("../themes/github_light.toml"))
            .unwrap()
            .build_css();
        let css = Box::leak(
            format!(r#"@media (prefers-color-scheme: light){{{theme}}}"#)
                .into_boxed_str()
                .into_boxed_bytes(),
        );
        HIGHLIGHT_CSS_HASH.set(build_asset_hash(css)).unwrap();
        css
    };

    let dark_css = {
        let theme = toml::from_str::<Theme>(include_str!("../themes/onedark.toml"))
            .unwrap()
            .build_css();
        let css = Box::leak(
            format!(r#"@media (prefers-color-scheme: dark){{{theme}}}"#)
                .into_boxed_str()
                .into_boxed_bytes(),
        );
        DARK_HIGHLIGHT_CSS_HASH.set(build_asset_hash(css)).unwrap();
        css
    };

    let static_favicon = |content: &'static [u8]| {
        move || async move {
            let mut resp = Response::new(Body::from(content));
            resp.headers_mut().insert(
                http::header::CONTENT_TYPE,
                HeaderValue::from_static("image/x-icon"),
            );
            resp
        }
    };

    let static_css = |content: &'static [u8]| {
        move || async move {
            let mut resp = Response::new(Body::from(content));
            resp.headers_mut().insert(
                http::header::CONTENT_TYPE,
                HeaderValue::from_static("text/css"),
            );
            resp
        }
    };

    info!("Priming highlighters...");
    prime_highlighters();
    info!("Server starting up...");

    let app = Router::new()
        .route("/", get(methods::index::handle))
        .route(
            formatcp!("/style-{}.css", GLOBAL_CSS_HASH),
            get(static_css(GLOBAL_CSS)),
        )
        .route(
            &format!("/highlight-{}.css", HIGHLIGHT_CSS_HASH.get().unwrap()),
            get(static_css(css)),
        )
        .route(
            &format!(
                "/highlight-dark-{}.css",
                DARK_HIGHLIGHT_CSS_HASH.get().unwrap()
            ),
            get(static_css(dark_css)),
        )
        .route(
            "/favicon.ico",
            get(static_favicon(include_bytes!("../statics/favicon.ico"))),
        )
        .fallback(methods::repo::service)
        .layer(TimeoutLayer::new(args.request_timeout.into()))
        .layer(layer_fn(LoggingMiddleware))
        .layer(Extension(Arc::new(Git::new())))
        .layer(Extension(db))
        .layer(Extension(Arc::new(args.scan_path)))
        .layer(CorsLayer::new());

    let listener = TcpListener::bind(&args.bind_address).await?;
    let app = app.into_make_service_with_connect_info::<SocketAddr>();
    let server = axum::serve(listener, app).into_future();

    tokio::select! {
        res = server => res.context("failed to run server"),
        res = indexer_wakeup_task => res.context("failed to run indexer"),
        _ = tokio::signal::ctrl_c() => {
            info!("Received ctrl-c, shutting down");
            Ok(())
        }
    }
}

fn open_db(args: &Args) -> Result<Arc<rocksdb::DB>, anyhow::Error> {
    loop {
        let mut db_options = Options::default();
        db_options.create_missing_column_families(true);
        db_options.create_if_missing(true);

        let mut commit_family_options = Options::default();
        commit_family_options.set_prefix_extractor(SliceTransform::create(
            "commit_prefix",
            |input| input.split(|&c| c == b'\0').next().unwrap_or(input),
            None,
        ));

        let mut tag_family_options = Options::default();
        tag_family_options.set_prefix_extractor(SliceTransform::create_fixed_prefix(
            std::mem::size_of::<u64>(),
        )); // repository id prefix

        let db = rocksdb::DB::open_cf_with_opts(
            &db_options,
            &args.db_store,
            vec![
                (COMMIT_FAMILY, commit_family_options),
                (REPOSITORY_FAMILY, Options::default()),
                (TAG_FAMILY, tag_family_options),
                (REFERENCE_FAMILY, Options::default()),
                (COMMIT_COUNT_FAMILY, Options::default()),
            ],
        )?;

        let needs_schema_regen = match db.get("schema_version")? {
            Some(v) if v.as_slice() != SCHEMA_VERSION.as_bytes() => Some(Some(v)),
            Some(_) => None,
            None => {
                db.put("schema_version", SCHEMA_VERSION)?;
                None
            }
        };

        if let Some(version) = needs_schema_regen {
            let old_version = version
                .as_deref()
                .map_or(Cow::Borrowed("unknown"), String::from_utf8_lossy);

            warn!("Clearing outdated database ({old_version} != {SCHEMA_VERSION})");

            drop(db);
            rocksdb::DB::destroy(&Options::default(), &args.db_store)?;
        } else {
            break Ok(Arc::new(db));
        }
    }
}

async fn run_indexer(
    db: Arc<rocksdb::DB>,
    scan_path: PathBuf,
    refresh_interval: RefreshInterval,
) -> Result<(), tokio::task::JoinError> {
    let (indexer_wakeup_send, mut indexer_wakeup_recv) = mpsc::channel(10);

    std::thread::spawn(move || loop {
        info!("Running periodic index");
        crate::database::indexer::run(&scan_path, &db);
        info!("Finished periodic index");

        if indexer_wakeup_recv.blocking_recv().is_none() {
            break;
        }
    });

    tokio::spawn({
        let mut sighup = signal(SignalKind::hangup()).expect("could not subscribe to sighup");
        let build_sleeper = move || async move {
            match refresh_interval {
                RefreshInterval::Never => futures_util::future::pending().await,
                RefreshInterval::Duration(v) => tokio::time::sleep(v).await,
            };
        };

        async move {
            loop {
                tokio::select! {
                    _ = sighup.recv() => {},
                    () = build_sleeper() => {},
                }

                if indexer_wakeup_send.send(()).await.is_err() {
                    error!("Indexing thread has died and is no longer accepting wakeup messages");
                }
            }
        }
    })
    .await
}

#[must_use]
pub fn build_asset_hash(v: &[u8]) -> Box<str> {
    let hasher = const_xxh3::xxh3_128(v);
    let out = const_hex::encode(hasher.to_be_bytes());
    Box::from(out)
}

pub struct TemplateResponse<T> {
    template: T,
}

impl<T: Template> IntoResponse for TemplateResponse<T> {
    #[instrument(skip_all)]
    fn into_response(self) -> Response {
        match self.template.render() {
            Ok(body) => {
                let headers = [(
                    http::header::CONTENT_TYPE,
                    HeaderValue::from_static(T::MIME_TYPE),
                )];

                (headers, body).into_response()
            }
            Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        }
    }
}

pub fn into_response<T: Template>(template: T) -> impl IntoResponse {
    TemplateResponse { template }
}

pub enum ResponseEither<A, B> {
    Left(A),
    Right(B),
}

impl<A: IntoResponse, B: IntoResponse> IntoResponse for ResponseEither<A, B> {
    fn into_response(self) -> Response {
        match self {
            Self::Left(a) => a.into_response(),
            Self::Right(b) => b.into_response(),
        }
    }
}
