use std::sync::Arc;

use anyhow::Context;
use askama::Template;
use axum::{extract::Query, response::Response, Extension};
use serde::Deserialize;
use yoke::Yoke;

use crate::{
    database::schema::{commit::YokedCommit, repository::YokedRepository},
    into_response,
    methods::{
        filters,
        repo::{Repository, Result, DEFAULT_BRANCHES},
    },
};

#[derive(Deserialize)]
pub struct UriQuery {
    #[serde(rename = "ofs")]
    offset: Option<u64>,
    #[serde(rename = "h")]
    branch: Option<String>,
}

#[derive(Template)]
#[template(path = "repo/log.html")]
pub struct View<'a> {
    repo: Repository,
    commits: Vec<&'a crate::database::schema::commit::Commit<'a>>,
    next_offset: Option<u64>,
    branch: Option<String>,
}

pub async fn handle(
    Extension(repo): Extension<Repository>,
    Extension(db): Extension<Arc<rocksdb::DB>>,
    Query(query): Query<UriQuery>,
) -> Result<Response> {
    tokio::task::spawn_blocking(move || {
        let offset = query.offset.unwrap_or(0);

        let repository = crate::database::schema::repository::Repository::open(&db, &*repo)?
            .context("Repository does not exist")?;
        let mut commits =
            get_branch_commits(&repository, &db, query.branch.as_deref(), 101, offset)?;

        let next_offset = if commits.len() == 101 {
            commits.pop();
            Some(offset + 100)
        } else {
            None
        };

        let commits = commits.iter().map(Yoke::get).collect();

        Ok(into_response(&View {
            repo,
            commits,
            next_offset,
            branch: query.branch,
        }))
    })
    .await
    .context("Failed to attach to tokio task")?
}

pub fn get_branch_commits(
    repository: &YokedRepository,
    database: &Arc<rocksdb::DB>,
    branch: Option<&str>,
    amount: u64,
    offset: u64,
) -> Result<Vec<YokedCommit>> {
    if let Some(reference) = branch {
        let commit_tree = repository
            .get()
            .commit_tree(database.clone(), &format!("refs/heads/{reference}"));
        let commit_tree = commit_tree.fetch_latest(amount, offset)?;

        if !commit_tree.is_empty() {
            return Ok(commit_tree);
        }

        let tag_tree = repository
            .get()
            .commit_tree(database.clone(), &format!("refs/tags/{reference}"));
        let tag_tree = tag_tree.fetch_latest(amount, offset)?;

        return Ok(tag_tree);
    }

    for branch in repository
        .get()
        .default_branch
        .as_deref()
        .into_iter()
        .chain(DEFAULT_BRANCHES.into_iter())
    {
        let commit_tree = repository.get().commit_tree(database.clone(), branch);
        let commits = commit_tree.fetch_latest(amount, offset)?;

        if !commits.is_empty() {
            return Ok(commits);
        }
    }

    Ok(vec![])
}
