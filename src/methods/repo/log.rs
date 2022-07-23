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
    offset: Option<usize>,
    #[serde(rename = "h")]
    branch: Option<String>,
}

#[derive(Template)]
#[template(path = "repo/log.html")]
pub struct View<'a> {
    repo: Repository,
    commits: Vec<&'a crate::database::schema::commit::Commit<'a>>,
    next_offset: Option<usize>,
    branch: Option<String>,
}

pub async fn handle(
    Extension(repo): Extension<Repository>,
    Extension(db): Extension<sled::Db>,
    Query(query): Query<UriQuery>,
) -> Result<Response> {
    let offset = query.offset.unwrap_or(0);

    let repository = crate::database::schema::repository::Repository::open(&db, &*repo)?
        .context("Repository does not exist")?;
    let mut commits =
        get_branch_commits(&repository, &db, query.branch.as_deref(), 101, offset).await?;

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
}

pub async fn get_branch_commits(
    repository: &YokedRepository,
    database: &sled::Db,
    branch: Option<&str>,
    amount: usize,
    offset: usize,
) -> Result<Vec<YokedCommit>> {
    let reference = branch.map(|branch| format!("refs/heads/{branch}"));

    if let Some(reference) = reference {
        let commit_tree = repository.get().commit_tree(database, &reference)?;
        return Ok(commit_tree.fetch_latest(amount, offset).await);
    }

    for branch in DEFAULT_BRANCHES {
        let commit_tree = repository.get().commit_tree(database, branch)?;
        let commits = commit_tree.fetch_latest(amount, offset).await;

        if !commits.is_empty() {
            return Ok(commits);
        }
    }

    Ok(vec![])
}
