use std::{
    borrow::Cow,
    ffi::OsStr,
    fmt::Write,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{anyhow, Context, Result};
use axum::response::IntoResponse;
use bytes::{BufMut, Bytes, BytesMut};
use comrak::{ComrakPlugins, Options};
use git2::{
    DiffFormat, DiffLineType, DiffOptions, DiffStatsFormat, Email, EmailCreateOptions, ObjectType,
    Oid, Signature, TreeWalkResult,
};
use moka::future::Cache;
use parking_lot::Mutex;
use syntect::{
    html::{ClassStyle, ClassedHTMLGenerator},
    parsing::SyntaxSet,
    util::LinesWithEndings,
};
use time::OffsetDateTime;
use tracing::{error, instrument, warn};

use crate::syntax_highlight::ComrakSyntectAdapter;

type ReadmeCacheKey = (PathBuf, Option<Arc<str>>);

pub struct Git {
    commits: Cache<Oid, Arc<Commit>>,
    readme_cache: Cache<ReadmeCacheKey, Option<(ReadmeFormat, Arc<str>)>>,
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
            syntax_set,
        }
    }
}

impl Git {
    #[instrument(skip(self))]
    pub async fn repo(
        self: Arc<Self>,
        repo_path: PathBuf,
        branch: Option<Arc<str>>,
    ) -> Result<Arc<OpenRepository>> {
        let repo = tokio::task::spawn_blocking({
            let repo_path = repo_path.clone();
            move || git2::Repository::open(repo_path)
        })
        .await
        .context("Failed to join Tokio task")?
        .map_err(|err| {
            error!("{}", err);
            anyhow!("Failed to open repository")
        })?;

        Ok(Arc::new(OpenRepository {
            git: self,
            cache_key: repo_path,
            repo: Mutex::new(repo),
            branch,
        }))
    }
}

pub struct OpenRepository {
    git: Arc<Git>,
    cache_key: PathBuf,
    repo: Mutex<git2::Repository>,
    branch: Option<Arc<str>>,
}

