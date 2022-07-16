use std::{
    borrow::Cow,
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
    fmt::Write,
};

use arc_swap::ArcSwapOption;
use git2::{BranchType, DiffFormat, DiffLineType, DiffOptions, DiffStatsFormat, ObjectType, Oid, Repository, Signature};
use moka::future::Cache;
use parking_lot::Mutex;
use syntect::html::{ClassStyle, ClassedHTMLGenerator};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;
use time::OffsetDateTime;
use tracing::instrument;

pub type RepositoryMetadataList = BTreeMap<Option<String>, Vec<RepositoryMetadata>>;

pub struct Git {
    commits: Cache<Oid, Arc<Commit>>,
    readme_cache: Cache<PathBuf, Option<Arc<str>>>,
    refs: Cache<PathBuf, Arc<Refs>>,
    repository_metadata: ArcSwapOption<RepositoryMetadataList>,
    syntax_set: SyntaxSet,
}

impl Git {
    #[instrument(skip(syntax_set))]
    pub fn new(syntax_set: SyntaxSet) -> Self {
        Self {
            commits: Cache::builder()
                .time_to_live(Duration::from_secs(10))
                .max_capacity(100)
                .build(),
            readme_cache: Cache::builder()
                .time_to_live(Duration::from_secs(10))
                .max_capacity(100)
                .build(),
            refs: Cache::builder()
                .time_to_live(Duration::from_secs(10))
                .max_capacity(100)
                .build(),
            repository_metadata: ArcSwapOption::default(),
            syntax_set,
        }
    }
}

impl Git {
    #[instrument(skip(self))]
    pub async fn repo(self: Arc<Self>, repo_path: PathBuf) -> Arc<OpenRepository> {
        let repo = tokio::task::spawn_blocking({
            let repo_path = repo_path.clone();
            move || git2::Repository::open(repo_path).unwrap()
        })
        .await
        .unwrap();

        Arc::new(OpenRepository {
            git: self,
            cache_key: repo_path,
            repo: Mutex::new(repo),
        })
    }

    #[instrument(skip(self))]
    pub async fn fetch_repository_metadata(&self) -> Arc<RepositoryMetadataList> {
        if let Some(metadata) = self.repository_metadata.load().as_ref() {
            return Arc::clone(metadata);
        }

        let start = Path::new("../test-git").canonicalize().unwrap();

        let repos = tokio::task::spawn_blocking(move || {
            let mut repos: RepositoryMetadataList = RepositoryMetadataList::new();
            fetch_repository_metadata_impl(&start, &start, &mut repos);
            repos
        })
        .await
        .unwrap();

        let repos = Arc::new(repos);
        self.repository_metadata.store(Some(repos.clone()));

        repos
    }
}

pub struct OpenRepository {
    git: Arc<Git>,
    cache_key: PathBuf,
    repo: Mutex<git2::Repository>,
}

impl OpenRepository {
    pub async fn path(self: Arc<Self>, path: Option<PathBuf>, tree_id: Option<&str>, branch: Option<String>) -> PathDestination {
        let tree_id = tree_id.map(Oid::from_str).transpose().unwrap();

        tokio::task::spawn_blocking(move || {
            let repo = self.repo.lock();

            let mut tree = if let Some(tree_id) = tree_id {
                repo.find_tree(tree_id).unwrap()
            } else if let Some(branch) = branch {
                let branch = repo.find_branch(&branch, BranchType::Local).unwrap();
                branch.get().peel_to_tree().unwrap()
            } else {
                let head = repo.head().unwrap();
                head.peel_to_tree().unwrap()
            };

            if let Some(path) = path.as_ref() {
                let item = tree.get_path(&path).unwrap();
                let object = item.to_object(&repo).unwrap();

                if let Some(blob) = object.as_blob() {
                    let name = item.name().unwrap().to_string();
                    let path = path.clone().join(&name);

                    let extension = path.extension()
                        .or(path.file_name())
                        .unwrap()
                        .to_string_lossy();
                    let content = format_file(blob.content(), &extension, &self.git.syntax_set);

                    return PathDestination::File(FileWithContent {
                        metadata: File {
                            mode: item.filemode(),
                            size: blob.size(),
                            path,
                            name,
                        },
                        content,
                    });
                } else if let Ok(new_tree) = object.into_tree() {
                    tree = new_tree;
                } else {
                    panic!("unknown item kind");
                }
            }

            let mut tree_items = Vec::new();

            for item in tree.iter() {
                let object = item.to_object(&repo).unwrap();

                let name = item.name().unwrap().to_string();
                let path = path.clone().unwrap_or_default().join(&name);

                if let Some(blob) = object.as_blob() {
                    tree_items.push(TreeItem::File(File {
                        mode: item.filemode(),
                        size: blob.size(),
                        path,
                        name,
                    }));
                } else if let Some(_tree) = object.as_tree() {
                    tree_items.push(TreeItem::Tree(Tree {
                        mode: item.filemode(),
                        path,
                        name,
                    }));
                }
            }

            PathDestination::Tree(tree_items)
        })
        .await
        .unwrap()
    }

