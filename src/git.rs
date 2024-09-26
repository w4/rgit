use anyhow::{anyhow, Context, Result};
use axum::response::IntoResponse;
use bytes::buf::Writer;
use bytes::{BufMut, Bytes, BytesMut};
use comrak::{ComrakPlugins, Options};
use flate2::write::GzEncoder;
use gix::{
    actor::SignatureRef,
    bstr::{BStr, BString, ByteSlice, ByteVec},
    diff::blob::{platform::prepare_diff::Operation, Sink},
    object::Kind,
    objs::tree::EntryRef,
    prelude::TreeEntryRefExt,
    traverse::tree::visit::Action,
    ObjectId,
};
use moka::future::Cache;
use parking_lot::Mutex;
use std::{
    borrow::Cow,
    collections::{BTreeMap, VecDeque},
    ffi::OsStr,
    fmt::{self, Arguments, Write},
    path::{Path, PathBuf},
    str::FromStr,
    sync::Arc,
    time::Duration,
};
use syntect::{
    parsing::SyntaxSet,
    parsing::{BasicScopeStackOp, ParseState, Scope, ScopeStack, SCOPE_REPO},
    util::LinesWithEndings,
};
use tar::Builder;
use time::{OffsetDateTime, UtcOffset};
use tracing::{error, instrument, warn};

use crate::{
    syntax_highlight::ComrakSyntectAdapter,
    unified_diff_builder::{Callback, UnifiedDiffBuilder},
};

type ReadmeCacheKey = (PathBuf, Option<Arc<str>>);

pub struct Git {
    commits: Cache<(ObjectId, bool), Arc<Commit>>,
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
        let mut repo = tokio::task::spawn_blocking({
            let repo_path = repo_path.clone();
            move || gix::open(repo_path)
        })
        .await
        .context("Failed to join Tokio task")?
        .map_err(|err| {
            error!("{}", err);
            anyhow!("Failed to open repository")
        })?;

        repo.object_cache_size(10 * 1024 * 1024);

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
    repo: Mutex<gix::Repository>,
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
            .map(ObjectId::from_str)
            .transpose()
            .context("Failed to parse tree hash")?;

