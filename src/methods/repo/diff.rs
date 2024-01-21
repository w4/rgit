use std::sync::Arc;

use askama::Template;
use axum::{
    extract::Query,
    http::HeaderValue,
    response::{IntoResponse, Response},
    Extension,
};

use crate::{
    git::Commit,
    http, into_response,
    methods::{
        filters,
        repo::{commit::UriQuery, Repository, RepositoryPath, Result},
    },
    Git,
};

#[derive(Template)]
#[template(path = "repo/diff.html")]
pub struct View {
    pub repo: Repository,
    pub commit: Arc<Commit>,
    pub branch: Option<Arc<str>>,
}

pub async fn handle(
    Extension(repo): Extension<Repository>,
    Extension(RepositoryPath(repository_path)): Extension<RepositoryPath>,
    Extension(git): Extension<Arc<Git>>,
    Query(query): Query<UriQuery>,
) -> Result<impl IntoResponse> {
    let open_repo = git.repo(repository_path, query.branch.clone()).await?;
    let commit = if let Some(commit) = query.id {
        open_repo.commit(&commit).await?
    } else {
        Arc::new(open_repo.latest_commit().await?)
    };

    Ok(into_response(View {
        repo,
        commit,
        branch: query.branch,
    }))
}

pub async fn handle_plain(
    Extension(RepositoryPath(repository_path)): Extension<RepositoryPath>,
    Extension(git): Extension<Arc<Git>>,
    Query(query): Query<UriQuery>,
) -> Result<Response> {
    let open_repo = git.repo(repository_path, query.branch).await?;
    let commit = if let Some(commit) = query.id {
        open_repo.commit(&commit).await?
    } else {
        Arc::new(open_repo.latest_commit().await?)
    };

    let headers = [(
        http::header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain"),
    )];

    Ok((headers, commit.diff_plain.clone()).into_response())
}
