use std::collections::BTreeMap;

use askama::Template;
use axum::response::Response;
use axum::Extension;

use super::filters;
use crate::database::schema::repository::Repository;
use crate::into_response;

#[derive(Template)]
#[template(path = "index.html")]
pub struct View {
    pub repositories: BTreeMap<Option<String>, Vec<Repository>>,
}

pub async fn handle(Extension(db): Extension<sled::Db>) -> Response {
    let mut repositories: BTreeMap<Option<String>, Vec<Repository>> = BTreeMap::new();

    for (k, v) in Repository::fetch_all(&db) {
        // TODO: fixme
        let mut split: Vec<_> = k.split('/').collect();
        split.pop();
        let key = Some(split.join("/")).filter(|v| !v.is_empty());

        let k = repositories.entry(key).or_default();
        k.push(v);
    }

    into_response(&View { repositories })
}
