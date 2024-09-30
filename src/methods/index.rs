use std::{collections::BTreeMap, sync::Arc};

use anyhow::Context;
use askama::Template;
use axum::{response::IntoResponse, Extension};

use super::filters;
use crate::{
    database::schema::repository::{Repository, YokedRepository},
    into_response,
};

#[derive(Template)]
#[template(path = "index.html")]
pub struct View {
    pub repositories: BTreeMap<Option<String>, Vec<YokedRepository>>,
}

pub async fn handle(
    Extension(db): Extension<Arc<rocksdb::DB>>,
) -> Result<impl IntoResponse, super::repo::Error> {
    let mut repositories: BTreeMap<Option<String>, Vec<YokedRepository>> = BTreeMap::new();

    let fetched = tokio::task::spawn_blocking(move || Repository::fetch_all(&db))
        .await
        .context("Failed to join Tokio task")??;

    for (k, v) in fetched {
        // TODO: fixme
        let mut split: Vec<_> = k.split('/').collect();
        split.pop();
        let key = Some(split.join("/")).filter(|v| !v.is_empty());

        let k = repositories.entry(key).or_default();
        k.push(v);
    }

    Ok(into_response(View { repositories }))
}
