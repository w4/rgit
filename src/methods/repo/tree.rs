use std::{
    fmt::{Display, Formatter},
    sync::Arc,
};

use askama::Template;
use axum::{extract::Query, http, response::IntoResponse, response::Response, Extension};
use serde::Deserialize;

use crate::{
    git::{FileWithContent, PathDestination, TreeItem},
    into_response,
    methods::{
        filters,
        repo::{ChildPath, Repository, RepositoryPath, Result},
    },
    Git,
};

#[derive(Deserialize)]
pub struct UriQuery {
    id: Option<String>,
    #[serde(rename = "h")]
    branch: Option<String>,
    #[serde(default)]
    raw: bool,
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
}

#[derive(Template)]
#[template(path = "repo/file.html")]
pub struct FileView {
    pub repo: Repository,
    pub file: FileWithContent,
}

pub async fn handle(
    Extension(repo): Extension<Repository>,
    Extension(RepositoryPath(repository_path)): Extension<RepositoryPath>,
    Extension(ChildPath(child_path)): Extension<ChildPath>,
    Extension(git): Extension<Arc<Git>>,
    Query(query): Query<UriQuery>,
) -> Result<Response> {
    let open_repo = git.repo(repository_path).await?;

    Ok(
        match open_repo
            .path(
                child_path,
                query.id.as_deref(),
                query.branch.clone(),
                !query.raw,
            )
            .await?
        {
            PathDestination::Tree(items) => into_response(&TreeView { repo, items, query }),
            PathDestination::File(file) if query.raw => {
                let headers = [(
                    http::header::CONTENT_TYPE,
                    http::HeaderValue::from_static("text/plain"),
                )];

                (headers, file.content).into_response()
            }
            PathDestination::File(file) => into_response(&FileView { repo, file }),
        },
    )
}
