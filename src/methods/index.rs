use askama::Template;
use axum::response::Html;

use crate::{fetch_repository_metadata, git::RepositoryMetadataList};

#[allow(clippy::unused_async)]
pub async fn handle() -> Html<String> {
    #[derive(Template)]
    #[template(path = "index.html")]
    pub struct View {
        pub repositories: RepositoryMetadataList,
    }

    Html(
        View {
            repositories: fetch_repository_metadata(),
        }
        .render()
        .unwrap(),
    )
}
