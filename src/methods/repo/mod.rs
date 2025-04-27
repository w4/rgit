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
    sync::{Arc, LazyLock},
};

use axum::{
    body::Body,
    handler::Handler,
    http::{Request, StatusCode},
    response::{IntoResponse, Response},
};
use path_clean::PathClean;

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
use crate::database::schema::{commit::YokedCommit, tag::YokedTag};

pub const DEFAULT_BRANCHES: [&str; 2] = ["refs/heads/master", "refs/heads/main"];

// this is some wicked, wicked abuse of axum right here...
#[allow(clippy::trait_duplication_in_bounds)] // clippy seems a bit.. lost
pub async fn service(mut request: Request<Body>) -> Response {
    let scan_path = request
        .extensions()
        .get::<Arc<PathBuf>>()
        .expect("scan_path missing");

    let ParsedUri {
        uri,
        child_path,
        action,
    } = parse_uri(request.uri().path().trim_matches('/'));

    let uri = Path::new(uri).clean();
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

    match action {
        HandlerAction::About => handle_about.call(request, None::<()>).await,
        HandlerAction::SmartGit => handle_smart_git.call(request, None::<()>).await,
        HandlerAction::Refs => handle_refs.call(request, None::<()>).await,
        HandlerAction::Log => handle_log.call(request, None::<()>).await,
        HandlerAction::Tree => handle_tree.call(request, None::<()>).await,
        HandlerAction::Commit => handle_commit.call(request, None::<()>).await,
        HandlerAction::Diff => handle_diff.call(request, None::<()>).await,
        HandlerAction::Patch => handle_patch.call(request, None::<()>).await,
        HandlerAction::Tag => handle_tag.call(request, None::<()>).await,
        HandlerAction::Snapshot => handle_snapshot.call(request, None::<()>).await,
        HandlerAction::Summary => handle_summary.call(request, None::<()>).await,
    }
}

#[derive(Debug, PartialEq, Eq)]
struct ParsedUri<'a> {
    action: HandlerAction,
    uri: &'a str,
    child_path: Option<PathBuf>,
}

fn parse_uri(uri: &str) -> ParsedUri<'_> {
    let mut uri_parts = memchr::memchr_iter(b'/', uri.as_bytes());

    let original_uri = uri;
    let (action, mut uri) = if let Some(idx) = uri_parts.next_back() {
        (uri.get(idx + 1..), &uri[..idx])
    } else {
        (None, uri)
    };

    match action {
        Some("about") => ParsedUri {
            action: HandlerAction::About,
            uri,
            child_path: None,
        },
        Some("git-upload-pack") => ParsedUri {
            action: HandlerAction::SmartGit,
            uri,
            child_path: None,
        },
        Some("refs") => {
            if let Some(idx) = uri_parts.next_back() {
                if uri.get(idx + 1..) == Some("info") {
                    ParsedUri {
                        action: HandlerAction::SmartGit,
                        uri: &uri[..idx],
                        child_path: None,
                    }
                } else {
                    ParsedUri {
                        action: HandlerAction::Refs,
                        uri,
                        child_path: None,
                    }
                }
            } else {
                ParsedUri {
                    action: HandlerAction::Refs,
                    uri,
                    child_path: None,
                }
            }
        }
        Some("log") => ParsedUri {
            action: HandlerAction::Log,
            uri,
            child_path: None,
        },
        Some("tree") => ParsedUri {
            action: HandlerAction::Tree,
            uri,
            child_path: None,
        },
        Some("commit") => ParsedUri {
            action: HandlerAction::Commit,
            uri,
            child_path: None,
        },
        Some("diff") => ParsedUri {
            action: HandlerAction::Diff,
            uri,
            child_path: None,
        },
        Some("patch") => ParsedUri {
            action: HandlerAction::Patch,
            uri,
            child_path: None,
        },
        Some("tag") => ParsedUri {
            action: HandlerAction::Tag,
            uri,
            child_path: None,
        },
        Some("snapshot") => ParsedUri {
            action: HandlerAction::Snapshot,
            uri,
            child_path: None,
        },
        Some(_) => {
            static TREE_FINDER: LazyLock<memchr::memmem::Finder> =
                LazyLock::new(|| memchr::memmem::Finder::new(b"/tree/"));

            uri = original_uri;

            // match tree children
            if let Some(idx) = TREE_FINDER.find(uri.as_bytes()) {
                ParsedUri {
                    action: HandlerAction::Tree,
                    uri: &uri[..idx],
                    // 6 is the length of /tree/
                    child_path: Some(Path::new(&uri[idx + 6..]).clean()),
                }
            } else {
                ParsedUri {
                    action: HandlerAction::Summary,
                    uri,
                    child_path: None,
                }
            }
        }
        None => ParsedUri {
            action: HandlerAction::Summary,
            uri,
            child_path: None,
        },
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum HandlerAction {
    About,
    SmartGit,
    Refs,
    Log,
    Tree,
    Commit,
    Diff,
    Patch,
    Tag,
    Snapshot,
    Summary,
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

pub struct InvalidRequest;

impl IntoResponse for InvalidRequest {
    fn into_response(self) -> Response {
        (StatusCode::NOT_FOUND, "Invalid request").into_response()
    }
}

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

impl From<Error> for anyhow::Error {
    fn from(value: Error) -> Self {
        value.0
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
