use std::{collections::BTreeMap, sync::Arc};

use anyhow::Context;
use askama::Template;
use axum::{response::Response, Extension};
use yoke::Yoke;

use crate::{
    database::schema::{commit::YokedCommit, repository::YokedRepository},
    into_response,
    methods::{
        filters,
        repo::{Refs, Repository, Result, DEFAULT_BRANCHES},
    },
};

#[derive(Template)]
#[template(path = "repo/summary.html")]
pub struct View<'a> {
    repo: Repository,
    refs: Refs,
    commit_list: Vec<&'a crate::database::schema::commit::Commit<'a>>,
    branch: Option<Arc<str>>,
}

pub async fn handle(
    Extension(repo): Extension<Repository>,
    Extension(db): Extension<sled::Db>,
) -> Result<Response> {
    let repository = crate::database::schema::repository::Repository::open(&db, &*repo)?
        .context("Repository does not exist")?;
    let commits = get_default_branch_commits(&repository, &db).await?;
    let commit_list = commits.iter().map(Yoke::get).collect();

    let mut heads = BTreeMap::new();
    for head in repository.get().heads(&db) {
        let commit_tree = repository.get().commit_tree(&db, &head)?;
        let name = head.strip_prefix("refs/heads/");

        if let (Some(name), Some(commit)) = (name, commit_tree.fetch_latest_one()) {
            heads.insert(name.to_string(), commit);
        }
    }

    let tags = repository
        .get()
        .tag_tree(&db)
        .context("Failed to fetch indexed tags")?
        .fetch_all();

    Ok(into_response(&View {
        repo,
        refs: Refs { heads, tags },
        commit_list,
        branch: None,
    }))
}

pub async fn get_default_branch_commits(
    repository: &YokedRepository,
    database: &sled::Db,
) -> Result<Vec<YokedCommit>> {
    for branch in DEFAULT_BRANCHES {
        let commit_tree = repository.get().commit_tree(database, branch)?;
        let commits = commit_tree.fetch_latest(11, 0).await;

        if !commits.is_empty() {
            return Ok(commits);
        }
    }

    Ok(vec![])
}
