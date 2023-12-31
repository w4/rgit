use std::{
    borrow::Cow,
    collections::HashSet,
    path::{Path, PathBuf},
};

use git2::Sort;
use ini::Ini;
use time::OffsetDateTime;
use tracing::{error, info, info_span};

use crate::database::schema::{
    commit::Commit,
    repository::{Repository, RepositoryId},
    tag::Tag,
};

pub fn run(scan_path: &Path, db: &sled::Db) {
    let span = info_span!("index_update");
    let _entered = span.enter();

    info!("Starting index update");

    update_repository_metadata(scan_path, db);
    update_repository_reflog(scan_path, db);
    update_repository_tags(scan_path, db);

    info!("Flushing to disk");

    db.flush().unwrap();

    info!("Finished index update");
}

fn update_repository_metadata(scan_path: &Path, db: &sled::Db) {
    let mut discovered = Vec::new();
    discover_repositories(scan_path, &mut discovered);

    for repository in discovered {
        let relative = get_relative_path(scan_path, &repository);

        let id = Repository::open(db, relative)
            .unwrap()
            .map_or_else(|| RepositoryId::new(db), |v| v.get().id);
        let name = relative.file_name().unwrap().to_string_lossy();
        let description = std::fs::read(repository.join("description")).unwrap_or_default();
        let description = Some(String::from_utf8_lossy(&description)).filter(|v| !v.is_empty());

        let repository_path = scan_path.join(relative);
        let git_repository = git2::Repository::open(repository_path.clone()).unwrap();

        Repository {
            id,
            name,
            description,
            owner: find_gitweb_owner(repository_path.as_path()),
            last_modified: find_last_committed_time(&git_repository)
                .unwrap_or(OffsetDateTime::UNIX_EPOCH),
        }
        .insert(db, relative);
    }
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

fn update_repository_reflog(scan_path: &Path, db: &sled::Db) {
    for (relative_path, db_repository) in Repository::fetch_all(db).unwrap() {
        let git_repository = git2::Repository::open(scan_path.join(&relative_path)).unwrap();

        for reference in git_repository.references().unwrap() {
            let reference = reference.unwrap();

            let reference_name = String::from_utf8_lossy(reference.name_bytes());
            if !reference_name.starts_with("refs/heads/")
                && !reference_name.starts_with("refs/tags/")
            {
                continue;
            }

            let span = info_span!(
                "branch_index_update",
                reference = reference_name.as_ref(),
                repository = relative_path
            );
            let _entered = span.enter();

            info!("Refreshing indexes");

            let commit_tree = db_repository
                .get()
                .commit_tree(db, &reference_name)
                .unwrap();

            if let (Some(latest_indexed), Ok(latest_commit)) =
                (commit_tree.fetch_latest_one(), reference.peel_to_commit())
            {
                if latest_commit.id().as_bytes() == &*latest_indexed.get().hash {
                    info!("No commits since last index");
                    continue;
                }
            }

            // TODO: only scan revs from the last time we looked
            let mut revwalk = git_repository.revwalk().unwrap();
            revwalk.set_sorting(Sort::REVERSE).unwrap();
            if let Err(error) = revwalk.push_ref(&reference_name) {
                error!(%error, "Failed to revwalk reference");
                continue;
            }

            let mut i = 0;
            for rev in revwalk {
                let commit = git_repository.find_commit(rev.unwrap()).unwrap();
                let author = commit.author();
                let committer = commit.committer();

                Commit::new(&commit, &author, &committer).insert(&commit_tree, i);
                i += 1;
            }

            // a complete and utter hack to remove potentially dropped commits from our tree,
            // we'll need to add `clear()` to sled's tx api to remove this
            for to_remove in (i + 1)..(i + 100) {
                commit_tree.remove(to_remove.to_be_bytes()).unwrap();
            }
        }
    }
}

fn update_repository_tags(scan_path: &Path, db: &sled::Db) {
    for (relative_path, db_repository) in Repository::fetch_all(db).unwrap() {
        let git_repository = git2::Repository::open(scan_path.join(&relative_path)).unwrap();

        let tag_tree = db_repository.get().tag_tree(db).unwrap();

        let git_tags: HashSet<_> = git_repository
            .references()
            .unwrap()
            .filter_map(Result::ok)
            .filter(|v| v.name_bytes().starts_with(b"refs/tags/"))
            .map(|v| String::from_utf8_lossy(v.name_bytes()).into_owned())
            .collect();
        let indexed_tags: HashSet<String> = tag_tree.list().into_iter().collect();

        // insert any git tags that are missing from the index
        for tag_name in git_tags.difference(&indexed_tags) {
            let span = info_span!(
                "tag_index_update",
                reference = tag_name,
                repository = relative_path
            );
            let _entered = span.enter();

            let reference = git_repository.find_reference(tag_name).unwrap();

            if let Ok(tag) = reference.peel_to_tag() {
                info!("Inserting newly discovered tag to index");

                Tag::new(tag.tagger().as_ref()).insert(&tag_tree, tag_name);
            }
        }

        // remove any extra tags that the index has
        // TODO: this also needs to check peel_to_tag
        for tag_name in indexed_tags.difference(&git_tags) {
            let span = info_span!(
                "tag_index_update",
                reference = tag_name,
                repository = relative_path
            );
            let _entered = span.enter();

            info!("Removing stale tag from index");

            tag_tree.remove(tag_name);
        }
    }
}

fn get_relative_path<'a>(relative_to: &Path, full_path: &'a Path) -> &'a Path {
    full_path.strip_prefix(relative_to).unwrap()
}

fn discover_repositories(current: &Path, discovered_repos: &mut Vec<PathBuf>) {
    let dirs = std::fs::read_dir(current)
        .unwrap()
        .map(|v| v.unwrap().path())
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