    #[instrument(skip(self))]
    pub async fn tag_info(self: Arc<Self>, tag_name: &str) -> DetailedTag {
        let reference = format!("refs/tags/{tag_name}");
        let tag_name = tag_name.to_string();

        tokio::task::spawn_blocking(move || {
            let repo = self.repo.lock();

            let tag = repo
                .find_reference(&reference)
                .unwrap()
                .peel_to_tag()
                .unwrap();
            let tag_target = tag.target().unwrap();

            let tagged_object = match tag_target.kind() {
                Some(ObjectType::Commit) => Some(TaggedObject::Commit(tag_target.id().to_string())),
                Some(ObjectType::Tree) => Some(TaggedObject::Tree(tag_target.id().to_string())),
                None | Some(_) => None,
            };

            DetailedTag {
                name: tag_name,
                tagger: tag.tagger().map(Into::into),
                message: tag.message().unwrap().to_string(),
                tagged_object,
            }
        })
        .await
        .unwrap()
    }

    #[instrument(skip(self))]
    pub async fn refs(self: Arc<Self>) -> Arc<Refs> {
        let git = self.git.clone();

        git.refs
            .get_with(self.cache_key.clone(), async move {
                tokio::task::spawn_blocking(move || {
                    let repo = self.repo.lock();

                    let ref_iter = repo.references().unwrap();

                    let mut built_refs = Refs::default();

                    for ref_ in ref_iter {
                        let ref_ = ref_.unwrap();

                        if ref_.is_branch() {
                            let commit = ref_.peel_to_commit().unwrap();

                            built_refs.branch.push(Branch {
                                name: ref_.shorthand().unwrap().to_string(),
                                commit: commit.into(),
                            });
                        } else if ref_.is_tag() {
                            if let Ok(tag) = ref_.peel_to_tag() {
                                built_refs.tag.push(Tag {
                                    name: ref_.shorthand().unwrap().to_string(),
                                    tagger: tag.tagger().map(Into::into),
                                });
                            }
                        }
                    }

                    Arc::new(built_refs)
                })
                .await
                .unwrap()
            })
            .await
    }

    #[instrument(skip(self))]
    pub async fn readme(self: Arc<Self>) -> Option<Arc<str>> {
        const README_FILES: &[&str] = &["README.md", "README", "README.txt"];

        let git = self.git.clone();

        git.readme_cache
            .get_with(self.cache_key.clone(), async move {
                tokio::task::spawn_blocking(move || {
                    let repo = self.repo.lock();

                    let head = repo.head().unwrap();
                    let commit = head.peel_to_commit().unwrap();
                    let tree = commit.tree().unwrap();

                    let blob = README_FILES
                        .iter()
                        .map(|file| tree.get_name(file))
                        .next()
                        .flatten()?
                        .to_object(&repo)
                        .unwrap()
                        .into_blob()
                        .unwrap();

                    Some(Arc::from(
                        String::from_utf8(blob.content().to_vec()).unwrap(),
                    ))
                })
                .await
                .unwrap()
            })
            .await
    }

    #[instrument(skip(self))]
    pub async fn latest_commit(self: Arc<Self>) -> Commit {
        tokio::task::spawn_blocking(move || {
            let repo = self.repo.lock();

            let head = repo.head().unwrap();
            let commit = head.peel_to_commit().unwrap();
            let (diff_output, diff_stats) =
                fetch_diff_and_stats(&repo, &commit, &self.git.syntax_set);

            let mut commit = Commit::from(commit);
            commit.diff_stats = diff_stats;
            commit.diff = diff_output;
            commit
        })
        .await
        .unwrap()
    }

