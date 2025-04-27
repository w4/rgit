use std::{
    collections::{BTreeMap, HashSet},
    ffi::OsStr,
    fmt::Debug,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::Context;
use gix::{
    bstr::{BStr, ByteSlice},
    objs::tree::EntryKind,
    refs::Category,
    url::Scheme,
    ObjectId, Reference, Url,
};
use itertools::{Either, Itertools};
use rocksdb::WriteBatch;
use time::{OffsetDateTime, UtcOffset};
use tracing::{error, info, info_span, instrument, warn};
use xxhash_rust::xxh3::Xxh3;

use crate::{
    database::schema::{
        commit::Commit,
        repository::{ArchivedRepository, Repository, RepositoryId},
        tag::{Tag, TagTree},
    },
    git::{PathVisitor, PathVisitorHandler},
};

use super::schema::tree::{SortedTree, SortedTreeItem, Tree, TreeItem, TreeItemKind, TreeKey};

pub fn run(scan_path: &Path, repository_list: Option<&Path>, db: &Arc<rocksdb::DB>) {
    let span = info_span!("index_update");
    let _entered = span.enter();

    info!("Starting index update");

    update_repository_metadata(scan_path, repository_list, db);
    update_repository_reflog(scan_path, db.clone());
    update_repository_tags(scan_path, db.clone());

    info!("Flushing to disk");

    if let Err(error) = db.flush() {
        error!(%error, "Failed to flush database to disk");
    }

    info!("Finished index update");
}

#[instrument(skip(db))]
fn update_repository_metadata(scan_path: &Path, repository_list: Option<&Path>, db: &rocksdb::DB) {
    let mut discovered = Vec::new();
    discover_repositories(scan_path, repository_list, &mut discovered);

    for (repository_path, git_repository) in discovered {
        let Some(relative) = get_relative_path(scan_path, &repository_path) else {
            continue;
        };

        let id = match Repository::open(db, relative) {
            Ok(v) => v.map_or_else(RepositoryId::new, |v| {
                RepositoryId(v.get().id.0.to_native())
            }),
            Err(error) => {
                // maybe we could nuke it ourselves, but we need to instantly trigger
                // a reindex and we could enter into an infinite loop if there's a bug
                // or something
                error!(%error, "Failed to open repository index {}, please consider nuking database", relative.display());
                continue;
            }
        };

        let Some(name) = relative.file_name().and_then(OsStr::to_str) else {
            continue;
        };
        let description = std::fs::read(repository_path.join("description")).unwrap_or_default();
        let description = String::from_utf8(description)
            .ok()
            .filter(|v| !v.is_empty());

        let owner = git_repository
            .config_snapshot()
            .string("gitweb.owner")
            .map(|v| v.to_string());

        let res = Repository {
            id,
            name: name.to_string(),
            description,
            owner,
            last_modified: {
                let r =
                    find_last_committed_time(&git_repository).unwrap_or(OffsetDateTime::UNIX_EPOCH);
                (r.unix_timestamp(), r.offset().whole_seconds())
            },
            default_branch: find_default_branch(&git_repository).ok().flatten(),
            exported: repository_path.join("git-daemon-export-ok").exists(),
        }
        .insert(db, relative);

        if let Err(error) = res {
            warn!(%error, "Failed to insert repository");
        }
    }
}

fn find_default_branch(repo: &gix::Repository) -> Result<Option<String>, anyhow::Error> {
    if repo.head()?.is_detached() {
        Ok(None)
    } else {
        Ok(Some(
            repo.head()?
                .referent_name()
                .context("HEAD does not point to anything")?
                .as_bstr()
                .to_string(),
        ))
    }
}

fn find_last_committed_time(repo: &gix::Repository) -> Result<OffsetDateTime, anyhow::Error> {
    let mut timestamp = OffsetDateTime::UNIX_EPOCH;

    for reference in repo.references()?.all()? {
        let Ok(commit) = reference.unwrap().peel_to_commit() else {
            continue;
        };

        let committer = commit.committer()?;
        let offset = UtcOffset::from_whole_seconds(committer.time.offset).unwrap_or(UtcOffset::UTC);
        let committed_time = OffsetDateTime::from_unix_timestamp(committer.time.seconds)
            .unwrap_or(OffsetDateTime::UNIX_EPOCH)
            .to_offset(offset);
        timestamp = timestamp.max(committed_time);
    }

    Ok(timestamp)
}

#[instrument(skip(db))]
fn update_repository_reflog(scan_path: &Path, db: Arc<rocksdb::DB>) {
    let repos = match Repository::fetch_all(&db) {
        Ok(v) => v,
        Err(error) => {
            error!(%error, "Failed to read repository index to update reflog, consider deleting database directory");
            return;
        }
    };

    for (relative_path, db_repository) in repos {
        let Some(git_repository) = open_repo(scan_path, &relative_path, db_repository.get(), &db)
        else {
            continue;
        };

        let references = match git_repository.references() {
            Ok(v) => v,
            Err(error) => {
                error!(%error, "Failed to read references for {relative_path}");
                continue;
            }
        };

        let references = match references.all() {
            Ok(v) => v,
            Err(error) => {
                error!(%error, "Failed to read references for {relative_path}");
                continue;
            }
        };

        let submodules = match git_repository.submodules() {
            Ok(submodules) => submodules
                .into_iter()
                .flatten()
                .filter_map(|v| Some((v.name().to_path_lossy().to_path_buf(), v.url().ok()?)))
                .collect::<BTreeMap<_, _>>(),
            Err(error) => {
                error!(%error, "Failed to read submodules for {relative_path}");
                continue;
            }
        };

        let mut valid_references = Vec::new();

        for reference in references {
            let mut reference = match reference {
                Ok(v) => v,
                Err(error) => {
                    error!(%error, "Failed to read reference for {relative_path}");
                    continue;
                }
            };

            let reference_name = reference.name();
            if !matches!(
                reference_name.category(),
                Some(Category::Tag | Category::LocalBranch)
            ) {
                continue;
            }

            valid_references.push(reference_name.as_bstr().to_string());

            if let Err(error) = branch_index_update(
                &mut reference,
                &relative_path,
                db_repository.get(),
                db.clone(),
                &git_repository,
                false,
                &submodules,
            ) {
                error!(%error, "Failed to update reflog for {relative_path}@{:?}", valid_references.last());
            }
        }

        if let Err(error) = db_repository.get().replace_heads(&db, &valid_references) {
            error!(%error, "Failed to update heads");
        }
    }
}

#[instrument(skip(reference, db_repository, db, git_repository))]
fn branch_index_update(
    reference: &mut Reference<'_>,
    relative_path: &str,
    db_repository: &ArchivedRepository,
    db: Arc<rocksdb::DB>,
    git_repository: &gix::Repository,
    force_reindex: bool,
    submodules: &BTreeMap<PathBuf, Url>,
) -> Result<(), anyhow::Error> {
    info!("Refreshing indexes");

    let commit_tree = db_repository.commit_tree(db.clone(), reference.name().as_bstr().to_str()?);

    if force_reindex {
        commit_tree.drop_commits()?;
    }

    let commit = reference.peel_to_commit()?;

    let latest_indexed = if let Some(latest_indexed) = commit_tree.fetch_latest_one()? {
        if commit.id().as_bytes() == latest_indexed.get().hash.as_slice() {
            info!("No commits since last index");
            return Ok(());
        }

        Some(latest_indexed)
    } else {
        None
    };

    // TODO: stop collecting into a vec
    let revwalk = git_repository
        .rev_walk([commit.id().detach()])
        .all()?
        .collect::<Vec<_>>()
        .into_iter()
        .rev();

    let mut hasher = Xxh3::new();

    let tree_len = commit_tree.len()?;
    let mut seen = false;
    let mut i = 0;
    for revs in &revwalk.chunks(250) {
        let mut batch = WriteBatch::default();

        for rev in revs {
            let rev = rev?;

            if let (false, Some(latest_indexed)) = (seen, &latest_indexed) {
                if rev.id.as_bytes() == latest_indexed.get().hash.as_slice() {
                    seen = true;
                }

                continue;
            }

            seen = true;

            if ((i + 1) % 25_000) == 0 {
                info!("{} commits ingested", i + 1);
            }

            let commit = rev.object()?;
            let oid = commit.id;
            let commit = commit.decode()?;
            let author = commit.author();
            let committer = commit.committer();

            let tree = git_repository.find_tree(commit.tree())?;
            let tree_id = index_tree(&db, &mut batch, &tree, &mut hasher, submodules)?;

            Commit::new(oid, &commit, author, committer, tree_id)?.insert(
                &commit_tree,
                tree_len + i,
                &mut batch,
            )?;

            i += 1;
        }

        commit_tree.update_counter(tree_len + i, &mut batch)?;
        db.write_without_wal(batch)?;
    }

    if !seen && !force_reindex {
        warn!("Detected converged history, forcing reindex");

        return branch_index_update(
            reference,
            relative_path,
            db_repository,
            db,
            git_repository,
            true,
            submodules,
        );
    }

    Ok(())
}

fn index_tree(
    database: &rocksdb::DB,
    batch: &mut WriteBatch,
    tree: &gix::Tree<'_>,
    hasher: &mut Xxh3,
    submodules: &BTreeMap<PathBuf, Url>,
) -> Result<u64, anyhow::Error> {
    hasher.reset();
    tree.traverse()
        .breadthfirst(&mut PathVisitor::new(TreeHasherVisitor { hasher }))?;
    let digest = hasher.digest();

    if !TreeItem::contains(database, digest)? {
        let mut sorted_out = SortedTree::default();

        tree.traverse()
            .breadthfirst(&mut PathVisitor::new(TreeItemIndexerVisitor {
                buffer: Vec::new(),
                digest,
                database,
                batch,
                submodules,
                sorted: &mut sorted_out,
            }))?;

        sorted_out.insert(digest, database, batch)?;
    }

    Tree {
        indexed_tree_id: digest,
    }
    .insert(database, batch, tree.id)?;

    Ok(digest)
}

/// Walks the entire tree and hashes all the (path, mode)s so trees can be deduplicated.
///
/// Note: unlike git's tree oid, this does not take into account blob contents.
struct TreeHasherVisitor<'a> {
    hasher: &'a mut Xxh3,
}

