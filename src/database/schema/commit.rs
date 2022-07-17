use serde::{Deserialize, Serialize};
use std::ops::Deref;
use time::OffsetDateTime;

#[derive(Serialize, Deserialize, Debug)]
pub struct Commit {
    pub summary: String,
    pub message: String,
    pub author: Author,
    pub committer: Author,
    pub hash: Vec<u8>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Author {
    pub name: String,
    pub email: String,
    pub time: OffsetDateTime,
}

impl Commit {
    pub fn insert(&self, database: &CommitTree, id: usize) {
        database
            .insert(id.to_be_bytes(), bincode::serialize(self).unwrap())
            .unwrap();
    }
}

pub struct CommitTree(sled::Tree);

impl Deref for CommitTree {
    type Target = sled::Tree;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl CommitTree {
    pub(super) fn new(tree: sled::Tree) -> Self {
        Self(tree)
    }

    pub fn fetch_latest(&self, amount: usize, offset: usize) -> Vec<Commit> {
        let (latest_key, _) = self.last().unwrap().unwrap();
        let mut latest_key_bytes = [0; std::mem::size_of::<usize>()];
        latest_key_bytes.copy_from_slice(&latest_key);

        let end = usize::from_be_bytes(latest_key_bytes).saturating_sub(offset);
        let start = end.saturating_sub(amount);

        self.range(start.to_be_bytes()..end.to_be_bytes())
            .rev()
            .map(|res| {
                let (_, value) = res?;
                let details = bincode::deserialize(&value).unwrap();

                Ok(details)
            })
            .collect::<Result<Vec<_>, sled::Error>>()
            .unwrap()
    }
}
