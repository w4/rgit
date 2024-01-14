use std::{collections::HashSet, sync::Arc};

use anyhow::Context;
use git2::Signature;
use serde::{Deserialize, Serialize};
use yoke::{Yoke, Yokeable};

use crate::database::schema::{
    commit::Author, prefixes::TAG_FAMILY, repository::RepositoryId, Yoked,
};

#[derive(Serialize, Deserialize, Debug, Yokeable)]
pub struct Tag<'a> {
    #[serde(borrow)]
    pub tagger: Option<Author<'a>>,
}

impl<'a> Tag<'a> {
    pub fn new(tagger: Option<&'a Signature<'_>>) -> Self {
        Self {
            tagger: tagger.map(Into::into),
        }
    }

    pub fn insert(&self, batch: &TagTree, name: &str) -> Result<(), anyhow::Error> {
        batch.insert(name, self)
    }
}

pub struct TagTree {
    db: Arc<rocksdb::DB>,
    prefix: RepositoryId,
}

pub type YokedTag = Yoked<Tag<'static>>;

impl TagTree {
    pub(super) fn new(db: Arc<rocksdb::DB>, prefix: RepositoryId) -> Self {
        Self { db, prefix }
    }

    pub fn insert(&self, name: &str, value: &Tag<'_>) -> anyhow::Result<()> {
        let cf = self
            .db
            .cf_handle(TAG_FAMILY)
            .context("missing tag column family")?;

        let mut db_name = self.prefix.to_be_bytes().to_vec();
        db_name.extend_from_slice(name.as_ref());

        self.db.put_cf(cf, db_name, bincode::serialize(value)?)?;

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

    pub fn fetch_all(&self) -> anyhow::Result<Vec<(String, YokedTag)>> {
        let cf = self
            .db
            .cf_handle(TAG_FAMILY)
            .context("missing tag column family")?;

        let mut res = self
            .db
            .prefix_iterator_cf(cf, self.prefix.to_be_bytes())
            .filter_map(Result::ok)
            .filter_map(|(name, value)| {
                let name = String::from_utf8_lossy(name.strip_prefix(&self.prefix.to_be_bytes())?)
                    .strip_prefix("refs/tags/")?
                    .to_string();

                Some((name, value))
            })
            .map(|(name, value)| {
                let value = Yoke::try_attach_to_cart(value, |data| bincode::deserialize(data))?;
                Ok((name, value))
            })
            .collect::<anyhow::Result<Vec<(String, YokedTag)>>>()?;

        res.sort_unstable_by(|a, b| {
            let a_tagger = a.1.get().tagger.as_ref().map(|v| v.time);
            let b_tagger = b.1.get().tagger.as_ref().map(|v| v.time);
            b_tagger.cmp(&a_tagger)
        });

        Ok(res)
    }
}
