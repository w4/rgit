use std::fmt::{Display, Formatter};
use std::{
    ops::Deref,
    path::{Path, PathBuf},
    sync::Arc,
};

use askama::Template;
use axum::{
    extract::Query,
    handler::Handler,
    http::Request,
    response::{IntoResponse, Response},
    Extension,
};
use path_clean::PathClean;
use serde::Deserialize;
use tower::{util::BoxCloneService, Service};

use super::filters;
use crate::git::{DetailedTag, FileWithContent, PathDestination, Refs, TreeItem};
use crate::{git::Commit, into_response, layers::UnwrapInfallible, Git};

#[derive(Clone)]
pub struct Repository(pub PathBuf);

impl Deref for Repository {
    type Target = Path;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[derive(Clone)]
pub struct RepositoryPath(pub PathBuf);

#[derive(Clone)]
pub struct ChildPath(pub Option<PathBuf>);

impl Deref for RepositoryPath {
    type Target = Path;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

// this is some wicked, wicked abuse of axum right here...
pub async fn service<ReqBody: Send + 'static>(mut request: Request<ReqBody>) -> Response {
    let mut uri_parts: Vec<&str> = request
        .uri()
        .path()
        .trim_start_matches('/')
        .trim_end_matches('/')
        .split('/')
        .collect();

    let mut child_path = None;

    let mut service = match uri_parts.pop() {
        Some("about") => BoxCloneService::new(handle_about.into_service()),
        Some("refs") => BoxCloneService::new(handle_refs.into_service()),
        Some("log") => BoxCloneService::new(handle_log.into_service()),
        Some("tree") => BoxCloneService::new(handle_tree.into_service()),
        Some("commit") => BoxCloneService::new(handle_commit.into_service()),
        Some("diff") => BoxCloneService::new(handle_diff.into_service()),
        Some("tag") => BoxCloneService::new(handle_tag.into_service()),
        Some(v) => {
            uri_parts.push(v);

            // match tree children
            if uri_parts.iter().any(|v| *v == "tree") {
                // TODO: this needs fixing up so it doesn't accidentally match repos that have
                //  `tree` in their path
                let mut reconstructed_path = Vec::new();

                while let Some(part) = uri_parts.pop() {
                    if part == "tree" {
                        break;
                    }

                    // TODO: FIXME
                    reconstructed_path.insert(0, part);
                }

                child_path = Some(reconstructed_path.into_iter().collect::<PathBuf>().clean());

                eprintln!("repo path: {:?}", child_path);

                BoxCloneService::new(handle_tree.into_service())
            } else {
                BoxCloneService::new(handle_summary.into_service())
            }
        }
        None => panic!("not found"),
    };

    let uri = uri_parts.into_iter().collect::<PathBuf>().clean();
    let path = Path::new("../test-git").canonicalize().unwrap().join(&uri);

    request.extensions_mut().insert(ChildPath(child_path));
    request.extensions_mut().insert(Repository(uri));
    request.extensions_mut().insert(RepositoryPath(path));

