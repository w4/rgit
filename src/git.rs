use std::{borrow::Cow, collections::BTreeMap, fmt::Display, path::Path, time::Duration};

use git2::{Repository, Signature};
use owning_ref::OwningHandle;
use time::OffsetDateTime;

pub type RepositoryMetadataList = BTreeMap<Option<String>, Vec<RepositoryMetadata>>;

#[derive(Debug)]
pub struct RepositoryMetadata {
    pub name: String,
    pub description: Option<Cow<'static, str>>,
    pub owner: Option<String>,
    pub last_modified: Duration,
}

pub struct CommitUser<'a>(Signature<'a>);

impl CommitUser<'_> {
    pub fn name(&self) -> &str {
        self.0.name().unwrap()
    }

    pub fn email(&self) -> &str {
        self.0.email().unwrap()
    }

    pub fn time(&self) -> String {
        OffsetDateTime::from_unix_timestamp(self.0.when().seconds())
            .unwrap()
            .to_string()
    }
}

pub struct Commit(OwningHandle<Box<Repository>, Box<git2::Commit<'static>>>);

impl Commit {
    pub fn author(&self) -> CommitUser<'_> {
        CommitUser(self.0.author())
    }

    pub fn committer(&self) -> CommitUser<'_> {
        CommitUser(self.0.committer())
    }

    pub fn oid(&self) -> impl Display {
        self.0.id()
    }

    pub fn tree(&self) -> impl Display {
        self.0.tree_id()
    }

    pub fn parents(&self) -> impl Iterator<Item = impl Display + '_> {
        self.0.parent_ids()
    }

    pub fn summary(&self) -> &str {
        self.0.summary().unwrap()
    }

    pub fn body(&self) -> &str {
        self.0.message().unwrap()
    }
}

pub fn get_latest_commit(path: &Path) -> Commit {
    let repo = Repository::open_bare(path).unwrap();

    let commit = OwningHandle::new_with_fn(Box::new(repo), |v| {
        let head = unsafe { (*v).head().unwrap() };
        Box::new(head.peel_to_commit().unwrap())
    });

    // TODO: we can cache this
    Commit(commit)
}

pub fn fetch_repository_metadata() -> RepositoryMetadataList {
    let start = Path::new("../test-git").canonicalize().unwrap();

    let mut repos: RepositoryMetadataList = RepositoryMetadataList::new();
    fetch_repository_metadata_impl(&start, &start, &mut repos);
    repos
}

fn fetch_repository_metadata_impl(
    start: &Path,
    current: &Path,
    repos: &mut RepositoryMetadataList,
) {
    let dirs = std::fs::read_dir(current)
        .unwrap()
        .map(|v| v.unwrap().path())
        .filter(|path| path.is_dir());

    for dir in dirs {
        let repository = match Repository::open_bare(&dir) {
            Ok(v) => v,
            Err(_e) => {
                fetch_repository_metadata_impl(start, &dir, repos);
                continue;
            }
        };

        let repo_path = Some(
            current
                .strip_prefix(start)
                .unwrap()
                .to_string_lossy()
                .into_owned(),
        )
        .filter(|v| !v.is_empty());
        let repos = repos.entry(repo_path).or_default();

        let description = std::fs::read_to_string(dir.join("description"))
            .map(Cow::Owned)
            .ok();
        let last_modified = std::fs::metadata(&dir).unwrap().modified().unwrap();
        let owner = repository.config().unwrap().get_string("gitweb.owner").ok();

        repos.push(RepositoryMetadata {
            name: dir
                .components()
                .last()
                .unwrap()
                .as_os_str()
                .to_string_lossy()
                .into_owned(),
            description,
            owner,
            last_modified: (OffsetDateTime::now_utc() - OffsetDateTime::from(last_modified))
                .unsigned_abs(),
        });
    }
}
