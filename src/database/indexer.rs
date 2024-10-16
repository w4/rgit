use std::{
    collections::HashSet,
    ffi::OsStr,
    fmt::Debug,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::Context;
use gix::{bstr::ByteSlice, refs::Category, Reference};
use ini::Ini;
use itertools::Itertools;
use rocksdb::WriteBatch;
use time::{OffsetDateTime, UtcOffset};
use tracing::{error, info, info_span, instrument, warn};

use crate::database::schema::{
    commit::Commit,
    repository::{ArchivedRepository, Repository, RepositoryId},
    tag::{Tag, TagTree},
};

pub fn run(scan_path: &Path, db: &Arc<rocksdb::DB>) {
    let span = info_span!("index_update");
    let _entered = span.enter();

    info!("Starting index update");

    update_repository_metadata(scan_path, db);
    update_repository_reflog(scan_path, db.clone());
    update_repository_tags(scan_path, db.clone());

    info!("Flushing to disk");

    if let Err(error) = db.flush() {
        error!(%error, "Failed to flush database to disk");
    }

    info!("Finished index update");
}

#[instrument(skip(db))]
fn update_repository_metadata(scan_path: &Path, db: &rocksdb::DB) {
    let mut discovered = Vec::new();
    discover_repositories(scan_path, &mut discovered);

    for repository in discovered {
        let Some(relative) = get_relative_path(scan_path, &repository) else {
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
        let description = std::fs::read(repository.join("description")).unwrap_or_default();
        let description = String::from_utf8(description)
            .ok()
            .filter(|v| !v.is_empty());

        let repository_path = scan_path.join(relative);

        let mut git_repository = match gix::open(repository_path.clone()) {
            Ok(v) => v,
            Err(error) => {
                warn!(%error, "Failed to open repository {} to update metadata, skipping", relative.display());
                continue;
            }
        };

        git_repository.object_cache_size(10 * 1024 * 1024);

        let res = Repository {
            id,
            name: name.to_string(),
            description,
            owner: find_gitweb_owner(repository_path.as_path()),
            last_modified: {
                let r =
                    find_last_committed_time(&git_repository).unwrap_or(OffsetDateTime::UNIX_EPOCH);
                (r.unix_timestamp(), r.offset().whole_seconds())
            },
            default_branch: find_default_branch(&git_repository).ok().flatten(),
        }
        .insert(db, relative);

        if let Err(error) = res {
            warn!(%error, "Failed to insert repository");
        }
    }
}

fn find_default_branch(repo: &gix::Repository) -> Result<Option<String>, anyhow::Error> {
    Ok(Some(repo.head()?.name().as_bstr().to_string()))
}

fn find_last_committed_time(repo: &gix::Repository) -> Result<OffsetDateTime, anyhow::Error> {
    let mut timestamp = OffsetDateTime::UNIX_EPOCH;

    for reference in repo.references()?.all()? {
        let Ok(commit) = reference.unwrap().peel_to_commit() else {
            continue;
        };

        let committer = commit.committer()?;
        let mut committed_time = OffsetDateTime::from_unix_timestamp(committer.time.seconds)
            .unwrap_or(OffsetDateTime::UNIX_EPOCH);

        if let Ok(offset) = UtcOffset::from_whole_seconds(committer.time.offset) {
            committed_time = committed_time.to_offset(offset);
        }

        if committed_time > timestamp {
            timestamp = committed_time;
        }
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
            let author = commit.author()?;
            let committer = commit.committer()?;

            Commit::new(&commit, author, committer)?.insert(
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
        );
    }

    Ok(())
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

        if let Err(error) = tag_index_scan(
            &relative_path,
            db_repository.get(),
            db.clone(),
            &git_repository,
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
        tag_index_update(tag_name, git_repository, &tag_tree)?;
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
) -> Result<(), anyhow::Error> {
    let mut reference = git_repository
        .find_reference(tag_name)
        .context("Failed to read newly discovered tag")?;

    if let Ok(tag) = reference.peel_to_tag() {
        info!("Inserting newly discovered tag to index");

        Tag::new(tag.tagger()?)?.insert(tag_tree, tag_name)?;
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

fn discover_repositories(current: &Path, discovered_repos: &mut Vec<PathBuf>) {
    let current = match std::fs::read_dir(current) {
        Ok(v) => v,
        Err(error) => {
            error!(%error, "Failed to enter repository directory {}", current.display());
            return;
        }
    };

    let dirs = current
        .filter_map(Result::ok)
        .map(|v| v.path())
        .filter(|path| path.is_dir());

    for dir in dirs {
        if dir.join("packed-refs").is_file() {
            // we've hit what looks like a bare git repo, lets take it
            discovered_repos.push(dir);
        } else {
            // probably not a bare git repo, lets recurse deeper
            discover_repositories(&dir, discovered_repos);
        }
    }
}

fn find_gitweb_owner(repository_path: &Path) -> Option<String> {
    // Load the Git config file and attempt to extract the owner from the "gitweb" section.
    // If the owner is not found, an empty string is returned.
    Ini::load_from_file(repository_path.join("config"))
        .ok()?
        .section_mut(Some("gitweb"))
        .and_then(|section| section.remove("owner"))
}
