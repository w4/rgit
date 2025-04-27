use anyhow::{bail, Context};
use askama::Template;
use axum::{extract::Query, response::IntoResponse, Extension};
use gix::ObjectId;
use itertools::Itertools;
use serde::Deserialize;
use std::path::PathBuf;
use std::{
    fmt::{Display, Formatter},
    sync::Arc,
};

use crate::database::schema::tree::{
    ArchivedTreeItemKind, Tree, TreeItem, YokedTreeItem, YokedTreeItemKeyUtf8,
};
use crate::{
    git::FileWithContent,
    into_response,
    methods::{
        filters,
        repo::{ChildPath, Repository, RepositoryPath, Result},
    },
    Git, ResponseEither,
};

use super::log::get_branch_commits;

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
    pub items: Vec<(YokedTreeItemKeyUtf8, usize, YokedTreeItem)>,
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

enum LookupResult {
    RealPath,
    Children(Vec<(YokedTreeItemKeyUtf8, usize, YokedTreeItem)>),
}

pub async fn handle(
    Extension(repo): Extension<Repository>,
    Extension(RepositoryPath(repository_path)): Extension<RepositoryPath>,
    Extension(ChildPath(child_path)): Extension<ChildPath>,
    Extension(git): Extension<Arc<Git>>,
    Extension(db): Extension<Arc<rocksdb::DB>>,
    Query(query): Query<UriQuery>,
) -> Result<impl IntoResponse> {
    // TODO: bit messy
    let (repo, query, child_path, lookup_result) = tokio::task::spawn_blocking(move || {
        let tree_id = if let Some(id) = query.id.as_deref() {
            let hex = const_hex::decode_to_array(id).context("Failed to parse tree hash")?;
            Tree::find(&db, ObjectId::Sha1(hex))
                .context("Failed to lookup tree")?
                .context("Couldn't find tree with given id")?
        } else {
            let repository = crate::database::schema::repository::Repository::open(&db, &*repo)?
                .context("Repository does not exist")?;
            let commit = get_branch_commits(&repository, &db, query.branch.as_deref(), 1, 0)?
                .into_iter()
                .next()
                .context("Branch not found")?;
            commit.get().tree.to_native()
        };

        if let Some(path) = &child_path {
            if let Some(item) =
                TreeItem::find_exact(&db, tree_id, path.as_os_str().as_encoded_bytes())?
            {
                if let ArchivedTreeItemKind::File = item.get().kind {
                    return Ok((repo, query, child_path, LookupResult::RealPath));
                }
            }
        }

        let path = child_path
            .as_ref()
            .map(|v| v.as_os_str().as_encoded_bytes())
            .unwrap_or_default();

        let tree_items = TreeItem::find_prefix(&db, tree_id, path)
            // don't take the current path the user is on
            .filter_ok(|(k, _)| !k.get()[path.len()..].is_empty())
            // only take direct descendents
            .filter_ok(|(k, _)| {
                memchr::memrchr(b'/', &k.get()[path.len()..]).is_none_or(|v| v == 0)
            })
            .map_ok(|(k, v)| {
                (
                    k.try_map_project(|v, _| simdutf8::basic::from_utf8(v))
                        .expect("invalid utf8"),
                    path.len(),
                    v,
                )
            })
            .try_collect::<_, Vec<_>, _>()?;

        if tree_items.is_empty() {
            bail!("Path doesn't exist in tree");
        }

        Ok::<_, anyhow::Error>((repo, query, child_path, LookupResult::Children(tree_items)))
    })
    .await
    .context("Failed to join on task")??;

    Ok(match lookup_result {
        LookupResult::RealPath => {
            let open_repo = git.repo(repository_path, query.branch.clone()).await?;
            let file = open_repo
                .path(child_path.clone(), query.id.as_deref(), !query.raw)
                .await?;

            if query.raw {
                ResponseEither::Right(file.content)
            } else {
                ResponseEither::Left(ResponseEither::Right(into_response(FileView {
                    repo,
                    file,
                    branch: query.branch,
                    repo_path: child_path.unwrap_or_default(),
                })))
            }
        }
        LookupResult::Children(items) => {
            ResponseEither::Left(ResponseEither::Left(into_response(TreeView {
                repo,
                items,
                branch: query.branch.clone(),
                query,
                repo_path: child_path.unwrap_or_default(),
            })))
        }
    })
}
