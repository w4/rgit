use crate::database::schema::commit::CommitTree;
use crate::database::schema::prefixes::TreePrefix;
use crate::database::schema::Yoked;
use serde::{Deserialize, Serialize};
use sled::IVec;
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::ops::Deref;
use std::path::Path;
use time::OffsetDateTime;
use yoke::{Yoke, Yokeable};

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
}

pub type YokedRepository = Yoked<Repository<'static>>;

impl Repository<'_> {
    pub fn fetch_all(database: &sled::Db) -> BTreeMap<String, YokedRepository> {
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
                    Yoke::try_attach_to_cart(value, |data: &IVec| bincode::deserialize(data))
                        .unwrap();

                (key, value)
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

    pub fn open<P: AsRef<Path>>(database: &sled::Db, path: P) -> Option<YokedRepository> {
        database
            .get(TreePrefix::repository_id(path))
            .unwrap()
            .map(|value| {
                // internally value is an Arc so it should already be stablederef but because
                // of reasons unbeknownst to me, sled has its own Arc implementation so we need
                // to box the value as well to get a stablederef...
                let value = Box::new(value);

                Yoke::try_attach_to_cart(value, |data: &IVec| bincode::deserialize(data))
            })
            .transpose()
            .unwrap()
    }

    pub fn commit_tree(&self, database: &sled::Db, reference: &str) -> CommitTree {
        let tree = database
            .open_tree(TreePrefix::commit_id(self.id, reference))
            .unwrap();

        CommitTree::new(tree)
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
