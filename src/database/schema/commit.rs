use crate::database::schema::Yoked;
use git2::{Oid, Signature};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sled::IVec;
use std::borrow::Cow;
use std::ops::Deref;
use time::OffsetDateTime;
use yoke::{Yoke, Yokeable};

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

    pub fn insert(&self, batch: &CommitTree, id: usize) {
        batch
            .insert(&id.to_be_bytes(), bincode::serialize(self).unwrap())
            .unwrap();
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
            CommitHash::Oid(v) => v.as_bytes().serialize(serializer),
            CommitHash::Bytes(v) => v.serialize(serializer),
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
    pub name: Cow<'a, str>,
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

pub struct CommitTree(sled::Tree);

impl Deref for CommitTree {
    type Target = sled::Tree;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

pub type YokedCommit = Yoked<Commit<'static>>;

impl CommitTree {
    pub(super) fn new(tree: sled::Tree) -> Self {
        Self(tree)
    }

    pub fn fetch_latest_one(&self) -> Option<YokedCommit> {
        self.last().unwrap().map(|(_, value)| {
            // internally value is an Arc so it should already be stablederef but because
            // of reasons unbeknownst to me, sled has its own Arc implementation so we need
            // to box the value as well to get a stablederef...
            let value = Box::new(value);

            Yoke::try_attach_to_cart(value, |data: &IVec| bincode::deserialize(data)).unwrap()
        })
    }

    pub async fn fetch_latest(&self, amount: usize, offset: usize) -> Vec<YokedCommit> {
        let latest_key = if let Some((latest_key, _)) = self.last().unwrap() {
            let mut latest_key_bytes = [0; std::mem::size_of::<usize>()];
            latest_key_bytes.copy_from_slice(&latest_key);
            usize::from_be_bytes(latest_key_bytes)
        } else {
            return vec![];
        };

        let end = latest_key.saturating_sub(offset);
        let start = end.saturating_sub(amount);

        let iter = self.range(start.to_be_bytes()..end.to_be_bytes());

        tokio::task::spawn_blocking(move || {
            iter.rev()
                .map(|res| {
                    let (_, value) = res?;

                    // internally value is an Arc so it should already be stablederef but because
                    // of reasons unbeknownst to me, sled has its own Arc implementation so we need
                    // to box the value as well to get a stablederef...
                    let value = Box::new(value);

                    Ok(
                        Yoke::try_attach_to_cart(value, |data: &IVec| bincode::deserialize(data))
                            .unwrap(),
                    )
                })
                .collect::<Result<Vec<_>, sled::Error>>()
                .unwrap()
        })
        .await
        .unwrap()
    }
}