impl PathVisitorHandler for TreeHasherVisitor<'_> {
    fn visit(
        &mut self,
        entry: &gix::objs::tree::EntryRef<'_>,
        path: &BStr,
    ) -> gix::traverse::tree::visit::Action {
        self.hasher.update(path);
        self.hasher.update(&entry.mode.to_ne_bytes());
        gix::traverse::tree::visit::Action::Continue
    }
}

struct TreeItemIndexerVisitor<'a> {
    digest: u64,
    buffer: Vec<u8>,
    sorted: &'a mut SortedTree,
    database: &'a rocksdb::DB,
    batch: &'a mut WriteBatch,
    submodules: &'a BTreeMap<PathBuf, Url>,
}

impl PathVisitorHandler for TreeItemIndexerVisitor<'_> {
    fn visit(
        &mut self,
        entry: &gix::objs::tree::EntryRef<'_>,
        path: &BStr,
    ) -> gix::traverse::tree::visit::Action {
        let kind = match entry.mode.kind() {
            EntryKind::Blob | EntryKind::BlobExecutable | EntryKind::Link => TreeItemKind::File,
            EntryKind::Commit => {
                let Some(mut url) = self
                    .submodules
                    .get(&path.to_path_lossy().into_owned())
                    .cloned()
                else {
                    return gix::traverse::tree::visit::Action::Continue;
                };

                if matches!(url.scheme, Scheme::Git | Scheme::Ssh) {
                    url.scheme = Scheme::Https;
                }

                TreeItemKind::Submodule(match entry.oid.to_owned() {
                    ObjectId::Sha1(oid) => super::schema::tree::Submodule {
                        url: url.to_string(),
                        oid,
                    },
                })
            }
            EntryKind::Tree => TreeItemKind::Tree,
        };

        // TODO: this could be more optimised doing a recursive DFS to not have to keep diving deep into self.sorted
        if !entry.mode.is_tree() {
            let mut tree = &mut *self.sorted;
            let mut offset = 0;
            for end in memchr::memchr_iter(b'/', path) {
                let path = &path[offset..end];
                offset = end + 1;

                tree = match tree
                    .0
                    .entry(TreeKey(path.to_string()))
                    .or_insert(SortedTreeItem::Directory(SortedTree::default()))
                {
                    SortedTreeItem::Directory(dir) => dir,
                    SortedTreeItem::File => panic!("a file is somehow not in a directory"),
                };
            }
            tree.0
                .insert(TreeKey(path[offset..].to_string()), SortedTreeItem::File);
        }

        TreeItem {
            mode: entry.mode.0,
            kind,
        }
        .insert(
            &mut self.buffer,
            self.digest,
            path,
            self.database,
            self.batch,
        )
        .expect("failed to insert TreeItem");

        gix::traverse::tree::visit::Action::Continue
    }
}

