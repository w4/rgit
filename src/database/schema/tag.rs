use std::{collections::HashSet, ops::Deref};

use git2::Signature;
use serde::{Deserialize, Serialize};
use sled::IVec;
use yoke::{Yoke, Yokeable};

use crate::database::schema::{commit::Author, Yoked};

#[derive(Serialize, Deserialize, Debug, Yokeable)]
pub struct Tag<'a> {
    #[serde(borrow)]
    pub tagger: Option<Author<'a>>,
}

impl<'a> Tag<'a> {
    pub fn new(tagger: Option<&'a Signature<'_>>) -> Self {
        Self {
            tagger: tagger.map(Into::into),
        }
    }

    pub fn insert(&self, batch: &TagTree, name: &str) {
        batch
            .insert(&name.as_bytes(), bincode::serialize(self).unwrap())
            .unwrap();
    }
}

pub struct TagTree(sled::Tree);

impl Deref for TagTree {
    type Target = sled::Tree;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

pub type YokedTag = Yoked<Tag<'static>>;

impl TagTree {
    pub(super) fn new(tree: sled::Tree) -> Self {
        Self(tree)
    }

    pub fn remove(&self, name: &str) -> bool {
        self.0.remove(name).unwrap().is_some()
    }

    pub fn list(&self) -> HashSet<String> {
        self.iter()
            .keys()
            .filter_map(Result::ok)
            .map(|v| String::from_utf8_lossy(&v).into_owned())
            .collect()
    }

    pub fn fetch_all(&self) -> Vec<(String, YokedTag)> {
        let mut res = self
            .iter()
            .map(|res| {
                let (name, value) = res?;

                let name = String::from_utf8_lossy(&name)
                    .strip_prefix("refs/tags/")
                    .unwrap()
                    .to_string();

                // internally value is an Arc so it should already be stablederef but because
                // of reasons unbeknownst to me, sled has its own Arc implementation so we need
                // to box the value as well to get a stablederef...
                let value = Box::new(value);

                Ok((
                    name,
                    Yoke::try_attach_to_cart(value, |data: &IVec| bincode::deserialize(data))
                        .unwrap(),
                ))
            })
            .collect::<Result<Vec<(String, YokedTag)>, sled::Error>>()
            .unwrap();

        res.sort_unstable_by(|a, b| {
            let a_tagger = a.1.get().tagger.as_ref().map(|v| v.time);
            let b_tagger = b.1.get().tagger.as_ref().map(|v| v.time);
            b_tagger.cmp(&a_tagger)
        });

        res
    }
}
