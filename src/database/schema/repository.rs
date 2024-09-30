use std::{collections::BTreeMap, ops::Deref, path::Path, sync::Arc};

use anyhow::{Context, Result};
use rand::random;
use rkyv::{Archive, Serialize};
use rocksdb::IteratorMode;
use yoke::{Yoke, Yokeable};

use crate::database::schema::{
    commit::CommitTree,
    prefixes::{COMMIT_FAMILY, REFERENCE_FAMILY, REPOSITORY_FAMILY, TAG_FAMILY},
    tag::TagTree,
    Yoked,
};

#[derive(Serialize, Archive, Debug, PartialEq, Eq, Hash, Yokeable)]
pub struct Repository {
    /// The ID of the repository, as stored in `RocksDB`
    pub id: RepositoryId,
    /// The "clean name" of the repository (ie. `hello-world.git`)
    pub name: String,
    /// The description of the repository, as it is stored in the `description` file in the
    /// bare repo root
    pub description: Option<String>,
    /// The owner of the repository (`gitweb.owner` in the repository configuration)
    pub owner: Option<String>,
    /// The last time this repository was updated, currently read from the directory mtime
    pub last_modified: (i64, i32),
    /// The default branch for Git operations
    pub default_branch: Option<String>,
}

pub type YokedRepository = Yoked<&'static <Repository as Archive>::Archived>;

impl Repository {
    pub fn exists<P: AsRef<Path>>(database: &rocksdb::DB, path: P) -> Result<bool> {
        let cf = database
            .cf_handle(REPOSITORY_FAMILY)
            .context("repository column family missing")?;
        let path = path.as_ref().to_str().context("invalid path")?;

        Ok(database.get_pinned_cf(cf, path)?.is_some())
    }

    pub fn fetch_all(database: &rocksdb::DB) -> Result<BTreeMap<String, YokedRepository>> {
        let cf = database
            .cf_handle(REPOSITORY_FAMILY)
            .context("repository column family missing")?;

        database
            .iterator_cf(cf, IteratorMode::Start)
            .filter_map(Result::ok)
            .map(|(key, value)| {
                let key = String::from_utf8(key.into_vec()).context("invalid repo name")?;
                let value = Yoke::try_attach_to_cart(value, |data| {
                    rkyv::access::<_, rkyv::rancor::Error>(data)
                })?;

                Ok((key, value))
            })
            .collect()
    }

    pub fn insert<P: AsRef<Path>>(&self, database: &rocksdb::DB, path: P) -> Result<()> {
        let cf = database
            .cf_handle(REPOSITORY_FAMILY)
            .context("repository column family missing")?;
        let path = path.as_ref().to_str().context("invalid path")?;

        database.put_cf(cf, path, rkyv::to_bytes::<rkyv::rancor::Error>(self)?)?;

        Ok(())
    }

    pub fn open<P: AsRef<Path>>(
        database: &rocksdb::DB,
        path: P,
    ) -> Result<Option<YokedRepository>> {
        let cf = database
            .cf_handle(REPOSITORY_FAMILY)
            .context("repository column family missing")?;

        let path = path.as_ref().to_str().context("invalid path")?;
        let Some(value) = database.get_cf(cf, path)? else {
            return Ok(None);
        };

        Yoke::try_attach_to_cart(value.into_boxed_slice(), |data| {
            rkyv::access::<_, rkyv::rancor::Error>(data)
        })
        .map(Some)
        .context("Failed to open repository")
    }
}

impl ArchivedRepository {
    pub fn delete<P: AsRef<Path>>(&self, database: &rocksdb::DB, path: P) -> Result<()> {
        let start_id = self.id.0.to_native().to_be_bytes();
        let mut end_id = start_id;
        *end_id.last_mut().unwrap() += 1;

        // delete commits
        let commit_cf = database
            .cf_handle(COMMIT_FAMILY)
            .context("commit column family missing")?;
        database.delete_range_cf(commit_cf, start_id, end_id)?;

        // delete tags
        let tag_cf = database
            .cf_handle(TAG_FAMILY)
            .context("tag column family missing")?;
        database.delete_range_cf(tag_cf, start_id, end_id)?;

        // delete self
        let repo_cf = database
            .cf_handle(REPOSITORY_FAMILY)
            .context("repository column family missing")?;
        let path = path.as_ref().to_str().context("invalid path")?;
        database.delete_cf(repo_cf, path)?;

        Ok(())
    }

    pub fn commit_tree(&self, database: Arc<rocksdb::DB>, reference: &str) -> CommitTree {
        CommitTree::new(database, RepositoryId(self.id.0.to_native()), reference)
    }

    pub fn tag_tree(&self, database: Arc<rocksdb::DB>) -> TagTree {
        TagTree::new(database, RepositoryId(self.id.0.to_native()))
    }

    pub fn replace_heads(&self, database: &rocksdb::DB, new_heads: &Vec<String>) -> Result<()> {
        let cf = database
            .cf_handle(REFERENCE_FAMILY)
            .context("missing reference column family")?;

        database.put_cf(
            cf,
            self.id.0.to_native().to_be_bytes(),
            rkyv::to_bytes::<rkyv::rancor::Error>(new_heads)?,
        )?;

        Ok(())
    }

    #[allow(clippy::type_complexity)]
    pub fn heads(
        &self,
        database: &rocksdb::DB,
    ) -> Result<Option<Yoke<&'static ArchivedHeads, Box<[u8]>>>> {
        let cf = database
            .cf_handle(REFERENCE_FAMILY)
            .context("missing reference column family")?;

        let Some(bytes) = database.get_cf(cf, self.id.0.to_native().to_be_bytes())? else {
            return Ok(None);
        };

        Yoke::try_attach_to_cart(Box::from(bytes), |bytes| {
            rkyv::access::<_, rkyv::rancor::Error>(bytes)
        })
        .context("failed to deserialize heads")
        .map(Some)
    }
}

#[derive(Serialize, Archive, Debug, Clone, PartialEq, Eq, Hash)]
pub struct Heads(pub Vec<String>);

#[derive(Serialize, Archive, Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub struct RepositoryId(pub u64);

impl RepositoryId {
    pub fn new() -> Self {
        Self(random())
    }
}

impl Deref for RepositoryId {
    type Target = u64;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
