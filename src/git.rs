use anyhow::{anyhow, Context, Result};
use axum::response::IntoResponse;
use bytes::{buf::Writer, BufMut, Bytes, BytesMut};
use comrak::{ComrakPlugins, Options};
use flate2::write::GzEncoder;
use gix::{
    actor::SignatureRef,
    bstr::{BStr, BString, ByteSlice, ByteVec},
    diff::blob::{platform::prepare_diff::Operation, Sink},
    object::Kind,
    objs::{tree::EntryRef, CommitRef, TagRef},
    prelude::TreeEntryRefExt,
    traverse::tree::visit::Action,
    ObjectId, ThreadSafeRepository,
};
use itertools::Either;
use moka::future::Cache;
use std::{
    borrow::Cow,
    collections::VecDeque,
    ffi::OsStr,
    fmt::{self, Arguments, Write},
    io::ErrorKind,
    iter::Copied,
    path::{Path, PathBuf},
    str::FromStr,
    sync::Arc,
    time::Duration,
};
use tar::Builder;
use time::{OffsetDateTime, UtcOffset};
use tracing::{error, instrument, warn};
use yoke::{Yoke, Yokeable};

use crate::{
    methods::filters::DisplayHexBuffer,
    syntax_highlight::{format_file, format_file_inner, ComrakHighlightAdapter, FileIdentifier},
    unified_diff_builder::{Callback, UnifiedDiffBuilder},
};

type ReadmeCacheKey = (PathBuf, Option<Arc<str>>);

pub struct Git {
    commits: Cache<(ObjectId, bool), Arc<Commit>, hashbrown::hash_map::DefaultHashBuilder>,
    readme_cache: Cache<
        ReadmeCacheKey,
        Option<(ReadmeFormat, Arc<str>)>,
        hashbrown::hash_map::DefaultHashBuilder,
    >,
    open_repositories:
        Cache<PathBuf, ThreadSafeRepository, hashbrown::hash_map::DefaultHashBuilder>,
}

impl Git {
    #[instrument]
    pub fn new() -> Self {
        Self {
            commits: Cache::builder()
                .time_to_live(Duration::from_secs(30))
                .max_capacity(100)
                .build_with_hasher(hashbrown::hash_map::DefaultHashBuilder::default()),
            readme_cache: Cache::builder()
                .time_to_live(Duration::from_secs(30))
                .max_capacity(100)
                .build_with_hasher(hashbrown::hash_map::DefaultHashBuilder::default()),
            open_repositories: Cache::builder()
                .time_to_idle(Duration::from_secs(120))
                .max_capacity(100)
                .build_with_hasher(hashbrown::hash_map::DefaultHashBuilder::default()),
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
        let repo = repo_path.clone();
        let repo = self
            .open_repositories
            .try_get_with_by_ref(&repo_path, async move {
                tokio::task::spawn_blocking(move || {
                    gix::open::Options::isolated()
                        .open_path_as_is(true)
                        .open(&repo)
                })
                .await
                .context("Failed to join Tokio task")
                .map_err(|e| std::io::Error::new(ErrorKind::Other, e))?
                .map_err(|err| {
                    error!("{}", err);
                    std::io::Error::new(ErrorKind::Other, "Failed to open repository")
                })
            })
            .await?;

        Ok(Arc::new(OpenRepository {
            git: self,
            cache_key: repo_path,
            repo,
            branch,
        }))
    }
}

pub struct OpenRepository {
    git: Arc<Git>,
    cache_key: PathBuf,
    repo: ThreadSafeRepository,
    branch: Option<Arc<str>>,
}

impl OpenRepository {
    #[allow(clippy::too_many_lines)]
    pub async fn path(
        self: Arc<Self>,
        path: Option<PathBuf>,
        tree_id: Option<&str>,
        formatted: bool,
    ) -> Result<FileWithContent> {
        let tree_id = tree_id
            .map(ObjectId::from_str)
            .transpose()
            .context("Failed to parse tree hash")?;

        tokio::task::spawn_blocking(move || {
            let repo = self.repo.to_thread_local();

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
                        let mut blob = object.into_blob();

                        let content = match (formatted, simdutf8::basic::from_utf8(&blob.data)) {
                            (true, Err(_)) => Content::Binary(vec![]),
                            (true, Ok(data)) => Content::Text(Cow::Owned(format_file(
                                data,
                                FileIdentifier::Path(path.as_path()),
                            )?)),
                            (false, Err(_)) => Content::Binary(blob.take_data()),
                            (false, Ok(_data)) => Content::Text(Cow::Owned(unsafe {
                                String::from_utf8_unchecked(blob.take_data())
                            })),
                        };

                        return Ok(FileWithContent { content });
                    }
                    _ => anyhow::bail!("bad object of type {:?}", object.kind),
                }
            }

