use std::sync::Arc;

use askama::Template;
use axum::{Extension, extract::Query, response::IntoResponse};
use serde::Deserialize;
use yoke::Yoke;

use crate::{
    Git,
    git::DetailedTag,
    into_response,
    methods::{
        filters,
        repo::{Repository, RepositoryPath, Result},
    },
};

#[derive(Deserialize)]
pub struct UriQuery {
    #[serde(rename = "h")]
    name: Arc<str>,
}

#[derive(Template)]
#[template(path = "repo/tag.html")]
pub struct View {
    repo: Repository,
    tag: Yoke<DetailedTag<'static>, Vec<u8>>,
    branch: Option<Arc<str>>,
}

pub async fn handle(
    Extension(repo): Extension<Repository>,
    Extension(RepositoryPath(repository_path)): Extension<RepositoryPath>,
    Extension(git): Extension<Arc<Git>>,
    Query(query): Query<UriQuery>,
) -> Result<impl IntoResponse> {
    let open_repo = git.repo(repository_path, Some(query.name.clone())).await?;
    let tag = open_repo.tag_info().await?;

    Ok(into_response(View {
        repo,
        tag,
        branch: Some(query.name),
    }))
}
