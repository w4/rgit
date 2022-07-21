use anyhow::Context;
use std::collections::BTreeMap;

use askama::Template;
use axum::response::Response;
use axum::Extension;

use super::filters;
use crate::database::schema::repository::Repository;
use crate::into_response;

#[derive(Template)]
#[template(path = "index.html")]
pub struct View<'a> {
    pub repositories: BTreeMap<Option<String>, Vec<&'a Repository<'a>>>,
}

pub async fn handle(Extension(db): Extension<sled::Db>) -> Result<Response, super::repo::Error> {
    let mut repositories: BTreeMap<Option<String>, Vec<&Repository<'_>>> = BTreeMap::new();

    let fetched = tokio::task::spawn_blocking(move || Repository::fetch_all(&db))
        .await
        .context("Failed to join Tokio task")??;
    for (k, v) in &fetched {
        // TODO: fixme
        let mut split: Vec<_> = k.split('/').collect();
        split.pop();
        let key = Some(split.join("/")).filter(|v| !v.is_empty());

        let k = repositories.entry(key).or_default();
        k.push(v.get());
    }

    Ok(into_response(&View { repositories }))
}
