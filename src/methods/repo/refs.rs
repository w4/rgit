use std::{collections::BTreeMap, sync::Arc};

use anyhow::Context;
use askama::Template;
use axum::{response::IntoResponse, Extension};
use rkyv::string::ArchivedString;

use crate::{
    into_response,
    methods::{
        filters,
        repo::{Refs, Repository, Result},
    },
};

#[derive(Template)]
#[template(path = "repo/refs.html")]
pub struct View {
    repo: Repository,
    refs: Refs,
    branch: Option<Arc<str>>,
}

pub async fn handle(
    Extension(repo): Extension<Repository>,
    Extension(db): Extension<Arc<rocksdb::DB>>,
) -> Result<impl IntoResponse> {
    tokio::task::spawn_blocking(move || {
        let repository = crate::database::schema::repository::Repository::open(&db, &*repo)?
            .context("Repository does not exist")?;

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
            branch: None,
        }))
    })
    .await
    .context("Failed to attach to tokio task")?
}
