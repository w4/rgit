use crate::database::schema::repository::RepositoryId;
use std::path::Path;

#[repr(u8)]
pub enum TreePrefix {
    Repository = 0,
    Commit = 100,
    _Tag = 101,
}

impl TreePrefix {
    pub fn repository_id<T: AsRef<Path>>(path: T) -> Vec<u8> {
        let path = path.as_ref().to_string_lossy();
        let path_bytes = path.as_bytes();

        let mut prefixed = Vec::with_capacity(path_bytes.len() + std::mem::size_of::<TreePrefix>());
        prefixed.push(Self::Repository as u8);
        prefixed.extend_from_slice(path_bytes);

        prefixed
    }

    pub fn commit_id<T: AsRef<[u8]>>(repository: RepositoryId, commit: T) -> Vec<u8> {
        let commit = commit.as_ref();

        let mut prefixed = Vec::with_capacity(
            commit.len() + std::mem::size_of::<RepositoryId>() + std::mem::size_of::<TreePrefix>(),
        );
        prefixed.push(TreePrefix::Commit as u8);
        prefixed.extend_from_slice(&repository.to_ne_bytes());
        prefixed.extend_from_slice(&commit);

        prefixed
    }
}
