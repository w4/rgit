use askama::Template;
use axum::response::Html;
use axum::Extension;
use std::sync::Arc;

use super::filters;
use crate::{git::RepositoryMetadataList, Git};

#[allow(clippy::unused_async)]
pub async fn handle(Extension(git): Extension<Git>) -> Html<String> {
    #[derive(Template)]
    #[template(path = "index.html")]
    pub struct View {
        pub repositories: Arc<RepositoryMetadataList>,
    }

    Html(
        View {
            repositories: git.fetch_repository_metadata().await,
        }
        .render()
        .unwrap(),
    )
}
