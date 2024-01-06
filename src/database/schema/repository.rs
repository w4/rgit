use std::{borrow::Cow, collections::BTreeMap, ops::Deref, path::Path};

use anyhow::{Context, Result};
use nom::AsBytes;
use serde::{Deserialize, Serialize};
use sled::IVec;
use time::OffsetDateTime;
use yoke::{Yoke, Yokeable};

use crate::database::schema::{commit::CommitTree, prefixes::TreePrefix, tag::TagTree, Yoked};

#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Hash, Yokeable)]
pub struct Repository<'a> {
    /// The ID of the repository, as stored in `sled`
    pub id: RepositoryId,
    /// The "clean name" of the repository (ie. `hello-world.git`)
    #[serde(borrow)]
    pub name: Cow<'a, str>,
    /// The description of the repository, as it is stored in the `description` file in the
    /// bare repo root
    #[serde(borrow)]
    pub description: Option<Cow<'a, str>>,
    /// The owner of the repository (`gitweb.owner` in the repository configuration)
    #[serde(borrow)]
    pub owner: Option<Cow<'a, str>>,
    /// The last time this repository was updated, currently read from the directory mtime
    pub last_modified: OffsetDateTime,
    /// The default branch for Git operations
    #[serde(borrow)]
    pub default_branch: Option<Cow<'a, str>>,
}

pub type YokedRepository = Yoked<Repository<'static>>;

impl Repository<'_> {
    pub fn exists<P: AsRef<Path>>(database: &sled::Db, path: P) -> bool {
        database
            .contains_key(TreePrefix::repository_id(path))
            .unwrap_or_default()
    }

    pub fn fetch_all(database: &sled::Db) -> Result<BTreeMap<String, YokedRepository>> {
        database
            .scan_prefix([TreePrefix::Repository as u8])
            .filter_map(Result::ok)
            .map(|(key, value)| {
                // strip the prefix we've just scanned for
                let key = String::from_utf8_lossy(&key[1..]).to_string();

                // internally value is an Arc so it should already be stablederef but because
                // of reasons unbeknownst to me, sled has its own Arc implementation so we need
                // to box the value as well to get a stablederef...
                let value = Box::new(value);

                let value =
                    Yoke::try_attach_to_cart(value, |data: &IVec| bincode::deserialize(data))?;

                Ok((key, value))
            })
            .collect()
    }

    pub fn insert<P: AsRef<Path>>(&self, database: &sled::Db, path: P) {
        database
            .insert(
                TreePrefix::repository_id(path),
                bincode::serialize(self).unwrap(),
            )
            .unwrap();
    }

    pub fn delete<P: AsRef<Path>>(&self, database: &sled::Db, path: P) -> Result<()> {
        for reference in self.heads(database) {
            database.drop_tree(TreePrefix::commit_id(self.id, &reference))?;
        }

        database.drop_tree(TreePrefix::tag_id(self.id))?;
        database.remove(TreePrefix::repository_id(path))?;

        Ok(())
    }

    pub fn open<P: AsRef<Path>>(database: &sled::Db, path: P) -> Result<Option<YokedRepository>> {
        database
            .get(TreePrefix::repository_id(path))
            .context("Failed to open indexed repository")?
            .map(|value| {
                // internally value is an Arc so it should already be stablederef but because
                // of reasons unbeknownst to me, sled has its own Arc implementation so we need
                // to box the value as well to get a stablederef...
                let value = Box::new(value);

                Yoke::try_attach_to_cart(value, |data: &IVec| bincode::deserialize(data))
                    .context("Failed to deserialise indexed repository")
            })
            .transpose()
    }

    pub fn commit_tree(&self, database: &sled::Db, reference: &str) -> Result<CommitTree> {
        let tree = database
            .open_tree(TreePrefix::commit_id(self.id, reference))
            .context("Failed to open commit tree")?;

        Ok(CommitTree::new(tree))
    }

    pub fn tag_tree(&self, database: &sled::Db) -> Result<TagTree> {
        let tree = database
            .open_tree(TreePrefix::tag_id(self.id))
            .context("Failed to open tag tree")?;

        Ok(TagTree::new(tree))
    }

    pub fn heads(&self, database: &sled::Db) -> Vec<String> {
        let prefix = TreePrefix::commit_id(self.id, "");

        database
            .tree_names()
            .into_iter()
            .filter_map(|v| {
                v.strip_prefix(prefix.as_bytes())
                    .map(|v| String::from_utf8_lossy(v).into_owned())
            })
            .collect()
    }
}

#[derive(Serialize, Deserialize, Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub struct RepositoryId(pub(super) u64);

impl RepositoryId {
    pub fn new(db: &sled::Db) -> Self {
        Self(db.generate_id().unwrap())
    }
}

impl Deref for RepositoryId {
    type Target = u64;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
