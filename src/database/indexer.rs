use git2::Sort;
use std::path::{Path, PathBuf};
use time::OffsetDateTime;
use tracing::{info, info_span};

use crate::database::schema::{
    commit::Commit,
    repository::{Repository, RepositoryId},
};

pub fn run(db: &sled::Db) {
    let scan_path = Path::new("/Users/jordan/Code/test-git");
    update_repository_metadata(scan_path, db);
    update_repository_reflog(scan_path, db);
}

fn update_repository_metadata(scan_path: &Path, db: &sled::Db) {
    let mut discovered = Vec::new();
    discover_repositories(scan_path, &mut discovered);

    for repository in discovered {
        let relative = get_relative_path(scan_path, &repository);

        let id =
            Repository::open(db, relative).map_or_else(|| RepositoryId::new(db), |v| v.get().id);
        let name = relative.file_name().unwrap().to_string_lossy();
        let description = std::fs::read(repository.join("description")).unwrap_or_default();
        let description = Some(String::from_utf8_lossy(&description)).filter(|v| !v.is_empty());

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
    for (relative_path, db_repository) in Repository::fetch_all(db) {
        let git_repository = git2::Repository::open(scan_path.join(&relative_path)).unwrap();

        for reference in git_repository.references().unwrap() {
            let reference = reference.unwrap();

            let reference_name = String::from_utf8_lossy(reference.name_bytes());
            if !reference_name.starts_with("refs/heads/") {
                continue;
            }

            let span = info_span!(
                "index_update",
                reference = reference_name.as_ref(),
                repository = relative_path
            );
            let _entered = span.enter();

            info!("Refreshing indexes");

            let commit_tree = db_repository.get().commit_tree(db, &reference_name);

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
            revwalk.push_ref(&reference_name).unwrap();

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
                commit_tree.remove(&to_remove.to_be_bytes()).unwrap();
            }
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