impl OpenRepository {
    pub async fn path(
        self: Arc<Self>,
        path: Option<PathBuf>,
        tree_id: Option<&str>,
        formatted: bool,
    ) -> Result<PathDestination> {
        let tree_id = tree_id
            .map(Oid::from_str)
            .transpose()
            .context("Failed to parse tree hash")?;

        tokio::task::spawn_blocking(move || {
            let repo = self.repo.lock();

            let mut tree = if let Some(tree_id) = tree_id {
                repo.find_tree(tree_id)
                    .context("Couldn't find tree with given id")?
            } else if let Some(branch) = &self.branch {
                let reference = repo.resolve_reference_from_short_name(branch)?;
                reference
                    .peel_to_tree()
                    .context("Couldn't find tree for reference")?
            } else {
                let head = repo.head()?;
                head.peel_to_tree()
                    .context("Couldn't find tree from HEAD")?
            };

            if let Some(path) = path.as_ref() {
                let item = tree.get_path(path).context("Path doesn't exist in tree")?;
                let object = item
                    .to_object(&repo)
                    .context("Path in tree isn't an object")?;

                if let Some(blob) = object.as_blob() {
                    // TODO: use Path here instead of a lossy utf8 conv
                    let name = String::from_utf8_lossy(item.name_bytes());
                    let path = path.clone().join(&*name);

                    let extension = path
                        .extension()
                        .or_else(|| path.file_name())
                        .map_or_else(|| Cow::Borrowed(""), OsStr::to_string_lossy);
                    let content = match (formatted, blob.is_binary()) {
                        (true, true) => Content::Binary(vec![]),
                        (true, false) => Content::Text(
                            format_file(blob.content(), &extension, &self.git.syntax_set)?.into(),
                        ),
                        (false, true) => Content::Binary(blob.content().to_vec()),
                        (false, false) => Content::Text(
                            String::from_utf8_lossy(blob.content()).to_string().into(),
                        ),
                    };

                    return Ok(PathDestination::File(FileWithContent {
                        metadata: File {
                            mode: item.filemode(),
                            size: blob.size(),
                            path,
                            name: name.into_owned(),
                        },
                        content,
                    }));
                } else if let Ok(new_tree) = object.into_tree() {
                    tree = new_tree;
                } else {
                    anyhow::bail!("Given path not tree nor blob... what is it?!");
                }
            }

            let mut tree_items = Vec::new();

            for item in &tree {
                let object = item
                    .to_object(&repo)
                    .context("Expected item in tree to be object but it wasn't")?;

                let name = String::from_utf8_lossy(item.name_bytes()).into_owned();
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

            Ok(PathDestination::Tree(tree_items))
        })
        .await
        .context("Failed to join Tokio task")?
    }

    #[instrument(skip(self))]
    pub async fn tag_info(self: Arc<Self>) -> Result<DetailedTag> {
        tokio::task::spawn_blocking(move || {
            let tag_name = self.branch.clone().context("no tag given")?;
            let repo = self.repo.lock();

            let tag = repo
                .find_reference(&format!("refs/tags/{tag_name}"))
                .context("Given tag does not exist in repository")?
                .peel_to_tag()
                .context("Couldn't get to a tag from the given reference")?;
            let tag_target = tag.target().context("Couldn't find tagged object")?;

            let tagged_object = match tag_target.kind() {
                Some(ObjectType::Commit) => Some(TaggedObject::Commit(tag_target.id().to_string())),
                Some(ObjectType::Tree) => Some(TaggedObject::Tree(tag_target.id().to_string())),
                None | Some(_) => None,
            };

            Ok(DetailedTag {
                name: tag_name,
                tagger: tag.tagger().map(TryInto::try_into).transpose()?,
                message: tag
                    .message_bytes()
                    .map_or_else(|| Cow::Borrowed(""), String::from_utf8_lossy)
                    .into_owned(),
                tagged_object,
            })
        })
        .await
        .context("Failed to join Tokio task")?
    }

    #[instrument(skip(self))]
    pub async fn readme(
        self: Arc<Self>,
    ) -> Result<Option<(ReadmeFormat, Arc<str>)>, Arc<anyhow::Error>> {
        const README_FILES: &[&str] = &["README.md", "README", "README.txt"];

        let git = self.git.clone();

        git.readme_cache
            .try_get_with((self.cache_key.clone(), self.branch.clone()), async move {
                tokio::task::spawn_blocking(move || {
                    let repo = self.repo.lock();

                    let head = if let Some(reference) = &self.branch {
                        repo.resolve_reference_from_short_name(reference)?
                    } else {
                        repo.head().context("Couldn't find HEAD of repository")?
                    };

                    let commit = head.peel_to_commit().context(
                        "Couldn't find the commit that the HEAD of the repository refers to",
                    )?;
                    let tree = commit
                        .tree()
                        .context("Couldn't get the tree that the HEAD refers to")?;

                    for name in README_FILES {
                        let Some(tree_entry) = tree.get_name(name) else {
                            continue;
                        };

                        let Some(blob) = tree_entry
                            .to_object(&repo)
                            .ok()
                            .and_then(|v| v.into_blob().ok())
                        else {
                            continue;
                        };

                        let Ok(content) = std::str::from_utf8(blob.content()) else {
                            continue;
                        };

                        if Path::new(name).extension().and_then(OsStr::to_str) == Some("md") {
                            let value = parse_and_transform_markdown(content, &self.git.syntax_set);
                            return Ok(Some((ReadmeFormat::Markdown, Arc::from(value))));
                        }

                        return Ok(Some((ReadmeFormat::Plaintext, Arc::from(content))));
                    }

                    Ok(None)
                })
                .await
                .context("Failed to join Tokio task")?
            })
            .await
    }

    pub async fn default_branch(self: Arc<Self>) -> Result<Option<String>> {
        tokio::task::spawn_blocking(move || {
            let repo = self.repo.lock();
            let head = repo.head().context("Couldn't find HEAD of repository")?;
            Ok(head.shorthand().map(ToString::to_string))
        })
        .await
        .context("Failed to join Tokio task")?
    }

    #[instrument(skip(self))]
    pub async fn latest_commit(self: Arc<Self>) -> Result<Commit> {
        tokio::task::spawn_blocking(move || {
            let repo = self.repo.lock();

            let head = if let Some(reference) = &self.branch {
                repo.resolve_reference_from_short_name(reference)?
            } else {
                repo.head().context("Couldn't find HEAD of repository")?
            };

            let commit = head
                .peel_to_commit()
                .context("Couldn't find commit HEAD of repository refers to")?;
            let (diff_plain, diff_output, diff_stats) =
                fetch_diff_and_stats(&repo, &commit, &self.git.syntax_set)?;

            let mut commit = Commit::try_from(commit)?;
            commit.diff_stats = diff_stats;
            commit.diff = diff_output;
            commit.diff_plain = diff_plain;
            Ok(commit)
        })
        .await
        .context("Failed to join Tokio task")?
    }

    #[instrument(skip_all)]
    pub async fn archive(
        self: Arc<Self>,
        res: tokio::sync::mpsc::Sender<Result<Bytes, anyhow::Error>>,
        cont: tokio::sync::oneshot::Sender<()>,
        commit: Option<&str>,
    ) -> Result<(), anyhow::Error> {
        const BUFFER_CAP: usize = 512 * 1024;

        let commit = commit
            .map(Oid::from_str)
            .transpose()
            .context("failed to build oid")?;

        tokio::task::spawn_blocking(move || {
            let buffer = BytesMut::with_capacity(BUFFER_CAP + 1024);

            let flate = flate2::write::GzEncoder::new(buffer.writer(), flate2::Compression::fast());
            let mut archive = tar::Builder::new(flate);

            let repo = self.repo.lock();

            let tree = if let Some(commit) = commit {
                repo.find_commit(commit)?.tree()?
            } else if let Some(reference) = &self.branch {
                repo.resolve_reference_from_short_name(reference)?
                    .peel_to_tree()?
            } else {
                repo.head()
                    .context("Couldn't find HEAD of repository")?
                    .peel_to_tree()?
            };

            // tell the web server it can send response headers to the requester
            if cont.send(()).is_err() {
                return Err(anyhow!("requester gone"));
            }

            let mut callback = |root: &str, entry: &git2::TreeEntry| -> TreeWalkResult {
                if let Ok(blob) = entry.to_object(&repo).unwrap().peel_to_blob() {
                    let path =
                        Path::new(root).join(String::from_utf8_lossy(entry.name_bytes()).as_ref());

                    let mut header = tar::Header::new_gnu();
                    if let Err(error) = header.set_path(&path) {
                        warn!(%error, "Attempted to write invalid path to archive");
                        return TreeWalkResult::Skip;
                    }
                    header.set_size(blob.size() as u64);
                    #[allow(clippy::cast_sign_loss)]
                    header.set_mode(entry.filemode() as u32);
                    header.set_cksum();

                    if let Err(error) = archive.append(&header, blob.content()) {
                        error!(%error, "Failed to write blob to archive");
                        return TreeWalkResult::Abort;
                    }
                }

                if archive.get_ref().get_ref().get_ref().len() >= BUFFER_CAP {
                    let b = archive.get_mut().get_mut().get_mut().split().freeze();
                    if let Err(error) = res.blocking_send(Ok(b)) {
                        error!(%error, "Failed to send buffer to client");
                        return TreeWalkResult::Abort;
                    }
                }

                TreeWalkResult::Ok
            };

            tree.walk(git2::TreeWalkMode::PreOrder, &mut callback)?;

            res.blocking_send(Ok(archive.into_inner()?.finish()?.into_inner().freeze()))?;

            Ok::<_, anyhow::Error>(())
        })
        .await??;

        Ok(())
    }

    #[instrument(skip(self))]
    pub async fn commit(self: Arc<Self>, commit: &str) -> Result<Arc<Commit>, Arc<anyhow::Error>> {
        let commit = Oid::from_str(commit)
            .map_err(anyhow::Error::from)
            .map_err(Arc::new)?;

        let git = self.git.clone();

        git.commits
            .try_get_with(commit, async move {
                tokio::task::spawn_blocking(move || {
                    let repo = self.repo.lock();

                    let commit = repo.find_commit(commit)?;
                    let (diff_plain, diff_output, diff_stats) =
                        fetch_diff_and_stats(&repo, &commit, &self.git.syntax_set)?;

                    let mut commit = Commit::try_from(commit)?;
                    commit.diff_stats = diff_stats;
                    commit.diff = diff_output;
                    commit.diff_plain = diff_plain;

                    Ok(Arc::new(commit))
                })
                .await
                .context("Failed to join Tokio task")?
            })
            .await
    }
}

fn parse_and_transform_markdown(s: &str, syntax_set: &SyntaxSet) -> String {
    let mut plugins = ComrakPlugins::default();

    let highlighter = ComrakSyntectAdapter { syntax_set };
    plugins.render.codefence_syntax_highlighter = Some(&highlighter);

    // enable gfm extensions
    // https://github.github.com/gfm/
    let mut options = Options::default();
    options.extension.autolink = true;
    options.extension.footnotes = true;
    options.extension.strikethrough = true;
    options.extension.table = true;
    options.extension.tagfilter = true;
    options.extension.tasklist = true;

    comrak::markdown_to_html_with_plugins(s, &options, &plugins)
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ReadmeFormat {
    Markdown,
    Plaintext,
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
    pub content: Content,
}

#[derive(Debug)]
pub enum Content {
    Text(Cow<'static, str>),
    Binary(Vec<u8>),
}

impl IntoResponse for Content {
    fn into_response(self) -> axum::response::Response {
        use axum::http;

        match self {
            Self::Text(t) => {
                let headers = [(
                    http::header::CONTENT_TYPE,
                    http::HeaderValue::from_static("text/plain; charset=UTF-8"),
                )];

                (headers, t).into_response()
            }
            Self::Binary(b) => {
                let headers = [(
                    http::header::CONTENT_TYPE,
                    http::HeaderValue::from_static("application/octet-stream"),
                )];

                (headers, b).into_response()
            }
        }
    }
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
    pub name: Arc<str>,
    pub tagger: Option<CommitUser>,
    pub message: String,
    pub tagged_object: Option<TaggedObject>,
}

#[derive(Debug)]
pub struct CommitUser {
    name: String,
    email: String,
    time: OffsetDateTime,
}

impl TryFrom<Signature<'_>> for CommitUser {
    type Error = anyhow::Error;

    fn try_from(v: Signature<'_>) -> Result<Self> {
        Ok(CommitUser {
            name: String::from_utf8_lossy(v.name_bytes()).into_owned(),
            email: String::from_utf8_lossy(v.email_bytes()).into_owned(),
            time: OffsetDateTime::from_unix_timestamp(v.when().seconds())?,
        })
    }
}

impl CommitUser {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn email(&self) -> &str {
        &self.email
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
    pub diff_plain: Bytes,
}

impl TryFrom<git2::Commit<'_>> for Commit {
    type Error = anyhow::Error;

    fn try_from(commit: git2::Commit<'_>) -> Result<Self> {
        Ok(Commit {
            author: CommitUser::try_from(commit.author())?,
            committer: CommitUser::try_from(commit.committer())?,
            oid: commit.id().to_string(),
            tree: commit.tree_id().to_string(),
            parents: commit.parent_ids().map(|v| v.to_string()).collect(),
            summary: commit
                .summary_bytes()
                .map_or_else(|| Cow::Borrowed(""), String::from_utf8_lossy)
                .into_owned(),
            body: commit
                .body_bytes()
                .map_or_else(|| Cow::Borrowed(""), String::from_utf8_lossy)
                .into_owned(),
            diff_stats: String::with_capacity(0),
            diff: String::with_capacity(0),
            diff_plain: Bytes::new(),
        })
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
) -> Result<(Bytes, String, String)> {
    let current_tree = commit.tree().context("Couldn't get tree for the commit")?;
    let parent_tree = commit.parents().next().and_then(|v| v.tree().ok());
    let mut diff_opts = DiffOptions::new();
    let diff = repo.diff_tree_to_tree(
        parent_tree.as_ref(),
        Some(&current_tree),
        Some(&mut diff_opts),
    )?;

    let mut diff_plain = BytesMut::new();
    let email = Email::from_diff(
        &diff,
        1,
        1,
        &commit.id(),
        commit.summary().unwrap_or(""),
        commit.body().unwrap_or(""),
        &commit.author(),
        &mut EmailCreateOptions::default(),
    )
    .context("Couldn't build diff for commit")?;
    diff_plain.extend_from_slice(email.as_slice());

    let diff_stats = diff
        .stats()?
        .to_buf(DiffStatsFormat::FULL, 80)?
        .as_str()
        .unwrap_or("")
        .to_string();
    let diff_output = format_diff(&diff, syntax_set)?;

    Ok((diff_plain.freeze(), diff_output, diff_stats))
}

fn format_file(content: &[u8], extension: &str, syntax_set: &SyntaxSet) -> Result<String> {
    let content = String::from_utf8_lossy(content);

    let syntax = syntax_set
        .find_syntax_by_extension(extension)
        .unwrap_or_else(|| syntax_set.find_syntax_plain_text());
    let mut html_generator =
        ClassedHTMLGenerator::new_with_class_style(syntax, syntax_set, ClassStyle::Spaced);

    for line in LinesWithEndings::from(&content) {
        html_generator
            .parse_html_for_line_which_includes_newline(line)
            .context("Couldn't parse line of file")?;
    }

    Ok(format!(
        "<code>{}</code>",
        html_generator.finalize().replace('\n', "</code>\n<code>")
    ))
}

#[instrument(skip(diff, syntax_set))]
fn format_diff(diff: &git2::Diff<'_>, syntax_set: &SyntaxSet) -> Result<String> {
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

        let line = String::from_utf8_lossy(diff_line.content());

        let extension = if should_highlight_as_source {
            if let Some(path) = delta.new_file().path() {
                path.extension()
                    .or_else(|| path.file_name())
                    .map_or_else(|| Cow::Borrowed(""), OsStr::to_string_lossy)
            } else {
                Cow::Borrowed("")
            }
        } else {
            Cow::Borrowed("patch")
        };
        let syntax = syntax_set
            .find_syntax_by_extension(&extension)
            .unwrap_or_else(|| syntax_set.find_syntax_plain_text());
        let mut html_generator =
            ClassedHTMLGenerator::new_with_class_style(syntax, syntax_set, ClassStyle::Spaced);
        let _res = html_generator.parse_html_for_line_which_includes_newline(&line);
        if let Some(class) = class {
            let _ = write!(diff_output, r#"<span class="diff-{class}">"#);
        }
        diff_output.push_str(&html_generator.finalize());
        if class.is_some() {
            diff_output.push_str("</span>");
        }

        true
    })
    .context("Failed to prepare diff")?;

    Ok(diff_output)
}
