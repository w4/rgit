use std::sync::Arc;

use askama::Template;
use axum::{response::Response, Extension};

use crate::{
    git::ReadmeFormat,
    into_response,
    methods::repo::{Repository, RepositoryPath, Result},
    Git,
};

#[derive(Template)]
#[template(path = "repo/about.html")]
pub struct View {
    repo: Repository,
    readme: Option<(ReadmeFormat, Arc<str>)>,
}

pub async fn handle(
    Extension(repo): Extension<Repository>,
    Extension(RepositoryPath(repository_path)): Extension<RepositoryPath>,
    Extension(git): Extension<Arc<Git>>,
) -> Result<Response> {
    let open_repo = git.clone().repo(repository_path).await?;
    let readme = open_repo.readme().await?;

    Ok(into_response(&View { repo, readme }))
}