#[instrument(skip(db))]
fn update_repository_tags(scan_path: &Path, db: Arc<rocksdb::DB>) {
    let repos = match Repository::fetch_all(&db) {
        Ok(v) => v,
        Err(error) => {
            error!(%error, "Failed to read repository index to update tags, consider deleting database directory");
            return;
        }
    };

    for (relative_path, db_repository) in repos {
        let Some(git_repository) = open_repo(scan_path, &relative_path, db_repository.get(), &db)
        else {
            continue;
        };

        let submodules = match git_repository.submodules() {
            Ok(submodules) => submodules
                .into_iter()
                .flatten()
                .filter_map(|v| Some((v.name().to_path_lossy().to_path_buf(), v.url().ok()?)))
                .collect::<BTreeMap<_, _>>(),
            Err(error) => {
                error!(%error, "Failed to read submodules for {relative_path}");
                continue;
            }
        };

        if let Err(error) = tag_index_scan(
            &relative_path,
            db_repository.get(),
            db.clone(),
            &git_repository,
            &submodules,
        ) {
            error!(%error, "Failed to update tags for {relative_path}");
        }
    }
}

#[instrument(skip(db_repository, db, git_repository))]
fn tag_index_scan(
    relative_path: &str,
    db_repository: &ArchivedRepository,
    db: Arc<rocksdb::DB>,
    git_repository: &gix::Repository,
    submodules: &BTreeMap<PathBuf, Url>,
) -> Result<(), anyhow::Error> {
    let tag_tree = db_repository.tag_tree(db);

    let git_tags: HashSet<_> = git_repository
        .references()
        .context("Failed to scan indexes on git repository")?
        .all()?
        .filter_map(Result::ok)
        .filter(|v| v.name().category() == Some(Category::Tag))
        .map(|v| v.name().as_bstr().to_string())
        .collect();
    let indexed_tags: HashSet<String> = tag_tree.list()?.into_iter().collect();

    // insert any git tags that are missing from the index
    for tag_name in git_tags.difference(&indexed_tags) {
        tag_index_update(tag_name, git_repository, &tag_tree, submodules)?;
    }

    // remove any extra tags that the index has
    // TODO: this also needs to check peel_to_tag
    for tag_name in indexed_tags.difference(&git_tags) {
        tag_index_delete(tag_name, &tag_tree)?;
    }

    Ok(())
}