    #[instrument(skip(self))]
    pub async fn commit(self: Arc<Self>, commit: &str) -> Arc<Commit> {
        let commit = Oid::from_str(commit).unwrap();

        let git = self.git.clone();

        git.commits
            .get_with(commit, async move {
                tokio::task::spawn_blocking(move || {
                    let repo = self.repo.lock();

                    let commit = repo.find_commit(commit).unwrap();
                    let (diff_output, diff_stats) =
                        fetch_diff_and_stats(&repo, &commit, &self.git.syntax_set);

                    let mut commit = Commit::from(commit);
                    commit.diff_stats = diff_stats;
                    commit.diff = diff_output;

                    Arc::new(commit)
                })
                .await
                .unwrap()
            })
            .await
    }

    #[instrument(skip(self))]
    pub async fn commits(
        self: Arc<Self>,
        branch: Option<&str>,
        offset: usize,
    ) -> (Vec<Commit>, Option<usize>) {
        const LIMIT: usize = 200;

        let ref_name = branch.map(|branch| format!("refs/heads/{}", branch));

        tokio::task::spawn_blocking(move || {
            let repo = self.repo.lock();
            let mut revs = repo.revwalk().unwrap();

            if let Some(ref_name) = ref_name.as_deref() {
                revs.push_ref(ref_name).unwrap();
            } else {
                revs.push_head().unwrap();
            }

            let mut commits: Vec<Commit> = revs
                .skip(offset)
                .take(LIMIT + 1)
                .map(|rev| {
                    let rev = rev.unwrap();
                    repo.find_commit(rev).unwrap().into()
                })
                .collect();

            // TODO: avoid having to take + 1 and popping the last commit off
            let next_offset = commits.pop().is_some().then(|| offset + commits.len());

            (commits, next_offset)
        })
        .await
        .unwrap()
    }
}

pub enum PathDestination {
    Tree(Vec<TreeItem>),
    File(FileWithContent),
}

pub enum TreeItem {
    Tree(Tree),
    File(File),
}

#[derive(Debug)]
pub struct Tree {
    pub mode: i32,
    pub name: String,
    pub path: PathBuf,
}

#[derive(Debug)]
pub struct File {
    pub mode: i32,
    pub size: usize,
    pub name: String,
    pub path: PathBuf,
}

#[derive(Debug)]
pub struct FileWithContent {
    pub metadata: File,
    pub content: String,
}

#[derive(Debug, Default)]
pub struct Refs {
    pub branch: Vec<Branch>,
    pub tag: Vec<Tag>,
}

#[derive(Debug)]
pub struct Branch {
    pub name: String,
    pub commit: Commit,
}

#[derive(Debug)]
pub struct Remote {
    pub name: String,
}

#[derive(Debug)]
pub enum TaggedObject {
    Commit(String),
    Tree(String),
}

#[derive(Debug)]
pub struct DetailedTag {
    pub name: String,
    pub tagger: Option<CommitUser>,
    pub message: String,
    pub tagged_object: Option<TaggedObject>,
}

#[derive(Debug)]
pub struct Tag {
    pub name: String,
    pub tagger: Option<CommitUser>,
}

#[derive(Debug)]
pub struct RepositoryMetadata {
    pub name: String,
    pub description: Option<Cow<'static, str>>,
    pub owner: Option<String>,
    pub last_modified: OffsetDateTime,
}

#[derive(Debug)]
pub struct CommitUser {
    name: String,
    email: String,
    email_md5: String,
    time: OffsetDateTime,
}

impl From<Signature<'_>> for CommitUser {
    fn from(v: Signature<'_>) -> Self {
        CommitUser {
            name: v.name().unwrap().to_string(),
            email: v.email().unwrap().to_string(),
            email_md5: format!("{:x}", md5::compute(v.email_bytes())),
            time: OffsetDateTime::from_unix_timestamp(v.when().seconds()).unwrap(),
        }
    }
}

impl CommitUser {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn email(&self) -> &str {
        &self.email
    }

    pub fn email_md5(&self) -> &str {
        &self.email_md5
    }

    pub fn time(&self) -> OffsetDateTime {
        self.time
    }
}

#[derive(Debug)]
pub struct Commit {
    author: CommitUser,
    committer: CommitUser,
    oid: String,
    tree: String,
    parents: Vec<String>,
    summary: String,
    body: String,
    pub diff_stats: String,
    pub diff: String,
}

