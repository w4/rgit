use std::sync::Arc;

use anyhow::Context;
use gix::{actor::SignatureRef, ObjectId};
use rkyv::{Archive, Serialize};
use rocksdb::{IteratorMode, ReadOptions, WriteBatch};
use time::{OffsetDateTime, UtcOffset};
use tracing::debug;
use yoke::{Yoke, Yokeable};

use crate::database::schema::{
    prefixes::{COMMIT_COUNT_FAMILY, COMMIT_FAMILY},
    repository::RepositoryId,
    Yoked,
};

#[derive(Serialize, Archive, Debug, Yokeable)]
pub struct Commit {
    pub summary: String,
    pub message: String,
    pub author: Author,
    pub committer: Author,
    pub hash: [u8; 20],
}

impl Commit {
    pub fn new(
        commit: &gix::Commit<'_>,
        author: SignatureRef<'_>,
        committer: SignatureRef<'_>,
    ) -> Result<Self, anyhow::Error> {
        let message = commit.message()?;

        Ok(Self {
            summary: message.summary().to_string(),
            message: message.body.map(ToString::to_string).unwrap_or_default(),
            committer: committer.try_into()?,
            author: author.try_into()?,
            hash: match commit.id().detach() {
                ObjectId::Sha1(d) => d,
            },
        })
    }

    pub fn insert(&self, tree: &CommitTree, id: u64, tx: &mut WriteBatch) -> anyhow::Result<()> {
        tree.insert(id, self, tx)
    }
}

#[derive(Serialize, Archive, Debug)]
pub struct Author {
    pub name: String,
    pub email: String,
    pub time: (i64, i32),
}

impl ArchivedAuthor {
    pub fn time(&self) -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(self.time.0.to_native())
            .unwrap()
            .to_offset(UtcOffset::from_whole_seconds(self.time.1.to_native()).unwrap())
    }
}

impl TryFrom<SignatureRef<'_>> for Author {
    type Error = anyhow::Error;

    fn try_from(author: SignatureRef<'_>) -> Result<Self, anyhow::Error> {
        Ok(Self {
            name: author.name.to_string(),
            email: author.email.to_string(),
            time: (author.time.seconds, author.time.offset),
        })
    }
}

pub struct CommitTree {
    db: Arc<rocksdb::DB>,
    pub prefix: Box<[u8]>,
}

pub type YokedCommit = Yoked<&'static <Commit as Archive>::Archived>;

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

        let out: [u8; std::mem::size_of::<u64>()] = res.as_ref().try_into()?;
        Ok(u64::from_be_bytes(out))
    }

    fn insert(&self, id: u64, commit: &Commit, tx: &mut WriteBatch) -> anyhow::Result<()> {
        let cf = self
            .db
            .cf_handle(COMMIT_FAMILY)
            .context("missing column family")?;

        let mut key = self.prefix.to_vec();
        key.extend_from_slice(&id.to_be_bytes());

        tx.put_cf(cf, key, rkyv::to_bytes::<rkyv::rancor::Error>(commit)?);

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

        Yoke::try_attach_to_cart(Box::from(value), |value| {
            rkyv::access::<_, rkyv::rancor::Error>(value)
        })
        .context("Failed to deserialize commit")
        .map(Some)
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
                    rkyv::access::<_, rkyv::rancor::Error>(data).context("failed to deserialize")
                })
            })
            .collect::<Result<Vec<_>, anyhow::Error>>()
    }
}
