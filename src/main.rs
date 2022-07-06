#![deny(clippy::pedantic)]

use axum::{
    body::Body, handler::Handler, http::HeaderValue, response::Response, routing::get, Extension,
    Router,
};
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

    let app = Router::new()
        .route("/", get(methods::index::handle))
        .route(
            "/style.css",
            get(static_css(include_bytes!("../statics/style.css"))),
        )
        .fallback(methods::repo::service.into_service())
        .layer(layer_fn(LoggingMiddleware))
        .layer(Extension(Git::default()));

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
