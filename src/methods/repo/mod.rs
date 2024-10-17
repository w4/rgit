mod about;
mod commit;
mod diff;
mod log;
mod refs;
mod smart_git;
mod snapshot;
mod summary;
mod tag;
mod tree;

use std::{
    collections::BTreeMap,
    ops::Deref,
    path::{Path, PathBuf},
    sync::Arc,
};

use axum::{
    body::Body,
    handler::HandlerWithoutStateExt,
    http::{Request, StatusCode},
    response::{IntoResponse, Response},
};
use path_clean::PathClean;
use tower::{util::BoxCloneService, Service};

use self::{
    about::handle as handle_about,
    commit::handle as handle_commit,
    diff::{handle as handle_diff, handle_plain as handle_patch},
    log::handle as handle_log,
    refs::handle as handle_refs,
    smart_git::handle as handle_smart_git,
    snapshot::handle as handle_snapshot,
    summary::handle as handle_summary,
    tag::handle as handle_tag,
    tree::handle as handle_tree,
};
use crate::database::schema::tag::YokedString;
use crate::{
    database::schema::{commit::YokedCommit, tag::YokedTag},
    layers::UnwrapInfallible,
};

pub const DEFAULT_BRANCHES: [&str; 2] = ["refs/heads/master", "refs/heads/main"];

// this is some wicked, wicked abuse of axum right here...
#[allow(clippy::trait_duplication_in_bounds)] // clippy seems a bit.. lost
pub async fn service(mut request: Request<Body>) -> Response {
    let scan_path = request
        .extensions()
        .get::<Arc<PathBuf>>()
        .expect("scan_path missing");

    let mut uri_parts: Vec<&str> = request
        .uri()
        .path()
        .trim_start_matches('/')
        .trim_end_matches('/')
        .split('/')
        .collect();

    let mut child_path = None;

    macro_rules! h {
        ($handler:ident) => {
            BoxCloneService::new($handler.into_service())
        };
    }

    let mut service = match uri_parts.pop() {
        Some("about") => h!(handle_about),
        Some("refs") if uri_parts.last() == Some(&"info") => {
            uri_parts.pop();
            h!(handle_smart_git)
        }
        Some("git-upload-pack") => h!(handle_smart_git),
        Some("refs") => h!(handle_refs),
        Some("log") => h!(handle_log),
        Some("tree") => h!(handle_tree),
        Some("commit") => h!(handle_commit),
        Some("diff") => h!(handle_diff),
        Some("patch") => h!(handle_patch),
        Some("tag") => h!(handle_tag),
        Some("snapshot") => h!(handle_snapshot),
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

                h!(handle_tree)
            } else {
                h!(handle_summary)
            }
        }
        None => panic!("not found"),
    };

    let uri = uri_parts.into_iter().collect::<PathBuf>().clean();
    let path = scan_path.join(&uri);

    let db = request
        .extensions()
        .get::<Arc<rocksdb::DB>>()
        .expect("db extension missing");
    if path.as_os_str().is_empty()
        || !crate::database::schema::repository::Repository::exists(db, &uri).unwrap_or_default()
    {
        return RepositoryNotFound.into_response();
    }

    request.extensions_mut().insert(ChildPath(child_path));
    request.extensions_mut().insert(Repository(uri));
    request.extensions_mut().insert(RepositoryPath(path));

    service
        .call(request)
        .await
        .unwrap_infallible()
        .into_response()
}

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

pub type Result<T, E = Error> = std::result::Result<T, E>;

pub struct RepositoryNotFound;

impl IntoResponse for RepositoryNotFound {
    fn into_response(self) -> Response {
        (StatusCode::NOT_FOUND, "Repository not found").into_response()
    }
}

pub struct Error(anyhow::Error);

impl From<Arc<anyhow::Error>> for Error {
    fn from(e: Arc<anyhow::Error>) -> Self {
        Self(anyhow::Error::msg(format!("{e:?}")))
    }
}

impl From<anyhow::Error> for Error {
    fn from(e: anyhow::Error) -> Self {
        Self(e)
    }
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("{:?}", self.0)).into_response()
    }
}

pub struct Refs {
    heads: BTreeMap<String, YokedCommit>,
    tags: Vec<(YokedString, YokedTag)>,
}
