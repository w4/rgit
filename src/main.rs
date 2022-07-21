#![deny(clippy::pedantic)]

use askama::Template;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{
    body::Body, handler::Handler, http, http::HeaderValue, response::Response, routing::get,
    Extension, Router,
};
use bat::assets::HighlightingAssets;
use std::sync::Arc;
use std::time::Duration;
use syntect::html::ClassStyle;
use tower_layer::layer_fn;
use tracing::{info, instrument};

use crate::{git::Git, layers::logger::LoggingMiddleware};

mod database;
mod git;
mod git_cgi;
mod layers;
mod methods;
mod syntax_highlight;

const CRATE_VERSION: &str = clap::crate_version!();

#[tokio::main]
async fn main() {
    let subscriber = tracing_subscriber::fmt();
    #[cfg(debug_assertions)]
    let subscriber = subscriber.pretty();
    subscriber.init();

    let db = sled::Config::default()
        .use_compression(true)
        .path("/tmp/some-sled.db")
        .open()
        .unwrap();

    std::thread::spawn({
        let db = db.clone();

        move || loop {
            info!("Running periodic index");
            crate::database::indexer::run(&db);
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
        .layer(Extension(db));

    axum::Server::bind(&"127.0.0.1:3333".parse().unwrap())
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
