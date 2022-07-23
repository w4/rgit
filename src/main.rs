#![deny(clippy::pedantic)]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::{sync::Arc, time::Duration};

use askama::Template;
use axum::{
    body::Body,
    handler::Handler,
    http,
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Extension, Router,
};
use bat::assets::HighlightingAssets;
use clap::Parser;
use syntect::html::ClassStyle;
use tower_http::cors::CorsLayer;
use tower_layer::layer_fn;
use tracing::{info, instrument};

use crate::{git::Git, layers::logger::LoggingMiddleware};

mod database;
mod git;
mod layers;
mod methods;
mod syntax_highlight;

const CRATE_VERSION: &str = clap::crate_version!();

#[derive(Parser, Debug)]
#[clap(author, version, about)]
pub struct Args {
    /// Path to a directory in which the Sled database should be stored, will be created if it doesn't already exist
    ///
    /// The Sled database is very quick to generate, so this can be pointed to temporary storage
    #[clap(short, long, value_parser)]
    db_store: PathBuf,
    /// The socket address to bind to (eg. 0.0.0.0:3333)
    bind_address: SocketAddr,
    /// The path in which your bare Git repositories reside (will be scanned recursively)
    scan_path: PathBuf,
}

#[tokio::main]
async fn main() {
    let args: Args = Args::parse();

    let subscriber = tracing_subscriber::fmt();
    #[cfg(debug_assertions)]
    let subscriber = subscriber.pretty();
    subscriber.init();

    let db = sled::Config::default()
        .use_compression(true)
        .path(&args.db_store)
        .open()
        .unwrap();

    std::thread::spawn({
        let db = db.clone();
        let scan_path = args.scan_path.clone();

        move || loop {
            info!("Running periodic index");
            crate::database::indexer::run(&scan_path, &db);
            info!("Finished periodic index");

            std::thread::sleep(Duration::from_secs(300));
        }
    });

    let bat_assets = HighlightingAssets::from_binary();
    let syntax_set = bat_assets.get_syntax_set().unwrap().clone();

    let theme = bat_assets.get_theme("GitHub");
    let css = syntect::html::css_for_theme_with_class_style(theme, ClassStyle::Spaced).unwrap();
    let css = Box::leak(
        format!(r#"@media (prefers-color-scheme: light){{{}}}"#, css)
            .into_boxed_str()
            .into_boxed_bytes(),
    );

    let dark_theme = bat_assets.get_theme("TwoDark");
    let dark_css =
        syntect::html::css_for_theme_with_class_style(dark_theme, ClassStyle::Spaced).unwrap();
    let dark_css = Box::leak(
        format!(r#"@media (prefers-color-scheme: dark){{{}}}"#, dark_css)
            .into_boxed_str()
            .into_boxed_bytes(),
    );

    let app = Router::new()
        .route("/", get(methods::index::handle))
        .route(
            "/style.css",
            get(static_css(include_bytes!(concat!(
                env!("OUT_DIR"),
                "/statics/css/style.css"
            )))),
        )
        .route("/highlight.css", get(static_css(css)))
        .route("/highlight-dark.css", get(static_css(dark_css)))
        .fallback(methods::repo::service.into_service())
        .layer(layer_fn(LoggingMiddleware))
        .layer(Extension(Arc::new(Git::new(syntax_set))))
        .layer(Extension(db))
        .layer(Extension(Arc::new(args.scan_path)))
        .layer(CorsLayer::new());

    axum::Server::bind(&args.bind_address)
        .serve(app.into_make_service_with_connect_info::<std::net::SocketAddr>())
        .await
        .unwrap();
}

fn static_css(content: &'static [u8]) -> impl Handler<()> {
    move || async move {
        let mut resp = Response::new(Body::from(content));
        resp.headers_mut().insert(
            http::header::CONTENT_TYPE,
            HeaderValue::from_static("text/css"),
        );
        resp
    }
}

#[instrument(skip(t))]
pub fn into_response<T: Template>(t: &T) -> Response {
    match t.render() {
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