#[instrument(skip(git_repository, tag_tree))]
fn tag_index_update(
    tag_name: &str,
    git_repository: &gix::Repository,
    tag_tree: &TagTree,
    submodules: &BTreeMap<PathBuf, Url>,
) -> Result<(), anyhow::Error> {
    let mut reference = git_repository
        .find_reference(tag_name)
        .context("Failed to read newly discovered tag")?;

    let tree_id = if let Ok(tree) = reference.peel_to_tree() {
        let mut batch = WriteBatch::default();
        let tree_id = index_tree(
            &tag_tree.db,
            &mut batch,
            &tree,
            &mut Xxh3::new(),
            submodules,
        )?;
        tag_tree.db.write_without_wal(batch)?;
        Some(tree_id)
    } else {
        None
    };

    if let Ok(tag) = reference.peel_to_tag() {
        info!("Inserting newly discovered tag to index");

        Tag::new(tag.tagger()?, tree_id)?.insert(tag_tree, tag_name)?;
    }

    Ok(())
}

#[instrument(skip(tag_tree))]
fn tag_index_delete(tag_name: &str, tag_tree: &TagTree) -> Result<(), anyhow::Error> {
    info!("Removing stale tag from index");
    tag_tree.remove(tag_name)?;

    Ok(())
}

