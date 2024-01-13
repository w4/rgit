use std::{
    borrow::Cow,
    collections::HashSet,
    ffi::OsStr,
    fmt::Debug,
    path::{Path, PathBuf},
};

use anyhow::Context;
use git2::{ErrorCode, Reference, Sort};
use ini::Ini;
use time::OffsetDateTime;
use tracing::{error, info, info_span, instrument, warn};

use crate::database::schema::{
    commit::Commit,
    prefixes::TreePrefix,
    repository::{Repository, RepositoryId},
    tag::{Tag, TagTree},
};

pub fn run(scan_path: &Path, db: &sled::Db) {
    let span = info_span!("index_update");
    let _entered = span.enter();

    info!("Starting index update");

    update_repository_metadata(scan_path, db);
    update_repository_reflog(scan_path, db);
    update_repository_tags(scan_path, db);

    info!("Flushing to disk");

    if let Err(error) = db.flush() {
        error!(%error, "Failed to flush database to disk");
    }

    info!("Finished index update");
}

#[instrument(skip(db))]
fn update_repository_metadata(scan_path: &Path, db: &sled::Db) {
    let mut discovered = Vec::new();
    discover_repositories(scan_path, &mut discovered);

    for repository in discovered {
        let Some(relative) = get_relative_path(scan_path, &repository) else {
            continue;
        };

        let id = match Repository::open(db, relative) {
            Ok(v) => v.map_or_else(|| RepositoryId::new(db), |v| v.get().id),
            Err(error) => {
                // maybe we could nuke it ourselves, but we need to instantly trigger
                // a reindex and we could enter into an infinite loop if there's a bug
                // or something
                error!(%error, "Failed to open repository index {}, please consider nuking database", relative.display());
                continue;
            }
        };

        let Some(name) = relative.file_name().map(OsStr::to_string_lossy) else {
            continue;
        };
        let description = std::fs::read(repository.join("description")).unwrap_or_default();
        let description = Some(String::from_utf8_lossy(&description)).filter(|v| !v.is_empty());

        let repository_path = scan_path.join(relative);

        let git_repository = match git2::Repository::open(repository_path.clone()) {
            Ok(v) => v,
            Err(error) => {
                warn!(%error, "Failed to open repository {} to update metadata, skipping", relative.display());
                continue;
            }
        };

        Repository {
            id,
            name,
            description,
            owner: find_gitweb_owner(repository_path.as_path()),
            last_modified: find_last_committed_time(&git_repository)
                .unwrap_or(OffsetDateTime::UNIX_EPOCH),
            default_branch: find_default_branch(&git_repository)
                .ok()
                .flatten()
                .map(Cow::Owned),
        }
        .insert(db, relative);
    }
}

fn find_default_branch(repo: &git2::Repository) -> Result<Option<String>, git2::Error> {
    Ok(repo.head()?.name().map(ToString::to_string))
}

fn find_last_committed_time(repo: &git2::Repository) -> Result<OffsetDateTime, git2::Error> {
    let mut timestamp = OffsetDateTime::UNIX_EPOCH;

    for reference in repo.references()? {
        let Ok(commit) = reference?.peel_to_commit() else {
            continue;
        };

        let committed_time = commit.committer().when().seconds();
        let committed_time = OffsetDateTime::from_unix_timestamp(committed_time)
            .unwrap_or(OffsetDateTime::UNIX_EPOCH);

        if committed_time > timestamp {
            timestamp = committed_time;
        }
    }

    Ok(timestamp)
}

#[instrument(skip(db))]
fn update_repository_reflog(scan_path: &Path, db: &sled::Db) {
    let repos = match Repository::fetch_all(db) {
        Ok(v) => v,
        Err(error) => {
            error!(%error, "Failed to read repository index to update reflog, consider deleting database directory");
            return;
        }
    };

    for (relative_path, db_repository) in repos {
        let Some(git_repository) = open_repo(scan_path, &relative_path, db_repository.get(), db)
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

        for reference in references.filter_map(Result::ok) {
            let reference_name = String::from_utf8_lossy(reference.name_bytes());
            if !reference_name.starts_with("refs/heads/")
                && !reference_name.starts_with("refs/tags/")
            {
                continue;
            }

            if let Err(error) = branch_index_update(
                &reference,
                &reference_name,
                &relative_path,
                db_repository.get(),
                db,
                &git_repository,
                false,
            ) {
                error!(%error, "Failed to update reflog for {relative_path}@{reference_name}");
            }
        }
    }
}