            anyhow::bail!("bad object");
        })
        .await
        .context("Failed to join Tokio task")?
    }

    #[instrument(skip(self))]
    pub async fn tag_info(self: Arc<Self>) -> Result<Yoke<DetailedTag<'static>, Vec<u8>>> {
        tokio::task::spawn_blocking(move || {
            let tag_name = self.branch.clone().context("no tag given")?;
            let repo = self.repo.to_thread_local();

            let tag = repo
                .find_reference(&format!("refs/tags/{tag_name}"))
                .context("Given tag does not exist in repository")?
                .peel_to_tag()
                .context("Couldn't get to a tag from the given reference")?
                .detach()
                .data;

            Yoke::try_attach_to_cart(tag, move |tag| {
                let tag = TagRef::from_bytes(tag)?;

                let tagged_object = match tag.target_kind {
                    Kind::Commit => Some(TaggedObject::Commit(tag.target)),
                    Kind::Tree => Some(TaggedObject::Tree(tag.target)),
                    _ => None,
                };

                Ok::<_, anyhow::Error>(DetailedTag {
                    name: tag_name,
                    tagger: tag.tagger.map(TryInto::try_into).transpose()?,
                    tagged_object,
                    message: tag.message,
                })
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
                    let repo = self.repo.to_thread_local();

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

                        let Ok(content) = simdutf8::basic::from_utf8(&blob.data) else {
                            continue;
                        };

                        if Path::new(name).extension().and_then(OsStr::to_str) == Some("md") {
                            let value = parse_and_transform_markdown(content);
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
            let repo = self.repo.to_thread_local();
            let head = repo.head().context("Couldn't find HEAD of repository")?;
            Ok(head.referent_name().map(|v| v.shorten().to_string()))
        })
        .await
        .context("Failed to join Tokio task")?
    }

    #[instrument(skip(self))]
    pub async fn latest_commit(self: Arc<Self>, highlighted: bool) -> Result<Commit> {
        tokio::task::spawn_blocking(move || {
            let repo = self.repo.to_thread_local();

            let mut head = if let Some(reference) = &self.branch {
                repo.find_reference(reference.as_ref())?
            } else {
                repo.find_reference("HEAD")
                    .context("Couldn't find HEAD of repository")?
            };

            let commit = head
                .peel_to_commit()
                .context("Couldn't find commit HEAD of repository refers to")?;
            let (diff_output, diff_stats) = fetch_diff_and_stats(&repo, &commit, highlighted)?;

            let oid = take_oid(commit.id);
            let inner = Yoke::try_attach_to_cart(commit.detach().data, |commit| {
                CommitInner::new(CommitRef::from_bytes(commit)?, oid)
            })?;

            Ok(Commit {
                inner,
                diff_stats,
                diff: diff_output,
            })
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
            let repo = self.repo.to_thread_local();

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
            let mut visitor = PathVisitor::new(ArchivalVisitor {
                repository: &repo,
                res,
                archive: Builder::new(GzEncoder::new(buffer.writer(), flate2::Compression::fast())),
            });

            tree.traverse().breadthfirst(&mut visitor)?;

            let visitor = visitor.into_inner();

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
                    let repo = self.repo.to_thread_local();

                    let commit = repo.find_commit(commit)?;

                    let (diff_output, diff_stats) =
                        fetch_diff_and_stats(&repo, &commit, highlighted)?;

                    let oid = take_oid(commit.id);

                    let inner = Yoke::try_attach_to_cart(commit.detach().data, |commit| {
                        CommitInner::new(CommitRef::from_bytes(commit)?, oid)
                    })?;

                    Ok(Arc::new(Commit {
                        inner,
                        diff_stats,
                        diff: diff_output,
                    }))
                })
                .await
                .context("Failed to join Tokio task")?
            })
            .await
    }
}

fn take_oid(v: ObjectId) -> [u8; 20] {
    match v {
        ObjectId::Sha1(v) => v,
    }
}

const BUFFER_CAP: usize = 512 * 1024;

pub trait PathVisitorHandler {
    fn visit(&mut self, entry: &EntryRef<'_>, path: &BStr) -> Action;
}

struct ArchivalVisitor<'a> {
    repository: &'a gix::Repository,
    res: tokio::sync::mpsc::Sender<Result<Bytes, anyhow::Error>>,
    archive: Builder<GzEncoder<Writer<BytesMut>>>,
}

