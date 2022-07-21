use std::{
    fmt::{Debug, Display, Formatter},
    io::Write,
    ops::Deref,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
};

use askama::Template;
use axum::{
    body::HttpBody,
    extract::Query,
    handler::Handler,
    http,
    http::HeaderValue,
    http::Request,
    response::{IntoResponse, Response},
    Extension,
};
use bytes::Bytes;
use path_clean::PathClean;
use serde::Deserialize;
use tower::{util::BoxCloneService, Service};
use yoke::Yoke;

use super::filters;
use crate::git::{DetailedTag, FileWithContent, PathDestination, ReadmeFormat, Refs, TreeItem};
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
pub async fn service<ReqBody: HttpBody + Send + Debug + 'static>(
    mut request: Request<ReqBody>,
) -> Response
where
    <ReqBody as HttpBody>::Data: Send + Sync,
    <ReqBody as HttpBody>::Error: std::error::Error + Send + Sync,
{
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
        // TODO: https://man.archlinux.org/man/git-http-backend.1.en
        // TODO: GIT_PROTOCOL
        Some("refs") if uri_parts.last() == Some(&"info") => {
            uri_parts.pop();
            BoxCloneService::new(handle_info_refs.into_service())
        }
        Some("git-upload-pack") => BoxCloneService::new(handle_git_upload_pack.into_service()),
        Some("refs") => BoxCloneService::new(handle_refs.into_service()),
        Some("log") => BoxCloneService::new(handle_log.into_service()),
        Some("tree") => BoxCloneService::new(handle_tree.into_service()),
        Some("commit") => BoxCloneService::new(handle_commit.into_service()),
        Some("diff") => BoxCloneService::new(handle_diff.into_service()),
        Some("patch") => BoxCloneService::new(handle_patch.into_service()),
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
pub struct SummaryView<'a> {
    repo: Repository,
    refs: Arc<Refs>,
    commit_list: Vec<&'a crate::database::schema::commit::Commit<'a>>,
}

pub async fn handle_summary(
    Extension(repo): Extension<Repository>,
    Extension(RepositoryPath(repository_path)): Extension<RepositoryPath>,
    Extension(git): Extension<Arc<Git>>,
    Extension(db): Extension<sled::Db>,
) -> Response {
    let open_repo = git.repo(repository_path).await;
    let refs = open_repo.refs().await;

    let repository = crate::database::schema::repository::Repository::open(&db, &*repo).unwrap();
    let commit_tree = repository.get().commit_tree(&db, "refs/heads/master");
    let commits = commit_tree.fetch_latest(11, 0).await;
    let commit_list = commits.iter().map(Yoke::get).collect();

    into_response(&SummaryView {
        repo,
        refs,
        commit_list,
    })
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
pub struct LogView<'a> {
    repo: Repository,
    commits: Vec<&'a crate::database::schema::commit::Commit<'a>>,
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
    let commit_tree = repository.get().commit_tree(&db, &reference);
    let mut commits = commit_tree.fetch_latest(101, offset).await;

    let next_offset = if commits.len() == 101 {
        commits.pop();
        Some(offset + 100)
    } else {
        None
    };

    let commits = commits.iter().map(Yoke::get).collect();

    into_response(&LogView {
        repo,
        commits,
        next_offset,
        branch: query.branch,
    })
}

#[derive(Deserialize)]
pub struct SmartGitQuery {
    service: String,
}

pub async fn handle_info_refs(
    Extension(RepositoryPath(repository_path)): Extension<RepositoryPath>,
    Query(query): Query<SmartGitQuery>,
) -> Response {
    // todo: tokio command
    let out = std::process::Command::new("git")
        .arg("http-backend")
        .env("REQUEST_METHOD", "GET")
        .env("PATH_INFO", "/info/refs")
        .env("GIT_PROJECT_ROOT", repository_path)
        .env("QUERY_STRING", format!("service={}", query.service))
        .output()
        .unwrap();

    crate::git_cgi::cgi_to_response(&out.stdout)
}

pub async fn handle_git_upload_pack(
    Extension(RepositoryPath(repository_path)): Extension<RepositoryPath>,
    body: Bytes,
) -> Response {
    // todo: tokio command
    let mut child = std::process::Command::new("git")
        .arg("http-backend")
        // todo: read all this from request
        .env("REQUEST_METHOD", "POST")
        .env("CONTENT_TYPE", "application/x-git-upload-pack-request")
        .env("PATH_INFO", "/git-upload-pack")
        .env("GIT_PROJECT_ROOT", repository_path)
        .stdout(Stdio::piped())
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.as_mut().unwrap().write_all(&body).unwrap();
    let out = child.wait_with_output().unwrap();

    crate::git_cgi::cgi_to_response(&out.stdout)
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
    readme: Option<(ReadmeFormat, Arc<str>)>,
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

pub async fn handle_patch(
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

    let headers = [(
        http::header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain"),
    )];

    (headers, commit.diff_plain.clone()).into_response()
}
