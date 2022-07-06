#![deny(clippy::pedantic)]

use axum::{
    body::Body, handler::Handler, http::HeaderValue, response::Response, routing::get, Extension,
    Router,
};
use bat::assets::HighlightingAssets;
use std::sync::Arc;
use syntect::html::ClassStyle;
use tower_layer::layer_fn;

use crate::{git::Git, layers::logger::LoggingMiddleware};

mod git;
mod layers;
mod methods;

const CRATE_VERSION: &str = clap::crate_version!();

#[tokio::main]
async fn main() {
    let subscriber = tracing_subscriber::fmt();
    #[cfg(debug_assertions)]
    let subscriber = subscriber.pretty();
    subscriber.init();

    let bat_assets = HighlightingAssets::from_binary();
    let syntax_set = bat_assets.get_syntax_set().unwrap().clone();
    let theme = bat_assets.get_theme("GitHub");
    let css = Box::leak(
        syntect::html::css_for_theme_with_class_style(theme, ClassStyle::Spaced)
            .unwrap()
            .into_boxed_str()
            .into_boxed_bytes(),
    );

    let app = Router::new()
        .route("/", get(methods::index::handle))
        .route(
            "/style.css",
            get(static_css(include_bytes!("../statics/style.css"))),
        )
        .route("/highlight.css", get(static_css(css)))
        .fallback(methods::repo::service.into_service())
        .layer(layer_fn(LoggingMiddleware))
        .layer(Extension(Git::default()))
        .layer(Extension(Arc::new(syntax_set)));

    axum::Server::bind(&"127.0.0.1:3333".parse().unwrap())
        .serve(app.into_make_service_with_connect_info::<std::net::SocketAddr>())
        .await
        .unwrap();
}

fn static_css(content: &'static [u8]) -> impl Handler<()> {
    move || async move {
        let mut resp = Response::new(Body::from(content));
        resp.headers_mut()
            .insert("Content-Type", HeaderValue::from_static("text/css"));
        resp
    }
}