impl PathVisitorHandler for ArchivalVisitor<'_> {
    fn visit(&mut self, entry: &EntryRef<'_>, path: &BStr) -> Action {
        let entry = entry.attach(self.repository);

        let Ok(object) = entry.object() else {
            return Action::Continue;
        };

        if object.kind != Kind::Blob {
            return Action::Continue;
        }

        let blob = object.into_blob();

        let mut header = tar::Header::new_gnu();
        if let Err(error) = header.set_path(path.to_path_lossy()) {
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

pub struct PathVisitor<T> {
    path_deque: VecDeque<BString>,
    path: BString,
    inner: T,
}

impl<T> PathVisitor<T> {
    pub fn new(inner: T) -> Self {
        Self {
            path_deque: VecDeque::new(),
            path: BString::default(),
            inner,
        }
    }

    pub fn into_inner(self) -> T {
        self.inner
    }

    fn pop_element(&mut self) {
        if let Some(pos) = memchr::memrchr(b'/', &self.path) {
            self.path.resize(pos, 0);
        } else {
            self.path.clear();
        }
    }

    fn push_element(&mut self, name: &BStr) {
        if name.is_empty() {
            return;
        }
        if !self.path.is_empty() {
            self.path.push(b'/');
        }
        self.path.push_str(name);
    }
}

impl<T: PathVisitorHandler> gix::traverse::tree::Visit for PathVisitor<T> {
    fn pop_front_tracked_path_and_set_current(&mut self) {
        self.path = self
            .path_deque
            .pop_front()
            .expect("every call is matched with push_tracked_path_component");
    }

    fn pop_back_tracked_path_and_set_current(&mut self) {
        self.path = self.path_deque.pop_back().unwrap_or_default();
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

    fn visit_tree(&mut self, entry: &EntryRef<'_>) -> Action {
        self.inner.visit(entry, self.path.as_ref())
    }

    fn visit_nontree(&mut self, entry: &EntryRef<'_>) -> Action {
        self.inner.visit(entry, self.path.as_ref())
    }
}

fn parse_and_transform_markdown(s: &str) -> String {
    let mut plugins = ComrakPlugins::default();

    plugins.render.codefence_syntax_highlighter = Some(&ComrakHighlightAdapter);

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

#[derive(Debug)]
#[allow(unused)]
pub struct FileWithContent {
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
pub enum TaggedObject<'a> {
    Commit(&'a BStr),
    Tree(&'a BStr),
}

#[derive(Debug, Yokeable)]
pub struct DetailedTag<'a> {
    pub name: Arc<str>,
    pub tagger: Option<CommitUser<'a>>,
    pub message: &'a BStr,
    pub tagged_object: Option<TaggedObject<'a>>,
}

#[derive(Debug)]
pub struct CommitUser<'a> {
    name: &'a BStr,
    email: &'a BStr,
    time: (i64, i32),
}

impl<'a> TryFrom<SignatureRef<'a>> for CommitUser<'a> {
    type Error = anyhow::Error;

    fn try_from(v: SignatureRef<'a>) -> Result<Self> {
        Ok(CommitUser {
            name: v.name,
            email: v.email,
            time: (v.time.seconds, v.time.offset),
            // time: OffsetDateTime::from_unix_timestamp(v.time.seconds)?
            //     .to_offset(UtcOffset::from_whole_seconds(v.time.offset)?),
        })
    }
}

impl CommitUser<'_> {
    pub fn name(&self) -> &BStr {
        self.name
    }

    pub fn email(&self) -> &BStr {
        self.email
    }

    pub fn time(&self) -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(self.time.0)
            .unwrap()
            .to_offset(UtcOffset::from_whole_seconds(self.time.1).unwrap())
    }
}

#[derive(Debug)]
pub struct Commit {
    inner: yoke::Yoke<CommitInner<'static>, Vec<u8>>,
    pub diff_stats: String,
    pub diff: String,
}

impl Commit {
    pub fn get(&self) -> &CommitInner<'_> {
        self.inner.get()
    }
}

#[derive(Debug, Yokeable)]
pub struct CommitInner<'a> {
    author: CommitUser<'a>,
    committer: CommitUser<'a>,
    oid: [u8; 20],
    tree: &'a BStr,
    parents: SmallVec<&'a BStr>,
    summary: Cow<'a, BStr>,
    body: &'a BStr,
}

#[derive(Debug)]
enum SmallVec<T> {
    None,
    One(T),
    Many(Vec<T>),
}

impl<T: Copy> SmallVec<T> {
    #[allow(clippy::type_complexity)]
    fn iter(
        &self,
    ) -> Either<std::iter::Empty<T>, Either<std::iter::Once<T>, Copied<std::slice::Iter<T>>>> {
        match self {
            Self::None => Either::Left(std::iter::empty()),
            Self::One(v) => Either::Right(Either::Left(std::iter::once(*v))),
            Self::Many(v) => Either::Right(Either::Right(v.iter().copied())),
        }
    }
}