    service
        .call(request)
        .await
        .unwrap_infallible()
        .into_response()
}

#[derive(Template)]
#[template(path = "repo/summary.html")]
pub struct SummaryView {
    pub repo: Repository,
}

#[allow(clippy::unused_async)]
pub async fn handle_summary(Extension(repo): Extension<Repository>) -> Response {
    into_response(&SummaryView { repo })
}

#[derive(Deserialize)]
pub struct TagQuery {
    #[serde(rename = "h")]
    name: String,
}

#[derive(Template)]
#[template(path = "repo/tag.html")]
pub struct TagView {
    repo: Repository,
    tag: DetailedTag,
}

pub async fn handle_tag(
    Extension(repo): Extension<Repository>,
    Extension(RepositoryPath(repository_path)): Extension<RepositoryPath>,
    Extension(git): Extension<Arc<Git>>,
    Query(query): Query<TagQuery>,
) -> Response {
    let open_repo = git.repo(repository_path).await;
    let tag = open_repo.tag_info(&query.name).await;

    into_response(&TagView { repo, tag })
}

#[derive(Deserialize)]
pub struct LogQuery {
    #[serde(rename = "ofs")]
    offset: Option<usize>,
    #[serde(rename = "h")]
    branch: Option<String>,
}

#[derive(Template)]
#[template(path = "repo/log.html")]
pub struct LogView {
    repo: Repository,
    commits: Vec<crate::database::schema::commit::Commit>,
    next_offset: Option<usize>,
    branch: Option<String>,
}

pub async fn handle_log(
    Extension(repo): Extension<Repository>,
    Extension(db): Extension<sled::Db>,
    Query(query): Query<LogQuery>,
) -> Response {
    let offset = query.offset.unwrap_or(0);

    let reference = format!("refs/heads/{}", query.branch.as_deref().unwrap_or("master"));
    let repository = crate::database::schema::repository::Repository::open(&db, &*repo).unwrap();
    let commit_tree = repository.commit_tree(&db, &reference);
    let mut commits = commit_tree.fetch_latest(101, offset);

    let next_offset = if commits.len() == 101 {
        commits.pop();
        Some(offset + 100)
    } else {
        None
    };

    into_response(&LogView {
        repo,
        commits,
        next_offset,
        branch: query.branch,
    })
}

#[derive(Template)]
#[template(path = "repo/refs.html")]
pub struct RefsView {
    repo: Repository,
    refs: Arc<Refs>,
}

pub async fn handle_refs(
    Extension(repo): Extension<Repository>,
    Extension(RepositoryPath(repository_path)): Extension<RepositoryPath>,
    Extension(git): Extension<Arc<Git>>,
) -> Response {
    let open_repo = git.repo(repository_path).await;
    let refs = open_repo.refs().await;

    into_response(&RefsView { repo, refs })
}

#[derive(Template)]
#[template(path = "repo/about.html")]
pub struct AboutView {
    repo: Repository,
    readme: Option<Arc<str>>,
}

pub async fn handle_about(
    Extension(repo): Extension<Repository>,
    Extension(RepositoryPath(repository_path)): Extension<RepositoryPath>,
    Extension(git): Extension<Arc<Git>>,
) -> Response {
    let open_repo = git.clone().repo(repository_path).await;
    let readme = open_repo.readme().await;

    into_response(&AboutView { repo, readme })
}

#[derive(Template)]
#[template(path = "repo/commit.html")]
pub struct CommitView {
    pub repo: Repository,
    pub commit: Arc<Commit>,
}

#[derive(Deserialize)]
pub struct CommitQuery {
    id: Option<String>,
}

pub async fn handle_commit(
    Extension(repo): Extension<Repository>,
    Extension(RepositoryPath(repository_path)): Extension<RepositoryPath>,
    Extension(git): Extension<Arc<Git>>,
    Query(query): Query<CommitQuery>,
) -> Response {
    let open_repo = git.repo(repository_path).await;
    let commit = if let Some(commit) = query.id {
        open_repo.commit(&commit).await
    } else {
        Arc::new(open_repo.latest_commit().await)
    };

    into_response(&CommitView { repo, commit })
}

#[derive(Deserialize)]
pub struct TreeQuery {
    id: Option<String>,
    #[serde(rename = "h")]
    branch: Option<String>,
}

impl Display for TreeQuery {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let mut prefix = "?";

        if let Some(id) = self.id.as_deref() {
            write!(f, "{}id={}", prefix, id)?;
            prefix = "&";
        }

        if let Some(branch) = self.branch.as_deref() {
            write!(f, "{}h={}", prefix, branch)?;
        }

        Ok(())
    }
}

pub async fn handle_tree(
    Extension(repo): Extension<Repository>,
    Extension(RepositoryPath(repository_path)): Extension<RepositoryPath>,
    Extension(ChildPath(child_path)): Extension<ChildPath>,
    Extension(git): Extension<Arc<Git>>,
    Query(query): Query<TreeQuery>,
) -> Response {
    #[derive(Template)]
    #[template(path = "repo/tree.html")]
    pub struct TreeView {
        pub repo: Repository,
        pub items: Vec<TreeItem>,
        pub query: TreeQuery,
    }

    #[derive(Template)]
    #[template(path = "repo/file.html")]
    pub struct FileView {
        pub repo: Repository,
        pub file: FileWithContent,
    }

    let open_repo = git.repo(repository_path).await;

    match open_repo
        .path(child_path, query.id.as_deref(), query.branch.clone())
        .await
    {
        PathDestination::Tree(items) => into_response(&TreeView { repo, items, query }),
        PathDestination::File(file) => into_response(&FileView { repo, file }),
    }
}

#[derive(Template)]
#[template(path = "repo/diff.html")]
pub struct DiffView {
    pub repo: Repository,
    pub commit: Arc<Commit>,
}

pub async fn handle_diff(
    Extension(repo): Extension<Repository>,
    Extension(RepositoryPath(repository_path)): Extension<RepositoryPath>,
    Extension(git): Extension<Arc<Git>>,
    Query(query): Query<CommitQuery>,
) -> Response {
    let open_repo = git.repo(repository_path).await;
    let commit = if let Some(commit) = query.id {
        open_repo.commit(&commit).await
    } else {
        Arc::new(open_repo.latest_commit().await)
    };

    into_response(&DiffView { repo, commit })
}