#[instrument(skip(scan_path, db_repository, db))]
fn open_repo<P: AsRef<Path> + Debug>(
    scan_path: &Path,
    relative_path: P,
    db_repository: &ArchivedRepository,
    db: &rocksdb::DB,
) -> Option<gix::Repository> {
    match gix::open(scan_path.join(relative_path.as_ref())) {
        Ok(mut v) => {
            v.object_cache_size(10 * 1024 * 1024);
            Some(v)
        }
        Err(gix::open::Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            warn!("Repository gone from disk, removing from db");

            if let Err(error) = db_repository.delete(db, relative_path) {
                warn!(%error, "Failed to delete dangling index");
            }

            None
        }
        Err(error) => {
            warn!(%error, "Failed to reindex, skipping");
            None
        }
    }
}

fn get_relative_path<'a>(relative_to: &Path, full_path: &'a Path) -> Option<&'a Path> {
    full_path.strip_prefix(relative_to).ok()
}

fn discover_repositories(
    current: &Path,
    repository_list: Option<&Path>,
    discovered_repos: &mut Vec<(PathBuf, gix::Repository)>,
) {
    let dirs = if let Some(repo_list) = repository_list {
        let repo_list = match std::fs::File::open(repo_list) {
            Ok(v) => BufReader::new(v).lines(),
            Err(error) => {
                error!(%error, "Failed to open repository list file");
                return;
            }
        };

        let mut out = Vec::new();

        for line in repo_list {
            let line = match line {
                Ok(v) => v,
                Err(error) => {
                    error!(%error, "Failed to read repository list file");
                    return;
                }
            };

            out.push(current.join(line));
        }

        Either::Left(out.into_iter())
    } else {
        let current = match std::fs::read_dir(current) {
            Ok(v) => v,
            Err(error) => {
                error!(%error, "Failed to enter repository directory {}", current.display());
                return;
            }
        };

        Either::Right(
            current
                .filter_map(Result::ok)
                .map(|v| v.path())
                .filter(|path| path.is_dir()),
        )
    };

    for dir in dirs {
        match gix::open_opts(&dir, gix::open::Options::default().open_path_as_is(true)) {
            Ok(mut repo) => {
                repo.object_cache_size(10 * 1024 * 1024);
                discovered_repos.push((dir, repo));
            }
            Err(gix::open::Error::NotARepository { .. }) if repository_list.is_none() => {
                discover_repositories(&dir, None, discovered_repos);
            }

            Err(gix::open::Error::NotARepository { .. }) => {
                warn!(
                    "Repository list points to directory which isn't a Git repository: {}",
                    dir.display()
                );
            }
            Err(error) => {
                warn!(%error, "Failed to open repository {} for indexing", dir.display());
            }
        }
    }
}
