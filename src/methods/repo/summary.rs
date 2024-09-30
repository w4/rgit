use std::{collections::BTreeMap, sync::Arc};

use anyhow::Context;
use askama::Template;
use axum::{response::IntoResponse, Extension};
use rkyv::string::ArchivedString;

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
pub struct View {
    repo: Repository,
    refs: Refs,
    commit_list: Vec<YokedCommit>,
    branch: Option<Arc<str>>,
}

pub async fn handle(
    Extension(repo): Extension<Repository>,
    Extension(db): Extension<Arc<rocksdb::DB>>,
) -> Result<impl IntoResponse> {
    tokio::task::spawn_blocking(move || {
        let repository = crate::database::schema::repository::Repository::open(&db, &*repo)?
            .context("Repository does not exist")?;
        let commits = get_default_branch_commits(&repository, &db)?;

        let mut heads = BTreeMap::new();
        if let Some(heads_db) = repository.get().heads(&db)? {
            for head in heads_db
                .get()
                .0
                .as_slice()
                .iter()
                .map(ArchivedString::as_str)
            {
                let commit_tree = repository.get().commit_tree(db.clone(), head);
                let name = head.strip_prefix("refs/heads/");

                if let (Some(name), Some(commit)) = (name, commit_tree.fetch_latest_one()?) {
                    heads.insert(name.to_string(), commit);
                }
            }
        }

        let tags = repository.get().tag_tree(db).fetch_all()?;

        Ok(into_response(View {
            repo,
            refs: Refs { heads, tags },
            commit_list: commits,
            branch: None,
        }))
    })
    .await
    .context("Failed to attach to tokio task")?
}

pub fn get_default_branch_commits(
    repository: &YokedRepository,
    database: &Arc<rocksdb::DB>,
) -> Result<Vec<YokedCommit>> {
    for branch in repository
        .get()
        .default_branch
        .as_deref()
        .into_iter()
        .chain(DEFAULT_BRANCHES.into_iter())
    {
        let commit_tree = repository.get().commit_tree(database.clone(), branch);
        let commits = commit_tree.fetch_latest(11, 0)?;

        if !commits.is_empty() {
            return Ok(commits);
        }
    }

    Ok(vec![])
}
