use std::{cell::RefCell, sync::Arc};

use anyhow::Context;
use askama::Template;
use axum::{
    response::{IntoResponse, Response},
    Extension,
};
use itertools::{Either, Itertools};

use super::filters;
use crate::{
    database::schema::repository::{Repository, YokedRepository},
    into_response,
};

#[derive(Template)]
#[template(path = "index.html")]
pub struct View<
    'a,
    Group: Iterator<Item = (&'a String, &'a YokedRepository)>,
    GroupIter: Iterator<Item = (&'a str, Group)>,
> {
    // this type sig is a necessary evil unfortunately, because askama takes a reference
    // to the data for rendering.
    pub repositories: RefCell<Either<GroupIter, std::iter::Empty<(&'a str, Group)>>>,
}

impl<'a, Group, GroupIter> View<'a, Group, GroupIter>
where
    Group: Iterator<Item = (&'a String, &'a YokedRepository)>,
    GroupIter: Iterator<Item = (&'a str, Group)>,
{
    fn take_iter(&self) -> Either<GroupIter, std::iter::Empty<(&'a str, Group)>> {
        self.repositories.replace(Either::Right(std::iter::empty()))
    }
}

pub async fn handle(
    Extension(db): Extension<Arc<rocksdb::DB>>,
) -> Result<Response, super::repo::Error> {
    let fetched = tokio::task::spawn_blocking(move || Repository::fetch_all(&db))
        .await
        .context("Failed to join Tokio task")??;

    // rocksdb returned the keys already ordered for us so group_by is a nice
    // operation we can use here to avoid writing into a map to group. though,
    // now that i think about it it might act a little bit strangely when mixing
    // root repositories and nested repositories. we're going to have to prefix
    // root repositories with a null byte or something. i'll just leave this here
    // as a TODO.
    let repositories = fetched
        .iter()
        .group_by(|(k, _)| memchr::memrchr(b'/', k.as_bytes()).map_or("", |idx| &k[..idx]));

    Ok(into_response(View {
        repositories: Either::Left(repositories.into_iter()).into(),
    })
    .into_response())
}
