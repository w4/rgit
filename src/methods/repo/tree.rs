use askama::Template;
use axum::{extract::Query, response::IntoResponse, Extension};
use itertools::Itertools;
use serde::Deserialize;
use std::path::PathBuf;
use std::{
    fmt::{Display, Formatter},
    sync::Arc,
};

use crate::{
    git::{FileWithContent, PathDestination, TreeItem},
    into_response,
    methods::{
        filters,
        repo::{ChildPath, Repository, RepositoryPath, Result},
    },
    Git, ResponseEither,
};

#[derive(Deserialize)]
pub struct UriQuery {
    id: Option<String>,
    #[serde(default)]
    raw: bool,
    #[serde(rename = "h")]
    branch: Option<Arc<str>>,
}

impl Display for UriQuery {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let mut prefix = "?";

        if let Some(id) = self.id.as_deref() {
            write!(f, "{prefix}id={id}")?;
            prefix = "&";
        }

        if let Some(branch) = self.branch.as_deref() {
            write!(f, "{prefix}h={branch}")?;
        }

        Ok(())
    }
}

#[derive(Template)]
#[template(path = "repo/tree.html")]
#[allow(clippy::module_name_repetitions)]
pub struct TreeView {
    pub repo: Repository,
    pub items: Vec<TreeItem>,
    pub query: UriQuery,
    pub repo_path: PathBuf,
    pub branch: Option<Arc<str>>,
}

#[derive(Template)]
#[template(path = "repo/file.html")]
pub struct FileView {
    pub repo: Repository,
    pub repo_path: PathBuf,
    pub file: FileWithContent,
    pub branch: Option<Arc<str>>,
}

pub async fn handle(
    Extension(repo): Extension<Repository>,
    Extension(RepositoryPath(repository_path)): Extension<RepositoryPath>,
    Extension(ChildPath(child_path)): Extension<ChildPath>,
    Extension(git): Extension<Arc<Git>>,
    Query(query): Query<UriQuery>,
) -> Result<impl IntoResponse> {
    let open_repo = git.repo(repository_path, query.branch.clone()).await?;

    Ok(
        match open_repo
            .path(child_path.clone(), query.id.as_deref(), !query.raw)
            .await?
        {
            PathDestination::Tree(items) => {
                ResponseEither::Left(ResponseEither::Left(into_response(TreeView {
                    repo,
                    items,
                    branch: query.branch.clone(),
                    query,
                    repo_path: child_path.unwrap_or_default(),
                })))
            }
            PathDestination::File(file) if query.raw => ResponseEither::Right(file.content),
            PathDestination::File(file) => {
                ResponseEither::Left(ResponseEither::Right(into_response(FileView {
                    repo,
                    file,
                    branch: query.branch,
                    repo_path: child_path.unwrap_or_default(),
                })))
            }
        },
    )
}
