use std::{borrow::Cow, ops::Deref, sync::Arc};

use anyhow::Context;
use git2::{Oid, Signature};
use rocksdb::{IteratorMode, ReadOptions, WriteBatch};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use time::OffsetDateTime;
use tracing::debug;
use yoke::{Yoke, Yokeable};

use crate::database::schema::{
    prefixes::{COMMIT_COUNT_FAMILY, COMMIT_FAMILY},
    repository::RepositoryId,
    Yoked,
};

#[derive(Serialize, Deserialize, Debug, Yokeable)]
pub struct Commit<'a> {
    #[serde(borrow)]
    pub summary: Cow<'a, str>,
    #[serde(borrow)]
    pub message: Cow<'a, str>,
    pub author: Author<'a>,
    pub committer: Author<'a>,
    pub hash: CommitHash<'a>,
}

impl<'a> Commit<'a> {
    pub fn new(
        commit: &'a git2::Commit<'_>,
        author: &'a git2::Signature<'_>,
        committer: &'a git2::Signature<'_>,
    ) -> Self {
        Self {
            summary: commit
                .summary_bytes()
                .map_or(Cow::Borrowed(""), String::from_utf8_lossy),
            message: commit
                .body_bytes()
                .map_or(Cow::Borrowed(""), String::from_utf8_lossy),
            committer: committer.into(),
            author: author.into(),
            hash: CommitHash::Oid(commit.id()),
        }
    }

    pub fn insert(&self, tree: &CommitTree, id: u64, tx: &mut WriteBatch) -> anyhow::Result<()> {
        tree.insert(id, self, tx)
    }
}

#[derive(Debug)]
pub enum CommitHash<'a> {
    Oid(Oid),
    Bytes(&'a [u8]),
}

impl<'a> Deref for CommitHash<'a> {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        match self {
            CommitHash::Oid(v) => v.as_bytes(),
            CommitHash::Bytes(v) => v,
        }
    }
}

impl Serialize for CommitHash<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            CommitHash::Oid(v) => serializer.serialize_bytes(v.as_bytes()),
            CommitHash::Bytes(v) => serializer.serialize_bytes(v),
        }
    }
}

impl<'a, 'de: 'a> Deserialize<'de> for CommitHash<'a> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let bytes = <&'a [u8]>::deserialize(deserializer)?;
        Ok(Self::Bytes(bytes))
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Author<'a> {
    #[serde(borrow)]
    pub name: Cow<'a, str>,
    #[serde(borrow)]
    pub email: Cow<'a, str>,
    pub time: OffsetDateTime,
}

impl<'a> From<&'a git2::Signature<'_>> for Author<'a> {
    fn from(author: &'a Signature<'_>) -> Self {
        Self {
            name: String::from_utf8_lossy(author.name_bytes()),
            email: String::from_utf8_lossy(author.email_bytes()),
            // TODO: this needs to deal with offset
            time: OffsetDateTime::from_unix_timestamp(author.when().seconds()).unwrap(),
        }
    }
}

pub struct CommitTree {
    db: Arc<rocksdb::DB>,
    pub prefix: Box<[u8]>,
}

pub type YokedCommit = Yoked<Commit<'static>>;

impl CommitTree {
    pub(super) fn new(db: Arc<rocksdb::DB>, repository: RepositoryId, reference: &str) -> Self {
        let mut prefix = Vec::with_capacity(std::mem::size_of::<u64>() + reference.len() + 1);
        prefix.extend_from_slice(&repository.to_be_bytes());
        prefix.extend_from_slice(reference.as_bytes());
        prefix.push(b'\0');

        Self {
            db,
            prefix: prefix.into_boxed_slice(),
        }
    }

    pub fn drop_commits(&self) -> anyhow::Result<()> {
        let mut to = self.prefix.clone();
        *to.last_mut().unwrap() += 1;

        let commit_cf = self
            .db
            .cf_handle(COMMIT_FAMILY)
            .context("commit column family missing")?;
        self.db.delete_range_cf(commit_cf, &self.prefix, &to)?;

        let commit_count_cf = self
            .db
            .cf_handle(COMMIT_COUNT_FAMILY)
            .context("missing column family")?;
        self.db.delete_cf(commit_count_cf, &self.prefix)?;

        Ok(())
    }

    pub fn update_counter(&self, count: u64, tx: &mut WriteBatch) -> anyhow::Result<()> {
        let cf = self
            .db
            .cf_handle(COMMIT_COUNT_FAMILY)
            .context("missing column family")?;

        tx.put_cf(cf, &self.prefix, count.to_be_bytes());

        Ok(())
    }

    pub fn len(&self) -> anyhow::Result<u64> {
        let cf = self
            .db
            .cf_handle(COMMIT_COUNT_FAMILY)
            .context("missing column family")?;

        let Some(res) = self.db.get_pinned_cf(cf, &self.prefix)? else {
            return Ok(0);
        };

        let mut out = [0_u8; std::mem::size_of::<u64>()];
        out.copy_from_slice(&res);
        Ok(u64::from_be_bytes(out))
    }

    fn insert(&self, id: u64, commit: &Commit<'_>, tx: &mut WriteBatch) -> anyhow::Result<()> {
        let cf = self
            .db
            .cf_handle(COMMIT_FAMILY)
            .context("missing column family")?;

        let mut key = self.prefix.to_vec();
        key.extend_from_slice(&id.to_be_bytes());

        tx.put_cf(cf, key, bincode::serialize(commit)?);

        Ok(())
    }

    pub fn fetch_latest_one(&self) -> Result<Option<YokedCommit>, anyhow::Error> {
        let mut key = self.prefix.to_vec();
        key.extend_from_slice(&(self.len()?.saturating_sub(1)).to_be_bytes());

        let cf = self
            .db
            .cf_handle(COMMIT_FAMILY)
            .context("missing column family")?;

        let Some(value) = self.db.get_cf(cf, key)? else {
            return Ok(None);
        };

        Yoke::try_attach_to_cart(Box::from(value), |data| bincode::deserialize(data))
            .map(Some)
            .context("Failed to deserialize commit")
    }

    pub fn fetch_latest(
        &self,
        amount: u64,
        offset: u64,
    ) -> Result<Vec<YokedCommit>, anyhow::Error> {
        let cf = self
            .db
            .cf_handle(COMMIT_FAMILY)
            .context("missing column family")?;

        let latest_commit_id = self.len()?;
        debug!("Searching from latest commit {latest_commit_id}");

        let mut start_key = self.prefix.to_vec();
        start_key.extend_from_slice(
            &latest_commit_id
                .saturating_sub(offset)
                .saturating_sub(amount)
                .to_be_bytes(),
        );

        let mut end_key = self.prefix.to_vec();
        end_key.extend_from_slice(&(latest_commit_id.saturating_sub(offset)).to_be_bytes());

        let mut opts = ReadOptions::default();
        opts.set_iterate_range(start_key.as_slice()..end_key.as_slice());

        opts.set_prefix_same_as_start(true);

        self.db
            .iterator_cf_opt(cf, opts, IteratorMode::End)
            .map(|v| {
                Yoke::try_attach_to_cart(v.context("failed to read commit")?.1, |data| {
                    bincode::deserialize(data).context("failed to deserialize")
                })
            })
            .collect::<Result<Vec<_>, anyhow::Error>>()
    }
}
