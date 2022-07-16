use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
pub struct Commit {
    age: String,
    message: String,
    author: String,
}

impl Commit {}

pub struct CommitVault {
    _tree: sled::Tree,
}

impl CommitVault {
    pub(super) fn new(tree: sled::Tree) -> Self {
        Self { _tree: tree }
    }
}