impl From<git2::Commit<'_>> for Commit {
    fn from(commit: git2::Commit<'_>) -> Self {
        Commit {
            author: commit.author().into(),
            committer: commit.committer().into(),
            oid: commit.id().to_string(),
            tree: commit.tree_id().to_string(),
            parents: commit.parent_ids().map(|v| v.to_string()).collect(),
            summary: commit.summary().unwrap().to_string(),
            body: commit.body().map(ToString::to_string).unwrap_or_default(),
            diff_stats: String::with_capacity(0),
            diff: String::with_capacity(0),
        }
    }
}

impl Commit {
    pub fn author(&self) -> &CommitUser {
        &self.author
    }

    pub fn committer(&self) -> &CommitUser {
        &self.committer
    }

    pub fn oid(&self) -> &str {
        &self.oid
    }

    pub fn tree(&self) -> &str {
        &self.tree
    }

    pub fn parents(&self) -> impl Iterator<Item = &str> {
        self.parents.iter().map(String::as_str)
    }

    pub fn summary(&self) -> &str {
        &self.summary
    }

    pub fn body(&self) -> &str {
        &self.body
    }
}

#[instrument(skip(repo, commit, syntax_set))]
fn fetch_diff_and_stats(
    repo: &git2::Repository,
    commit: &git2::Commit<'_>,
    syntax_set: &SyntaxSet,
) -> (String, String) {
    let current_tree = commit.tree().unwrap();
    let parent_tree = commit.parents().next().and_then(|v| v.tree().ok());
    let mut diff_opts = DiffOptions::new();
    let diff = repo
        .diff_tree_to_tree(
            parent_tree.as_ref(),
            Some(&current_tree),
            Some(&mut diff_opts),
        )
        .unwrap();
    let diff_stats = diff
        .stats()
        .unwrap()
        .to_buf(DiffStatsFormat::FULL, 80)
        .unwrap()
        .as_str()
        .unwrap()
        .to_string();
    let diff_output = format_diff(&diff, &syntax_set);

    (diff_output, diff_stats)
}

fn format_file(content: &[u8], extension: &str, syntax_set: &SyntaxSet) -> String {
    let content = std::str::from_utf8(content).unwrap();

    let syntax = syntax_set
        .find_syntax_by_extension(&extension)
        .unwrap_or(syntax_set.find_syntax_plain_text());
    let mut html_generator =
        ClassedHTMLGenerator::new_with_class_style(syntax, &syntax_set, ClassStyle::Spaced);

    for line in LinesWithEndings::from(content) {
        html_generator
            .parse_html_for_line_which_includes_newline(line)
            .unwrap();
    }

    format!("<code>{}</code>", html_generator.finalize().replace('\n', "</code>\n<code>"))
}

#[instrument(skip(diff, syntax_set))]
fn format_diff(diff: &git2::Diff<'_>, syntax_set: &SyntaxSet) -> String {
    let mut diff_output = String::new();

    diff.print(DiffFormat::Patch, |delta, _diff_hunk, diff_line| {
        let (class, should_highlight_as_source) = match diff_line.origin_value() {
            DiffLineType::Addition => (Some("add-line"), true),
            DiffLineType::Deletion => (Some("remove-line"), true),
            DiffLineType::Context => (Some("context"), true),
            DiffLineType::AddEOFNL => (Some("remove-line"), false),
            DiffLineType::DeleteEOFNL => (Some("add-line"), false),
            DiffLineType::FileHeader => (Some("file-header"), false),
            _ => (None, false),
        };

        let line = std::str::from_utf8(diff_line.content()).unwrap();

        let extension = if should_highlight_as_source {
            let path = delta.new_file().path().unwrap();
            path.extension()
                .or(path.file_name())
                .unwrap()
                .to_string_lossy()
        } else {
            Cow::Borrowed("patch")
        };
        let syntax = syntax_set
            .find_syntax_by_extension(&extension)
            .unwrap_or(syntax_set.find_syntax_plain_text());
        let mut html_generator =
            ClassedHTMLGenerator::new_with_class_style(syntax, &syntax_set, ClassStyle::Spaced);
        html_generator
            .parse_html_for_line_which_includes_newline(line)
            .unwrap();
        if let Some(class) = class {
            write!(diff_output, r#"<span class="diff-{class}">"#).unwrap();
        }
        diff_output.push_str(&html_generator.finalize());
        if class.is_some() {
            diff_output.push_str("</span>");
        }

        true
    })
    .unwrap();

    diff_output
}

#[instrument(skip(repos))]
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
            last_modified: OffsetDateTime::from(last_modified),
        });
    }
}
