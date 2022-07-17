use crate::database::schema::commit::CommitTree;
use crate::database::schema::prefixes::TreePrefix;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::ops::Deref;
use std::path::Path;
use time::OffsetDateTime;

#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Hash)]
pub struct Repository {
    /// The ID of the repository, as stored in `sled`
    pub id: RepositoryId,
    /// The "clean name" of the repository (ie. `hello-world.git`)
    pub name: String,
    /// The description of the repository, as it is stored in the `description` file in the
    /// bare repo root
    pub description: Option<String>,
    /// The owner of the repository (`gitweb.owner` in the repository configuration)
    pub owner: Option<String>,
    /// The last time this repository was updated, currently read from the directory mtime
    pub last_modified: OffsetDateTime,
}

impl Repository {
    pub fn fetch_all(database: &sled::Db) -> HashMap<String, Repository> {
        database
            .scan_prefix([TreePrefix::Repository as u8])
            .filter_map(Result::ok)
            .map(|(k, v)| {
                // strip the prefix we've just scanned for
                let key = String::from_utf8_lossy(&k[1..]).to_string();
                let value = bincode::deserialize(&v).unwrap();

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

    pub fn open<P: AsRef<Path>>(database: &sled::Db, path: P) -> Option<Repository> {
        database
            .get(TreePrefix::repository_id(path))
            .unwrap()
            .map(|v| bincode::deserialize(&v))
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
