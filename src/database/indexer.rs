use git2::Sort;
use std::path::{Path, PathBuf};
use time::OffsetDateTime;
use tracing::info;

use crate::database::schema::{
    commit::Commit,
    repository::{Repository, RepositoryId},
};

pub fn run_indexer(db: &sled::Db) {
    let scan_path = Path::new("/Users/jordan/Code/test-git");
    update_repository_metadata(scan_path, &db);
    update_repository_reflog(scan_path, &db);
}

fn update_repository_metadata(scan_path: &Path, db: &sled::Db) {
    let mut discovered = Vec::new();
    discover_repositories(scan_path, &mut discovered);

    for repository in discovered {
        let relative = get_relative_path(scan_path, &repository);

        let id = Repository::open(db, relative)
            .map(|v| v.id)
            .unwrap_or_else(|| RepositoryId::new(db));
        let name = relative.file_name().unwrap().to_string_lossy().to_string();
        let description = Some(
            String::from_utf8_lossy(
                &std::fs::read(repository.join("description")).unwrap_or_default(),
            )
            .to_string(),
        )
        .filter(|v| !v.is_empty());

        Repository {
            id,
            name,
            description,
            owner: None, // TODO read this from config
            last_modified: OffsetDateTime::now_utc(),
        }
        .insert(db, relative);
    }
}

fn update_repository_reflog(scan_path: &Path, db: &sled::Db) {
    for (relative_path, db_repository) in Repository::fetch_all(&db) {
        let git_repository = git2::Repository::open(scan_path.join(&relative_path)).unwrap();

        for reference in git_repository.references().unwrap() {
            let reference = if let Some(reference) = reference.as_ref().ok().and_then(|v| v.name())
            {
                reference
            } else {
                continue;
            };

            if !reference.starts_with("refs/heads/") {
                continue;
            }

            info!("Updating indexes for {} on {}", reference, relative_path);

            let commit_tree = db_repository.commit_tree(db, reference);

            // TODO: only scan revs from the last time we looked
            let mut revwalk = git_repository.revwalk().unwrap();
            revwalk.set_sorting(Sort::REVERSE).unwrap();
            revwalk.push_ref(reference).unwrap();
            let revs: Vec<_> = revwalk.collect::<Result<_, _>>().unwrap();

            let git_repository = &git_repository;

            commit_tree
                .transaction::<_, _, std::io::Error>(move |tx| {
                    for (i, rev) in revs.iter().enumerate() {
                        let commit = git_repository.find_commit(*rev).unwrap();

                        Commit::from(commit).insert(tx, i);
                    }

                    // a complete and utter hack to remove potentially dropped commits from our tree,
                    // we'll need to add `clear()` to sled's tx api to remove this
                    for to_remove in (revs.len() + 1)..(revs.len() + 100) {
                        tx.remove(&to_remove.to_be_bytes())?;
                    }

                    Ok(())
                })
                .unwrap();
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