        tokio::task::spawn_blocking(move || {
            let repo = self.repo.lock();

            let mut tree = if let Some(tree_id) = tree_id {
                repo.find_tree(tree_id)
                    .context("Couldn't find tree with given id")?
            } else if let Some(branch) = &self.branch {
                repo.find_reference(branch.as_ref())?
                    .peel_to_tree()
                    .context("Couldn't find tree for reference")?
            } else {
                repo.find_reference("HEAD")
                    .context("Failed to find HEAD")?
                    .peel_to_tree()
                    .context("Couldn't find HEAD for reference")?
            };

            if let Some(path) = path.as_ref() {
                let item = tree
                    .peel_to_entry_by_path(path)?
                    .context("Path doesn't exist in tree")?;
                let object = item.object().context("Path in tree isn't an object")?;

                match object.kind {
                    Kind::Blob => {
                        let path = path.join(item.filename().to_path_lossy());
                        let mut blob = object.into_blob();

                        let size = blob.data.len();
                        let extension = path
                            .extension()
                            .or_else(|| path.file_name())
                            .map_or_else(|| Cow::Borrowed(""), OsStr::to_string_lossy);

                        let content = match (formatted, String::from_utf8(blob.take_data())) {
                            (true, Err(_)) => Content::Binary(vec![]),
                            (true, Ok(data)) => Content::Text(Cow::Owned(format_file(
                                &data,
                                &extension,
                                &self.git.syntax_set,
                            )?)),
                            (false, Err(e)) => Content::Binary(e.into_bytes()),
                            (false, Ok(data)) => Content::Text(Cow::Owned(data)),
                        };

                        return Ok(PathDestination::File(FileWithContent {
                            metadata: File {
                                mode: item.mode().0,
                                size,
                                path: path.clone(),
                                name: item.filename().to_string(),
                            },
                            content,
                        }));
                    }
                    Kind::Tree => {
                        tree = object.into_tree();
                    }
                    _ => anyhow::bail!("bad object of type {:?}", object.kind),
                }
            }

            let mut tree_items = Vec::new();

            for item in tree.iter() {
                let item = item?;
                let object = item
                    .object()
                    .context("Expected item in tree to be object but it wasn't")?;

                let path = path
                    .clone()
                    .unwrap_or_default()
                    .join(item.filename().to_path_lossy());

                tree_items.push(match object.kind {
                    Kind::Blob => TreeItem::File(File {
                        mode: item.mode().0,
                        size: object.into_blob().data.len(),
                        path,
                        name: item.filename().to_string(),
                    }),
                    Kind::Tree => TreeItem::Tree(Tree {
                        mode: item.mode().0,
                        path,
                        name: item.filename().to_string(),
                    }),
                    _ => continue,
                });
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
            let tag_target = tag
                .target_id()
                .context("Couldn't find tagged object")?
                .object()?;

            let tagged_object = match tag_target.kind {
                Kind::Commit => Some(TaggedObject::Commit(tag_target.id.to_string())),
                Kind::Tree => Some(TaggedObject::Tree(tag_target.id.to_string())),
                _ => None,
            };

            let tag_info = tag.decode()?;

            Ok(DetailedTag {
                name: tag_name,
                tagger: tag_info.tagger.map(TryInto::try_into).transpose()?,
                message: tag_info.message.to_string(),
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

                    let mut head = if let Some(reference) = &self.branch {
                        repo.find_reference(reference.as_ref())?
                    } else {
                        repo.find_reference("HEAD")
                            .context("Couldn't find HEAD of repository")?
                    };

                    let commit = head.peel_to_commit().context(
                        "Couldn't find the commit that the HEAD of the repository refers to",
                    )?;
                    let mut tree = commit
                        .tree()
                        .context("Couldn't get the tree that the HEAD refers to")?;

                    for name in README_FILES {
                        let Some(tree_entry) = tree.peel_to_entry_by_path(name)? else {
                            continue;
                        };

                        let Some(blob) = tree_entry
                            .object()
                            .ok()
                            .and_then(|v| v.try_into_blob().ok())
                        else {
                            continue;
                        };

                        let Ok(content) = std::str::from_utf8(&blob.data) else {
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
            Ok(head.referent_name().map(|v| v.shorten().to_string()))
        })
        .await
        .context("Failed to join Tokio task")?
    }

    #[instrument(skip(self))]
    pub async fn latest_commit(self: Arc<Self>, highlighted: bool) -> Result<Commit> {
        tokio::task::spawn_blocking(move || {
            let repo = self.repo.lock();

            let mut head = if let Some(reference) = &self.branch {
                repo.find_reference(reference.as_ref())?
            } else {
                repo.find_reference("HEAD")
                    .context("Couldn't find HEAD of repository")?
            };

            let commit = head
                .peel_to_commit()
                .context("Couldn't find commit HEAD of repository refers to")?;
            let (diff_output, diff_stats) =
                fetch_diff_and_stats(&repo, &commit, highlighted.then_some(&self.git.syntax_set))?;

            let mut commit = Commit::try_from(commit)?;
            commit.diff_stats = diff_stats;
            commit.diff = diff_output;
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
        let commit = commit
            .map(ObjectId::from_str)
            .transpose()
            .context("failed to build oid")?;

        tokio::task::spawn_blocking(move || {
            let repo = self.repo.lock();

            let tree = if let Some(commit) = commit {
                repo.find_commit(commit)?.tree()?
            } else if let Some(reference) = &self.branch {
                repo.find_reference(reference.as_ref())?.peel_to_tree()?
            } else {
                repo.find_reference("HEAD")
                    .context("Couldn't find HEAD of repository")?
                    .peel_to_tree()?
            };

            // tell the web server it can send response headers to the requester
            if cont.send(()).is_err() {
                return Err(anyhow!("requester gone"));
            }

            let buffer = BytesMut::with_capacity(BUFFER_CAP + 1024);
            let mut visitor = ArchivalVisitor {
                repository: &repo,
                res,
                archive: Builder::new(GzEncoder::new(buffer.writer(), flate2::Compression::fast())),
                path_deque: VecDeque::new(),
                path: BString::default(),
            };

            tree.traverse().breadthfirst(&mut visitor)?;

            visitor.res.blocking_send(Ok(visitor
                .archive
                .into_inner()?
                .finish()?
                .into_inner()
                .freeze()))?;

            Ok::<_, anyhow::Error>(())
        })
        .await??;

        Ok(())
    }

    #[instrument(skip(self))]
    pub async fn commit(
        self: Arc<Self>,
        commit: &str,
        highlighted: bool,
    ) -> Result<Arc<Commit>, Arc<anyhow::Error>> {
        let commit = ObjectId::from_str(commit)
            .map_err(anyhow::Error::from)
            .map_err(Arc::new)?;

        let git = self.git.clone();

        git.commits
            .try_get_with((commit, highlighted), async move {
                tokio::task::spawn_blocking(move || {
                    let repo = self.repo.lock();

                    let commit = repo.find_commit(commit)?;

                    let (diff_output, diff_stats) = fetch_diff_and_stats(
                        &repo,
                        &commit,
                        highlighted.then_some(&self.git.syntax_set),
                    )?;

                    let mut commit = Commit::try_from(commit)?;
                    commit.diff_stats = diff_stats;
                    commit.diff = diff_output;

                    Ok(Arc::new(commit))
                })
                .await
                .context("Failed to join Tokio task")?
            })
            .await
    }
}

const BUFFER_CAP: usize = 512 * 1024;

pub struct ArchivalVisitor<'a> {
    repository: &'a gix::Repository,
    res: tokio::sync::mpsc::Sender<Result<Bytes, anyhow::Error>>,
    archive: Builder<GzEncoder<Writer<BytesMut>>>,
    path_deque: VecDeque<BString>,
    path: BString,
}

impl<'a> ArchivalVisitor<'a> {
    fn pop_element(&mut self) {
        if let Some(pos) = self.path.rfind_byte(b'/') {
            self.path.resize(pos, 0);
        } else {
            self.path.clear();
        }
    }

    fn push_element(&mut self, name: &BStr) {
        if !self.path.is_empty() {
            self.path.push(b'/');
        }
        self.path.push_str(name);
    }
}

impl<'a> gix::traverse::tree::Visit for ArchivalVisitor<'a> {
    fn pop_front_tracked_path_and_set_current(&mut self) {
        self.path = self
            .path_deque
            .pop_front()
            .expect("every call is matched with push_tracked_path_component");
    }

    fn push_back_tracked_path_component(&mut self, component: &BStr) {
        self.push_element(component);
        self.path_deque.push_back(self.path.clone());
    }

    fn push_path_component(&mut self, component: &BStr) {
        self.push_element(component);
    }

    fn pop_path_component(&mut self) {
        self.pop_element();
    }

    fn visit_tree(&mut self, _entry: &EntryRef<'_>) -> Action {
        Action::Continue
    }

    fn visit_nontree(&mut self, entry: &EntryRef<'_>) -> Action {
        let entry = entry.attach(self.repository);

        let Ok(object) = entry.object() else {
            return Action::Continue;
        };

        if object.kind != Kind::Blob {
            return Action::Continue;
        }

        let blob = object.into_blob();

        let mut header = tar::Header::new_gnu();
        if let Err(error) = header.set_path(self.path.to_path_lossy()) {
            warn!(%error, "Attempted to write invalid path to archive");
            return Action::Continue;
        }
        header.set_size(blob.data.len() as u64);
        #[allow(clippy::cast_sign_loss)]
        header.set_mode(entry.mode().0.into());
        header.set_cksum();

        if let Err(error) = self.archive.append(&header, blob.data.as_slice()) {
            warn!(%error, "Failed to append to archive");
            return Action::Cancel;
        }

        if self.archive.get_ref().get_ref().get_ref().len() >= BUFFER_CAP {
            let b = self.archive.get_mut().get_mut().get_mut().split().freeze();

            if self.res.blocking_send(Ok(b)).is_err() {
                return Action::Cancel;
            }
        }

        Action::Continue
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
    pub mode: u16,
    pub name: String,
    pub path: PathBuf,
}

#[derive(Debug)]
pub struct File {
    pub mode: u16,
    pub size: usize,
    pub name: String,
    pub path: PathBuf,
}

#[derive(Debug)]
#[allow(unused)]
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

impl TryFrom<SignatureRef<'_>> for CommitUser {
    type Error = anyhow::Error;

    fn try_from(v: SignatureRef<'_>) -> Result<Self> {
        Ok(CommitUser {
            name: v.name.to_string(),
            email: v.email.to_string(),
            time: OffsetDateTime::from_unix_timestamp(v.time.seconds)?
                .to_offset(UtcOffset::from_whole_seconds(v.time.offset)?),
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
}

impl TryFrom<gix::Commit<'_>> for Commit {
    type Error = anyhow::Error;

    fn try_from(commit: gix::Commit<'_>) -> Result<Self> {
        let message = commit.message()?;

        Ok(Commit {
            author: CommitUser::try_from(commit.author()?)?,
            committer: CommitUser::try_from(commit.committer()?)?,
            oid: commit.id().to_string(),
            tree: commit.tree_id()?.to_string(),
            parents: commit.parent_ids().map(|v| v.to_string()).collect(),
            summary: message.summary().to_string(),
            body: message.body.map_or_else(String::new, ToString::to_string),
            diff_stats: String::with_capacity(0),
            diff: String::with_capacity(0),
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
    repo: &gix::Repository,
    commit: &gix::Commit<'_>,
    syntax_set: Option<&SyntaxSet>,
) -> Result<(String, String)> {
    const WIDTH: usize = 80;

    let current_tree = commit.tree().context("Couldn't get tree for the commit")?;
    let parent_tree = commit
        .ancestors()
        .first_parent_only()
        .all()?
        .nth(1)
        .transpose()?
        .map(|v| v.object())
        .transpose()?
        .map(|v| v.tree())
        .transpose()?
        .unwrap_or_else(|| repo.empty_tree());

    let mut diffs = BTreeMap::<_, FileDiff>::new();
    let mut diff_output = String::new();

    let mut resource_cache = repo.diff_resource_cache_for_tree_diff()?;

    let mut changes = parent_tree.changes()?;
    changes.track_path().track_rewrites(None);
    changes.for_each_to_obtain_tree_with_cache(
        &current_tree,
        &mut repo.diff_resource_cache_for_tree_diff()?,
        |change| {
            if let Some(syntax_set) = syntax_set {
                DiffBuilder {
                    output: &mut diff_output,
                    resource_cache: &mut resource_cache,
                    diffs: &mut diffs,
                    formatter: SyntaxHighlightedDiffFormatter::new(
                        change.location.to_path().unwrap(),
                        syntax_set,
                    ),
                }
                .handle(change)
            } else {
                DiffBuilder {
                    output: &mut diff_output,
                    resource_cache: &mut resource_cache,
                    diffs: &mut diffs,
                    formatter: PlainDiffFormatter,
                }
                .handle(change)
            }
        },
    )?;

    let (max_file_name_length, max_change_length, files_changed, insertions, deletions) =
        diffs.iter().fold(
            (0, 0, 0, 0, 0),
            |(max_file_name_length, max_change_length, files_changed, insertions, deletions),
             (f, stats)| {
                (
                    max_file_name_length.max(f.len()),
                    max_change_length
                        .max(((stats.insertions + stats.deletions).ilog10() + 1) as usize),
                    files_changed + 1,
                    insertions + stats.insertions,
                    deletions + stats.deletions,
                )
            },
        );

    let mut diff_stats = String::new();

    let total_changes = insertions + deletions;

    for (file, diff) in &diffs {
        let local_changes = diff.insertions + diff.deletions;
        let width = WIDTH.min(local_changes);

        // Calculate proportions of `+` and `-` within the total width
        let addition_width = (width * diff.insertions) / total_changes;
        let deletion_width = (width * diff.deletions) / total_changes;

        // Handle edge case where total width is less than total changes
        let remaining_width = width - (addition_width + deletion_width);
        let adjusted_addition_width = addition_width + remaining_width.min(diff.insertions);
        let adjusted_deletion_width =
            deletion_width + (remaining_width - remaining_width.min(diff.insertions));

        // Generate the string representation
        let plus_str = "+".repeat(adjusted_addition_width);
        let minus_str = "-".repeat(adjusted_deletion_width);

        writeln!(diff_stats, " {file:max_file_name_length$} | {local_changes:max_change_length$} {plus_str}{minus_str}").unwrap();
    }

    for (i, (singular_desc, plural_desc, amount)) in [
        ("file changed", "files changed", files_changed),
        ("insertion(+)", "insertions(+)", insertions),
        ("deletion(-)", "deletions(-)", deletions),
    ]
    .into_iter()
    .enumerate()
    {
        if amount == 0 {
            continue;
        }

        let prefix = if i == 0 { "" } else { "," };

        let desc = if amount == 1 {
            singular_desc
        } else {
            plural_desc
        };

        write!(diff_stats, "{prefix} {amount} {desc}")?;
    }

    // TODO: emit 'create mode 100644 pure-black-background-f82588d3.jpg' here

    writeln!(diff_stats)?;

    Ok((diff_output, diff_stats))
}

#[derive(Default, Debug)]
struct FileDiff {
    insertions: usize,
    deletions: usize,
}

fn format_file(content: &str, extension: &str, syntax_set: &SyntaxSet) -> Result<String> {
    let mut out = String::new();
    format_file_inner(&mut out, content, extension, syntax_set, true)?;
    Ok(out)
}

// TODO: this is in some serious need of refactoring
fn format_file_inner(
    out: &mut String,
    content: &str,
    extension: &str,
    syntax_set: &SyntaxSet,
    code_tag: bool,
) -> Result<()> {
    let syntax = syntax_set
        .find_syntax_by_extension(extension)
        .unwrap_or_else(|| syntax_set.find_syntax_plain_text());
    let mut parse_state = ParseState::new(syntax);

    let mut scope_stack = ScopeStack::new();
    let mut span_empty = false;
    let mut span_start = 0;
    let mut open_spans = Vec::new();

    for line in LinesWithEndings::from(content) {
        if code_tag {
            out.push_str("<code>");
        }

        if line.len() > 2048 {
            // avoid highlighting overly complex lines
            let line = if code_tag { line.trim_end() } else { line };
            write!(out, "{}", Escape(line))?;
        } else {
            let mut cur_index = 0;
            let ops = parse_state.parse_line(line, syntax_set)?;
            out.reserve(line.len() + ops.len() * 8);

            if code_tag {
                for scope in &open_spans {
                    out.push_str("<span class=\"");
                    scope_to_classes(out, *scope);
                    out.push_str("\">");
                }
            }

            // mostly copied from syntect, but slightly modified to keep track
            // of open spans, so we can open and close them for each line
            for &(i, ref op) in &ops {
                if i > cur_index {
                    let prefix = &line[cur_index..i];
                    let prefix = if code_tag {
                        prefix.trim_end_matches('\n')
                    } else {
                        prefix
                    };
                    write!(out, "{}", Escape(prefix))?;

                    span_empty = false;
                    cur_index = i;
                }

                scope_stack.apply_with_hook(op, |basic_op, _| match basic_op {
                    BasicScopeStackOp::Push(scope) => {
                        span_start = out.len();
                        span_empty = true;
                        out.push_str("<span class=\"");
                        open_spans.push(scope);
                        scope_to_classes(out, scope);
                        out.push_str("\">");
                    }
                    BasicScopeStackOp::Pop => {
                        open_spans.pop();
                        if span_empty {
                            out.truncate(span_start);
                        } else {
                            out.push_str("</span>");
                        }
                        span_empty = false;
                    }
                })?;
            }

            let line = if code_tag { line.trim_end() } else { line };
            if line.len() > cur_index {
                write!(out, "{}", Escape(&line[cur_index..]))?;
            }

            if code_tag {
                for _scope in &open_spans {
                    out.push_str("</span>");
                }
            }
        }

        if code_tag {
            out.push_str("</code>\n");
        }
    }

    if !code_tag {
        for _scope in &open_spans {
            out.push_str("</span>");
        }
    }

    Ok(())
}

fn scope_to_classes(s: &mut String, scope: Scope) {
    let repo = SCOPE_REPO.lock().unwrap();
    for i in 0..(scope.len()) {
        let atom = scope.atom_at(i as usize);
        let atom_s = repo.atom_str(atom);
        if i != 0 {
            s.push(' ');
        }
        s.push_str(atom_s);
    }
}

// Copied from syntect as it isn't exposed from there.
pub struct Escape<'a>(pub &'a str);

impl<'a> fmt::Display for Escape<'a> {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Escape(s) = *self;
        let pile_o_bits = s;
        let mut last = 0;
        for (i, ch) in s.bytes().enumerate() {
            match ch as char {
                '<' | '>' | '&' | '\'' | '"' => {
                    fmt.write_str(&pile_o_bits[last..i])?;
                    let s = match ch as char {
                        '>' => "&gt;",
                        '<' => "&lt;",
                        '&' => "&amp;",
                        '\'' => "&#39;",
                        '"' => "&quot;",
                        _ => unreachable!(),
                    };
                    fmt.write_str(s)?;
                    last = i + 1;
                }
                _ => {}
            }
        }

        if last < s.len() {
            fmt.write_str(&pile_o_bits[last..])?;
        }
        Ok(())
    }
}

trait DiffFormatter {
    fn file_header(&self, output: &mut String, data: fmt::Arguments<'_>);

    fn binary(
        &self,
        output: &mut String,
        left: &str,
        right: &str,
        left_content: &[u8],
        right_content: &[u8],
    );
}

struct DiffBuilder<'a, F> {
    output: &'a mut String,
    resource_cache: &'a mut gix::diff::blob::Platform,
    diffs: &'a mut BTreeMap<String, FileDiff>,
    formatter: F,
}

impl<'a, F: DiffFormatter + Callback> DiffBuilder<'a, F> {
    fn handle(
        &mut self,
        change: gix::object::tree::diff::Change<'_, '_, '_>,
    ) -> Result<gix::object::tree::diff::Action> {
        if !change.event.entry_mode().is_blob_or_symlink() {
            return Ok(gix::object::tree::diff::Action::Continue);
        }

        let diff = self.diffs.entry(change.location.to_string()).or_default();
        let change = change.diff(self.resource_cache)?;

        let prep = change.resource_cache.prepare_diff()?;

        self.formatter.file_header(
            self.output,
            format_args!(
                "diff --git a/{} b/{}",
                prep.old.rela_path, prep.new.rela_path
            ),
        );

        if prep.old.id.is_null() {
            self.formatter.file_header(
                self.output,
                format_args!("new file mode {}", prep.new.mode.as_octal_str()),
            );
        } else if prep.new.id.is_null() {
            self.formatter.file_header(
                self.output,
                format_args!("deleted file mode {}", prep.old.mode.as_octal_str()),
            );
        } else if prep.new.mode != prep.old.mode {
            self.formatter.file_header(
                self.output,
                format_args!("old mode {}", prep.old.mode.as_octal_str()),
            );
            self.formatter.file_header(
                self.output,
                format_args!("new mode {}", prep.new.mode.as_octal_str()),
            );
        }

        // copy from
        // copy to
        // rename old
        // rename new
        // rename from
        // rename to
        // similarity index
        // dissimilarity index

        let (index_suffix_sep, index_suffix) = if prep.old.mode == prep.new.mode {
            (" ", prep.new.mode.as_octal_str())
        } else {
            ("", BStr::new(&[]))
        };

        let old_path = if prep.old.id.is_null() {
            Cow::Borrowed("/dev/null")
        } else {
            Cow::Owned(format!("a/{}", prep.old.rela_path))
        };

        let new_path = if prep.new.id.is_null() {
            Cow::Borrowed("/dev/null")
        } else {
            Cow::Owned(format!("a/{}", prep.new.rela_path))
        };

        match prep.operation {
            Operation::InternalDiff { algorithm } => {
                self.formatter.file_header(
                    self.output,
                    format_args!(
                        "index {}..{}{index_suffix_sep}{index_suffix}",
                        prep.old.id.to_hex_with_len(7),
                        prep.new.id.to_hex_with_len(7)
                    ),
                );
                self.formatter
                    .file_header(self.output, format_args!("--- {old_path}"));
                self.formatter
                    .file_header(self.output, format_args!("+++ {new_path}"));

                let old_source = gix::diff::blob::sources::lines_with_terminator(
                    std::str::from_utf8(prep.old.data.as_slice().unwrap_or_default())?,
                );
                let new_source = gix::diff::blob::sources::lines_with_terminator(
                    std::str::from_utf8(prep.new.data.as_slice().unwrap_or_default())?,
                );
                let input = gix::diff::blob::intern::InternedInput::new(old_source, new_source);

                let output = gix::diff::blob::diff(
                    algorithm,
                    &input,
                    UnifiedDiffBuilder::with_writer(&input, &mut *self.output, &mut self.formatter)
                        .with_counter(),
                );

                diff.deletions += output.removals as usize;
                diff.insertions += output.insertions as usize;
            }
            Operation::ExternalCommand { .. } => {}
            Operation::SourceOrDestinationIsBinary => {
                self.formatter.file_header(
                    self.output,
                    format_args!(
                        "index {}..{}{index_suffix_sep}{index_suffix}",
                        prep.old.id, prep.new.id,
                    ),
                );

                self.formatter.binary(
                    self.output,
                    old_path.as_ref(),
                    new_path.as_ref(),
                    prep.old.data.as_slice().unwrap_or_default(),
                    prep.new.data.as_slice().unwrap_or_default(),
                );
            }
        }

        self.resource_cache.clear_resource_cache_keep_allocation();
        Ok(gix::object::tree::diff::Action::Continue)
    }
}

struct PlainDiffFormatter;

impl DiffFormatter for PlainDiffFormatter {
    fn file_header(&self, output: &mut String, data: fmt::Arguments<'_>) {
        writeln!(output, "{data}").unwrap();
    }

    fn binary(
        &self,
        output: &mut String,
        left: &str,
        right: &str,
        _left_content: &[u8],
        _right_content: &[u8],
    ) {
        // todo: actually perform the diff and write a `GIT binary patch` out
        writeln!(output, "Binary files {left} and {right} differ").unwrap();
    }
}

impl Callback for PlainDiffFormatter {
    fn addition(&mut self, data: &str, dst: &mut String) {
        write!(dst, "+{data}").unwrap();
    }

    fn remove(&mut self, data: &str, dst: &mut String) {
        write!(dst, "-{data}").unwrap();
    }

    fn context(&mut self, data: &str, dst: &mut String) {
        write!(dst, " {data}").unwrap();
    }
}

struct SyntaxHighlightedDiffFormatter<'a> {
    syntax_set: &'a SyntaxSet,
    extension: Cow<'a, str>,
}

impl<'a> SyntaxHighlightedDiffFormatter<'a> {
    fn new(path: &'a Path, syntax_set: &'a SyntaxSet) -> Self {
        let extension = path
            .extension()
            .or_else(|| path.file_name())
            .map_or_else(|| Cow::Borrowed(""), OsStr::to_string_lossy);

        Self {
            syntax_set,
            extension,
        }
    }

    fn write(&self, output: &mut String, class: &str, data: &str) {
        write!(output, r#"<span class="diff-{class}">"#).unwrap();
        format_file_inner(
            output,
            data,
            self.extension.as_ref(),
            self.syntax_set,
            false,
        )
        .unwrap();
        write!(output, r#"</span>"#).unwrap();
    }
}

impl<'a> DiffFormatter for SyntaxHighlightedDiffFormatter<'a> {
    fn file_header(&self, output: &mut String, data: Arguments<'_>) {
        write!(output, r#"<span class="diff-file-header">"#).unwrap();
        format_file_inner(output, &data.to_string(), "patch", self.syntax_set, false).unwrap();
        writeln!(output, r#"</span>"#).unwrap();
    }

    fn binary(
        &self,
        output: &mut String,
        left: &str,
        right: &str,
        _left_content: &[u8],
        _right_content: &[u8],
    ) {
        format_file_inner(
            output,
            &format!("Binary files {left} and {right} differ"),
            "patch",
            self.syntax_set,
            false,
        )
        .unwrap();
    }
}

impl<'a> Callback for SyntaxHighlightedDiffFormatter<'a> {
    fn addition(&mut self, data: &str, dst: &mut String) {
        self.write(dst, "add-line", data);
    }

    fn remove(&mut self, data: &str, dst: &mut String) {
        self.write(dst, "remove-line", data);
    }

    fn context(&mut self, data: &str, dst: &mut String) {
        self.write(dst, "context", data);
    }
}
