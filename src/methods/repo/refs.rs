use std::{collections::BTreeMap, sync::Arc};

use crate::{
    into_response,
    methods::{
        filters,
        repo::{Refs, Repository, Result},
    },
};
use anyhow::Context;
use askama::Template;
use axum::{response::IntoResponse, Extension};
use rkyv::string::ArchivedString;
use yoke::Yoke;

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
        let repository = repository.get();

        let heads_db = repository.heads(&db)?;
        let heads_db = heads_db.as_ref().map(Yoke::get);

        let mut heads = BTreeMap::new();
        if let Some(archived_heads) = heads_db {
            for head in archived_heads
                .0
                .as_slice()
                .iter()
                .map(ArchivedString::as_str)
            {
                let commit_tree = repository.commit_tree(db.clone(), head);
                let name = head.strip_prefix("refs/heads/");

                if let (Some(name), Some(commit)) = (name, commit_tree.fetch_latest_one()?) {
                    heads.insert(name.to_string(), commit);
                }
            }
        }

        let tags = repository.tag_tree(db).fetch_all()?;

        Ok(into_response(View {
            repo,
            refs: Refs { heads, tags },
            branch: None,
        }))
    })
    .await
    .context("Failed to attach to tokio task")?
}
