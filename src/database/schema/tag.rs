use std::{collections::HashSet, sync::Arc};

use anyhow::Context;
use gix::actor::SignatureRef;
use rkyv::{Archive, Serialize};
use yoke::{Yoke, Yokeable};

use crate::database::schema::{
    commit::{ArchivedAuthor, Author},
    prefixes::TAG_FAMILY,
    repository::RepositoryId,
    Yoked,
};

#[derive(Serialize, Archive, Debug, Yokeable)]
pub struct Tag {
    pub tagger: Option<Author>,
}

impl Tag {
    pub fn new(tagger: Option<SignatureRef<'_>>) -> Result<Self, anyhow::Error> {
        Ok(Self {
            tagger: tagger.map(TryFrom::try_from).transpose()?,
        })
    }

    pub fn insert(&self, batch: &TagTree, name: &str) -> Result<(), anyhow::Error> {
        batch.insert(name, self)
    }
}

pub struct TagTree {
    db: Arc<rocksdb::DB>,
    prefix: RepositoryId,
}

pub type YokedString = Yoked<&'static str>;
pub type YokedTag = Yoked<&'static <Tag as Archive>::Archived>;

impl TagTree {
    pub(super) fn new(db: Arc<rocksdb::DB>, prefix: RepositoryId) -> Self {
        Self { db, prefix }
    }

    pub fn insert(&self, name: &str, value: &Tag) -> anyhow::Result<()> {
        let cf = self
            .db
            .cf_handle(TAG_FAMILY)
            .context("missing tag column family")?;

        let mut db_name = self.prefix.to_be_bytes().to_vec();
        db_name.extend_from_slice(name.as_ref());

        self.db
            .put_cf(cf, db_name, rkyv::to_bytes::<rkyv::rancor::Error>(value)?)?;

        Ok(())
    }

    pub fn remove(&self, name: &str) -> anyhow::Result<()> {
        let cf = self
            .db
            .cf_handle(TAG_FAMILY)
            .context("missing tag column family")?;

        let mut db_name = self.prefix.to_be_bytes().to_vec();
        db_name.extend_from_slice(name.as_ref());
        self.db.delete_cf(cf, db_name)?;

        Ok(())
    }

    pub fn list(&self) -> anyhow::Result<HashSet<String>> {
        let cf = self
            .db
            .cf_handle(TAG_FAMILY)
            .context("missing tag column family")?;

        Ok(self
            .db
            .prefix_iterator_cf(cf, self.prefix.to_be_bytes())
            .filter_map(Result::ok)
            .filter_map(|(k, _)| {
                Some(
                    String::from_utf8_lossy(k.strip_prefix(&self.prefix.to_be_bytes())?)
                        .to_string(),
                )
            })
            .collect())
    }

    pub fn fetch_all(&self) -> anyhow::Result<Vec<(YokedString, YokedTag)>> {
        let cf = self
            .db
            .cf_handle(TAG_FAMILY)
            .context("missing tag column family")?;

        let mut res = self
            .db
            .prefix_iterator_cf(cf, self.prefix.to_be_bytes())
            .filter_map(Result::ok)
            .filter_map(|(name, value)| {
                let name = Yoke::try_attach_to_cart(name, |data| {
                    let data = data
                        .strip_prefix(&self.prefix.to_be_bytes())
                        .ok_or(())?
                        .strip_prefix(b"refs/tags/")
                        .ok_or(())?;
                    simdutf8::basic::from_utf8(data).map_err(|_| ())
                })
                .ok()?;

                Some((name, value))
            })
            .map(|(name, value)| {
                let value = Yoke::try_attach_to_cart(value, |data| {
                    rkyv::access::<_, rkyv::rancor::Error>(data)
                })?;
                Ok((name, value))
            })
            .collect::<anyhow::Result<Vec<(YokedString, YokedTag)>>>()?;

        res.sort_unstable_by(|a, b| {
            let a_tagger = a.1.get().tagger.as_ref().map(ArchivedAuthor::time);
            let b_tagger = b.1.get().tagger.as_ref().map(ArchivedAuthor::time);
            b_tagger.cmp(&a_tagger)
        });

        Ok(res)
    }
}