#[instrument(skip(reference, db_repository, db, git_repository))]
fn branch_index_update(
    reference: &Reference<'_>,
    reference_name: &str,
    relative_path: &str,
    db_repository: &Repository<'_>,
    db: &sled::Db,
    git_repository: &git2::Repository,
    force_reindex: bool,
) -> Result<(), anyhow::Error> {
    info!("Refreshing indexes");

    if force_reindex {
        db.drop_tree(TreePrefix::commit_id(db_repository.id, reference_name))?;
    }

    let commit = reference.peel_to_commit()?;
    let commit_tree = db_repository.commit_tree(db, reference_name)?;

    let latest_indexed = if let Some(latest_indexed) = commit_tree.fetch_latest_one() {
        if commit.id().as_bytes() == &*latest_indexed.get().hash {
            info!("No commits since last index");
            return Ok(());
        }

        Some(latest_indexed)
    } else {
        None
    };

    let mut revwalk = git_repository.revwalk()?;
    revwalk.set_sorting(Sort::REVERSE)?;
    revwalk.push_ref(reference_name)?;

    let tree_len = commit_tree.len();
    let mut seen = false;
    let mut i = 0;
    for rev in revwalk {
        let rev = rev?;

        if let (false, Some(latest_indexed)) = (seen, &latest_indexed) {
            if rev.as_bytes() == &*latest_indexed.get().hash {
                seen = true;
            }

            continue;
        }

        seen = true;

        if ((i + 1) % 25_000) == 0 {
            info!("{} commits ingested", i + 1);
        }

        let commit = git_repository.find_commit(rev)?;
        let author = commit.author();
        let committer = commit.committer();

        Commit::new(&commit, &author, &committer).insert(&commit_tree, tree_len + i);
        i += 1;
    }

    if !seen && !force_reindex {
        warn!("Detected converged history, forcing reindex");

        return branch_index_update(
            reference,
            reference_name,
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
fn update_repository_tags(scan_path: &Path, db: &sled::Db) {
    let repos = match Repository::fetch_all(db) {
        Ok(v) => v,
        Err(error) => {
            error!(%error, "Failed to read repository index to update tags, consider deleting database directory");
            return;
        }
    };

    for (relative_path, db_repository) in repos {
        let Some(git_repository) = open_repo(scan_path, &relative_path, db_repository.get(), db)
        else {
            continue;
        };

        if let Err(error) = tag_index_scan(&relative_path, db_repository.get(), db, &git_repository)
        {
            error!(%error, "Failed to update tags for {relative_path}");
        }
    }
}

#[instrument(skip(db_repository, db, git_repository))]
fn tag_index_scan(
    relative_path: &str,
    db_repository: &Repository<'_>,
    db: &sled::Db,
    git_repository: &git2::Repository,
) -> Result<(), anyhow::Error> {
    let tag_tree = db_repository
        .tag_tree(db)
        .context("Failed to read tag index tree")?;

    let git_tags: HashSet<_> = git_repository
        .references()
        .context("Failed to scan indexes on git repository")?
        .filter_map(Result::ok)
        .filter(|v| v.name_bytes().starts_with(b"refs/tags/"))
        .map(|v| String::from_utf8_lossy(v.name_bytes()).into_owned())
        .collect();
    let indexed_tags: HashSet<String> = tag_tree.list().into_iter().collect();

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
    git_repository: &git2::Repository,
    tag_tree: &TagTree,
) -> Result<(), anyhow::Error> {
    let reference = git_repository
        .find_reference(tag_name)
        .context("Failed to read newly discovered tag")?;

    if let Ok(tag) = reference.peel_to_tag() {
        info!("Inserting newly discovered tag to index");

        Tag::new(tag.tagger().as_ref()).insert(tag_tree, tag_name)?;
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
    db_repository: &Repository<'_>,
    db: &sled::Db,
) -> Option<git2::Repository> {
    match git2::Repository::open(scan_path.join(relative_path.as_ref())) {
        Ok(v) => Some(v),
        Err(e) if e.code() == ErrorCode::NotFound => {
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

fn find_gitweb_owner(repository_path: &Path) -> Option<Cow<'_, str>> {
    // Load the Git config file and attempt to extract the owner from the "gitweb" section.
    // If the owner is not found, an empty string is returned.
    Ini::load_from_file(repository_path.join("config"))
        .ok()?
        .section(Some("gitweb"))
        .and_then(|section| section.get("owner"))
        .map(String::from)
        .map(Cow::Owned)
}
