use askama::Template;
use axum::response::Response;
use axum::Extension;
use std::sync::Arc;

use super::filters;
use crate::{git::RepositoryMetadataList, Git, into_response};

#[derive(Template)]
#[template(path = "index.html")]
pub struct View {
    pub repositories: Arc<RepositoryMetadataList>,
}

pub async fn handle(Extension(git): Extension<Arc<Git>>) -> Response {
    let repositories = git.fetch_repository_metadata().await;

    into_response(&View {
        repositories,
    })
}
