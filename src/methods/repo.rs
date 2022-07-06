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
    response::{Html, IntoResponse, Response},
    Extension,
};
use path_clean::PathClean;
use serde::Deserialize;
use tower::{util::BoxCloneService, Service};

use crate::git::{DetailedTag, Refs};
use crate::{git::Commit, layers::UnwrapInfallible, Git};

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

impl Deref for RepositoryPath {
    type Target = Path;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

pub async fn service<ReqBody: Send + 'static>(mut request: Request<ReqBody>) -> Response {
    let mut uri_parts: Vec<&str> = request
        .uri()
        .path()
        .trim_start_matches('/')
        .trim_end_matches('/')
        .split('/')
        .collect();

    let mut service = match uri_parts.pop() {
        Some("about") => BoxCloneService::new(handle_about.into_service()),
        Some("refs") => BoxCloneService::new(handle_refs.into_service()),
        Some("log") => BoxCloneService::new(handle_log.into_service()),
        Some("tree") => BoxCloneService::new(handle_tree.into_service()),
        Some("commit") => BoxCloneService::new(handle_commit.into_service()),
        Some("diff") => BoxCloneService::new(handle_diff.into_service()),
        Some("stats") => BoxCloneService::new(handle_stats.into_service()),
        Some("tag") => BoxCloneService::new(handle_tag.into_service()),
        Some(v) => {
            uri_parts.push(v);
            BoxCloneService::new(handle_summary.into_service())
        }
        None => panic!("not found"),
    };

    let uri = uri_parts.into_iter().collect::<PathBuf>().clean();
    let path = Path::new("../test-git").canonicalize().unwrap().join(&uri);

    request.extensions_mut().insert(Repository(uri));
    request.extensions_mut().insert(RepositoryPath(path));

    service
        .call(request)
        .await
        .unwrap_infallible()
        .into_response()
}

#[allow(clippy::unused_async)]
pub async fn handle_summary(Extension(repo): Extension<Repository>) -> Html<String> {
    #[derive(Template)]
    #[template(path = "repo/summary.html")]
    pub struct View {
        repo: Repository,
    }

    Html(View { repo }.render().unwrap())
}

#[derive(Deserialize)]
pub struct TagQuery {
    #[serde(rename = "h")]
    name: String,
}

pub async fn handle_tag(
    Extension(repo): Extension<Repository>,
    Extension(RepositoryPath(repository_path)): Extension<RepositoryPath>,
    Extension(git): Extension<Git>,
    Query(query): Query<TagQuery>,
) -> Html<String> {
    #[derive(Template)]
    #[template(path = "repo/tag.html")]
    pub struct View {
        repo: Repository,
        tag: DetailedTag,
    }

    let tag = git.get_tag(repository_path, &query.name).await;

    Html(View { repo, tag }.render().unwrap())
}

#[derive(Deserialize)]
pub struct LogQuery {
    #[serde(rename = "ofs")]
    offset: Option<usize>,
    #[serde(rename = "h")]
    branch: Option<String>,
}

#[allow(clippy::unused_async)]
pub async fn handle_log(
    Extension(repo): Extension<Repository>,
    Extension(RepositoryPath(repository_path)): Extension<RepositoryPath>,
    Extension(git): Extension<Git>,
    Query(query): Query<LogQuery>,
) -> Html<String> {
    #[derive(Template)]
    #[template(path = "repo/log.html")]
    pub struct View {
        repo: Repository,
        commits: Vec<Commit>,
        next_offset: Option<usize>,
        branch: Option<String>,
    }

    let (commits, next_offset) = git
        .get_commits(
            repository_path,
            query.branch.as_deref(),
            query.offset.unwrap_or(0),
        )
        .await;

    Html(
        View {
            repo,
            commits,
            next_offset,
            branch: query.branch,
        }
        .render()
        .unwrap(),
    )
}

#[allow(clippy::unused_async)]
pub async fn handle_refs(
    Extension(repo): Extension<Repository>,
    Extension(RepositoryPath(repository_path)): Extension<RepositoryPath>,
    Extension(git): Extension<Git>,
) -> Html<String> {
    #[derive(Template)]
    #[template(path = "repo/refs.html")]
    pub struct View {
        repo: Repository,
        refs: Arc<Refs>,
    }

    let refs = git.get_refs(repository_path).await;

    Html(View { repo, refs }.render().unwrap())
}

#[allow(clippy::unused_async)]
pub async fn handle_about(
    Extension(repo): Extension<Repository>,
    Extension(RepositoryPath(repository_path)): Extension<RepositoryPath>,
    Extension(git): Extension<Git>,
) -> Html<String> {
    #[derive(Template)]
    #[template(path = "repo/about.html")]
    pub struct View {
        repo: Repository,
        readme: Option<Arc<str>>,
    }

    let readme = git.get_readme(repository_path).await;

    Html(View { repo, readme }.render().unwrap())
}

#[derive(Deserialize)]
pub struct CommitQuery {
    id: Option<String>,
}

#[allow(clippy::unused_async)]
pub async fn handle_commit(
    Extension(repo): Extension<Repository>,
    Extension(RepositoryPath(repository_path)): Extension<RepositoryPath>,
    Extension(git): Extension<Git>,
    Query(query): Query<CommitQuery>,
) -> Html<String> {
    #[derive(Template)]
    #[template(path = "repo/commit.html")]
    pub struct View {
        pub repo: Repository,
        pub commit: Arc<Commit>,
    }

    Html(
        View {
            repo,
            commit: if let Some(commit) = query.id {
                git.get_commit(repository_path, &commit).await
            } else {
                Arc::new(git.get_latest_commit(repository_path).await)
            },
        }
        .render()
        .unwrap(),
    )
}

#[allow(clippy::unused_async)]
pub async fn handle_tree(Extension(repo): Extension<Repository>) -> Html<String> {
    #[derive(Template)]
    #[template(path = "repo/tree.html")]
    pub struct View {
        pub repo: Repository,
    }

    Html(View { repo }.render().unwrap())
}

#[allow(clippy::unused_async)]
pub async fn handle_diff(Extension(repo): Extension<Repository>) -> Html<String> {
    #[derive(Template)]
    #[template(path = "repo/diff.html")]
    pub struct View {
        pub repo: Repository,
    }

    Html(View { repo }.render().unwrap())
}

#[allow(clippy::unused_async)]
pub async fn handle_stats(Extension(repo): Extension<Repository>) -> Html<String> {
    #[derive(Template)]
    #[template(path = "repo/stats.html")]
    pub struct View {
        pub repo: Repository,
    }

    Html(View { repo }.render().unwrap())
}
