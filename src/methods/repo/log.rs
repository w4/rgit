use anyhow::Context;
use askama::Template;
use axum::{extract::Query, response::Response, Extension};
use serde::Deserialize;
use yoke::Yoke;

use crate::{
    into_response,
    methods::{
        filters,
        repo::{Repository, Result},
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

    let reference = format!("refs/heads/{}", query.branch.as_deref().unwrap_or("master"));
    let repository = crate::database::schema::repository::Repository::open(&db, &*repo)?
        .context("Repository does not exist")?;
    let commit_tree = repository.get().commit_tree(&db, &reference)?;
    let mut commits = commit_tree.fetch_latest(101, offset).await;

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