impl<'a> CommitInner<'a> {
    pub fn new(commit: gix::worktree::object::CommitRef<'a>, oid: [u8; 20]) -> Result<Self> {
        let message = commit.message();

        Ok(CommitInner {
            author: CommitUser::try_from(commit.author)?,
            committer: CommitUser::try_from(commit.committer)?,
            oid,
            tree: commit.tree,
            parents: commit.parents.into_inner().map_or_else(
                |inner| {
                    if inner.is_empty() {
                        SmallVec::None
                    } else {
                        SmallVec::Many(inner.into_vec())
                    }
                },
                |[v]| SmallVec::One(v),
            ),

            summary: message.summary(),
            body: message.body.unwrap_or_else(|| BStr::new("")),
        })
    }
}

impl CommitInner<'_> {
    pub fn author(&self) -> &CommitUser<'_> {
        &self.author
    }

    pub fn committer(&self) -> &CommitUser<'_> {
        &self.committer
    }

    pub fn oid(&self) -> DisplayHexBuffer<20> {
        let mut buf = const_hex::Buffer::new();
        buf.format(&self.oid);
        DisplayHexBuffer(buf)
    }

    pub fn tree(&self) -> &BStr {
        self.tree
    }

    pub fn parents(&self) -> impl Iterator<Item = &BStr> {
        self.parents.iter()
    }

    pub fn summary(&self) -> &BStr {
        &self.summary
    }

    pub fn body(&self) -> &BStr {
        self.body
    }
}

#[instrument(skip(repo, commit))]
fn fetch_diff_and_stats(
    repo: &gix::Repository,
    commit: &gix::Commit<'_>,
    highlight: bool,
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

    let mut diffs = Vec::new();
    let mut diff_output = String::new();

    let mut resource_cache = repo.diff_resource_cache_for_tree_diff()?;

    let mut changes = parent_tree.changes()?;
    changes.options(|opts| {
        opts.track_path().track_rewrites(None);
    });
    changes.for_each_to_obtain_tree_with_cache(
        &current_tree,
        &mut repo.diff_resource_cache_for_tree_diff()?,
        |change| {
            if highlight {
                DiffBuilder {
                    output: &mut diff_output,
                    resource_cache: &mut resource_cache,
                    diffs: &mut diffs,
                    formatter: SyntaxHighlightedDiffFormatter::new(
                        change.location().to_path().unwrap(),
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
             stats| {
                (
                    max_file_name_length.max(stats.path.len()),
                    max_change_length
                        .max(((stats.insertions + stats.deletions + 1).ilog10() + 1) as usize),
                    files_changed + 1,
                    insertions + stats.insertions,
                    deletions + stats.deletions,
                )
            },
        );

    let mut diff_stats = String::new();

    let total_changes = insertions + deletions;

    for diff in &diffs {
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

        let file = diff.path.as_str();
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
    path: String,
    insertions: usize,
    deletions: usize,
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
    diffs: &'a mut Vec<FileDiff>,
    formatter: F,
}

impl<F: DiffFormatter + Callback> DiffBuilder<'_, F> {
    #[allow(clippy::too_many_lines)]
    fn handle(
        &mut self,
        change: gix::object::tree::diff::Change<'_, '_, '_>,
    ) -> Result<gix::object::tree::diff::Action> {
        if !change.entry_mode().is_blob_or_symlink() {
            return Ok(gix::object::tree::diff::Action::Continue);
        }

        let mut diff = FileDiff {
            path: change.location().to_string(),
            insertions: 0,
            deletions: 0,
        };
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
                    simdutf8::basic::from_utf8(prep.old.data.as_slice().unwrap_or_default())?,
                );
                let new_source = gix::diff::blob::sources::lines_with_terminator(
                    simdutf8::basic::from_utf8(prep.new.data.as_slice().unwrap_or_default())?,
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

        self.diffs.push(diff);

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
    path: &'a Path,
}

impl<'a> SyntaxHighlightedDiffFormatter<'a> {
    fn new(path: &'a Path) -> Self {
        Self { path }
    }

    fn write(&self, output: &mut String, class: &str, data: &str) {
        write!(output, r#"<span class="diff-{class}">"#).unwrap();
        format_file_inner(output, data, FileIdentifier::Path(self.path), false).unwrap();
        output.push_str("</span>");
    }
}

impl DiffFormatter for SyntaxHighlightedDiffFormatter<'_> {
    fn file_header(&self, output: &mut String, data: Arguments<'_>) {
        write!(output, r#"<span class="diff-file-header">"#).unwrap();
        write!(output, "{data}").unwrap();
        output.push_str("</span>\n");
    }

    fn binary(
        &self,
        output: &mut String,
        left: &str,
        right: &str,
        _left_content: &[u8],
        _right_content: &[u8],
    ) {
        write!(output, "Binary files {left} and {right} differ").unwrap();
    }
}

impl Callback for SyntaxHighlightedDiffFormatter<'_> {
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
