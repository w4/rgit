use std::sync::Arc;

use askama::Template;
use axum::{extract::Query, response::Response, Extension};
use serde::Deserialize;

use crate::{
    git::Commit,
    into_response,
    methods::repo::{Repository, RepositoryPath, Result},
    Git,
};

#[derive(Template)]
#[template(path = "repo/commit.html")]
pub struct View {
    pub repo: Repository,
    pub commit: Arc<Commit>,
    pub branch: Option<Arc<str>>,
}

#[derive(Deserialize)]
pub struct UriQuery {
    pub id: Option<String>,
    #[serde(rename = "h")]
    pub branch: Option<Arc<str>>,
}

pub async fn handle(
    Extension(repo): Extension<Repository>,
    Extension(RepositoryPath(repository_path)): Extension<RepositoryPath>,
    Extension(git): Extension<Arc<Git>>,
    Query(query): Query<UriQuery>,
) -> Result<Response> {
    let open_repo = git.repo(repository_path, query.branch.clone()).await?;
    let commit = if let Some(commit) = query.id {
        open_repo.commit(&commit).await?
    } else {
        Arc::new(open_repo.latest_commit().await?)
    };

    Ok(into_response(&View {
        repo,
        commit,
        branch: query.branch,
    }))
}
