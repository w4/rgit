use std::collections::BTreeMap;

use anyhow::Context;
use gix::{bstr::BStr, ObjectId};
use itertools::{Either, Itertools};
use rkyv::{Archive, Serialize};
use rocksdb::{WriteBatch, DB};
use yoke::{Yoke, Yokeable};

use super::{
    prefixes::{TREE_FAMILY, TREE_ITEM_FAMILY},
    Yoked,
};

#[derive(Serialize, Archive, Debug, PartialEq, Eq, Hash)]
pub struct Tree {
    pub indexed_tree_id: u64,
}

impl Tree {
    pub fn insert(
        &self,
        database: &DB,
        batch: &mut WriteBatch,
        tree_oid: ObjectId,
    ) -> Result<(), anyhow::Error> {
        let cf = database
            .cf_handle(TREE_FAMILY)
            .context("tree column family missing")?;

        batch.put_cf(
            cf,
            tree_oid.as_slice(),
            rkyv::to_bytes::<rkyv::rancor::Error>(self)?,
        );

        Ok(())
    }

    pub fn find(database: &DB, tree_oid: ObjectId) -> Result<Option<u64>, anyhow::Error> {
        let cf = database
            .cf_handle(TREE_FAMILY)
            .context("tree column family missing")?;

        let Some(data) = database.get_pinned_cf(cf, tree_oid.as_slice())? else {
            return Ok(None);
        };

        let data = rkyv::access::<<Self as Archive>::Archived, rkyv::rancor::Error>(data.as_ref())?;

        Ok(Some(data.indexed_tree_id.to_native()))
    }
}

#[derive(Serialize, Archive, Debug, PartialEq, Eq, Hash, Ord, PartialOrd)]
#[rkyv(derive(Ord, PartialOrd, Eq, PartialEq, Debug))]
#[rkyv(compare(PartialOrd, PartialEq))]
pub struct TreeKey(pub String);

#[derive(Serialize, Archive, Debug, PartialEq, Eq, Default, Yokeable)]
pub struct SortedTree(pub BTreeMap<TreeKey, SortedTreeItem>);

impl SortedTree {
    pub fn insert(
        &self,
        digest: u64,
        database: &DB,
        batch: &mut WriteBatch,
    ) -> Result<(), anyhow::Error> {
        let cf = database
            .cf_handle(TREE_ITEM_FAMILY)
            .context("tree column family missing")?;

        batch.put_cf(
            cf,
            digest.to_ne_bytes(),
            rkyv::to_bytes::<rkyv::rancor::Error>(self)?,
        );

        Ok(())
    }

    pub fn get(digest: u64, database: &DB) -> Result<Option<YokedSortedTree>, anyhow::Error> {
        let cf = database
            .cf_handle(TREE_ITEM_FAMILY)
            .expect("tree column family missing");

        database
            .get_cf(cf, digest.to_ne_bytes())?
            .map(|data| {
                Yoke::try_attach_to_cart(data.into_boxed_slice(), |data| {
                    rkyv::access::<_, rkyv::rancor::Error>(data)
                })
            })
            .transpose()
            .context("failed to parse full tree")
    }
}

#[derive(Serialize, Archive, Debug, PartialEq, Eq)]
#[rkyv(
    bytecheck(bounds(__C: rkyv::validation::ArchiveContext)),
    serialize_bounds(__S: rkyv::ser::Writer + rkyv::ser::Allocator, __S::Error: rkyv::rancor::Source),
)]
pub enum SortedTreeItem {
    File,
    Directory(#[rkyv(omit_bounds)] SortedTree),
}

#[derive(Serialize, Archive, Debug, PartialEq, Eq, Hash)]
pub struct Submodule {
    pub url: String,
    pub oid: [u8; 20],
}

#[derive(Serialize, Archive, Debug, PartialEq, Eq, Hash)]
pub enum TreeItemKind {
    Submodule(Submodule),
    Tree,
    File,
}

#[derive(Serialize, Archive, Debug, PartialEq, Eq, Hash, Yokeable)]
pub struct TreeItem {
    pub mode: u16,
    pub kind: TreeItemKind,
}

pub type YokedSortedTree = Yoked<&'static <SortedTree as Archive>::Archived>;
pub type YokedTreeItem = Yoked<&'static <TreeItem as Archive>::Archived>;
pub type YokedTreeItemKey = Yoked<&'static [u8]>;
pub type YokedTreeItemKeyUtf8 = Yoked<&'static str>;

impl TreeItem {
    pub fn insert(
        &self,
        buffer: &mut Vec<u8>,
        digest: u64,
        path: &BStr,
        database: &DB,
        batch: &mut WriteBatch,
    ) -> Result<(), anyhow::Error> {
        let cf = database
            .cf_handle(TREE_ITEM_FAMILY)
            .context("tree column family missing")?;

        buffer.clear();
        buffer.reserve(std::mem::size_of::<u64>() + path.len() + std::mem::size_of::<usize>());
        buffer.extend_from_slice(&digest.to_ne_bytes());
        buffer.extend_from_slice(&memchr::memchr_iter(b'/', path).count().to_be_bytes());
        buffer.extend_from_slice(path.as_ref());

        batch.put_cf(cf, &buffer, rkyv::to_bytes::<rkyv::rancor::Error>(self)?);

        Ok(())
    }

    pub fn find_exact(
        database: &DB,
        digest: u64,
        path: &[u8],
    ) -> Result<Option<YokedTreeItem>, anyhow::Error> {
        let cf = database
            .cf_handle(TREE_ITEM_FAMILY)
            .expect("tree column family missing");

        let mut buffer = Vec::with_capacity(std::mem::size_of::<u64>() + path.len());
        buffer.extend_from_slice(&digest.to_ne_bytes());
        buffer.extend_from_slice(&memchr::memchr_iter(b'/', path).count().to_be_bytes());
        buffer.extend_from_slice(path);

        database
            .get_cf(cf, buffer)?
            .map(|data| {
                Yoke::try_attach_to_cart(data.into_boxed_slice(), |data| {
                    rkyv::access::<_, rkyv::rancor::Error>(data)
                })
            })
            .transpose()
            .context("failed to parse tree item")
    }

    pub fn find_prefix<'a>(
        database: &'a DB,
        digest: u64,
        prefix: Option<&[u8]>,
    ) -> impl Iterator<Item = Result<(YokedTreeItemKey, YokedTreeItem), anyhow::Error>> + use<'a>
    {
        let cf = database
            .cf_handle(TREE_ITEM_FAMILY)
            .expect("tree column family missing");

        let (iterator, key) = match prefix {
            None => {
                let iterator = database.prefix_iterator_cf(cf, digest.to_ne_bytes());

                (iterator, Either::Left(Either::Left(digest.to_be_bytes())))
            }
            Some([]) => {
                let mut buffer = [0_u8; std::mem::size_of::<u64>() + std::mem::size_of::<usize>()];
                buffer[..std::mem::size_of::<u64>()].copy_from_slice(&digest.to_ne_bytes());
                buffer[std::mem::size_of::<u64>()..].copy_from_slice(&0_usize.to_be_bytes());

                let iterator = database.prefix_iterator_cf(cf, buffer);

                (iterator, Either::Left(Either::Right(buffer)))
            }
            Some(prefix) => {
                let mut buffer = Vec::with_capacity(
                    std::mem::size_of::<u64>() + prefix.len() + std::mem::size_of::<usize>(),
                );
                buffer.extend_from_slice(&digest.to_ne_bytes());
                buffer.extend_from_slice(
                    &(memchr::memchr_iter(b'/', prefix).count() + 1).to_be_bytes(),
                );
                buffer.extend_from_slice(prefix);
                buffer.push(b'/');

                let iterator = database.prefix_iterator_cf(cf, &buffer);

                (iterator, Either::Right(buffer))
            }
        };

        iterator
            .take_while(move |v| {
                v.as_ref().is_ok_and(|(k, _)| {
                    k.starts_with(match key.as_ref() {
                        Either::Left(Either::Right(v)) => v.as_ref(),
                        Either::Left(Either::Left(v)) => v.as_ref(),
                        Either::Right(v) => v.as_ref(),
                    })
                })
            })
            .map_ok(|(key, value)| {
                let key = Yoke::attach_to_cart(key, |data| {
                    &data[std::mem::size_of::<u64>() + std::mem::size_of::<usize>()..]
                });
                let value = Yoke::try_attach_to_cart(value, |data| {
                    rkyv::access::<_, rkyv::rancor::Error>(data)
                })
                .context("Failed to open repository")?;
                Ok((key, value))
            })
            .flatten()
    }

    pub fn contains(database: &DB, digest: u64) -> Result<bool, anyhow::Error> {
        let cf = database
            .cf_handle(TREE_ITEM_FAMILY)
            .context("tree column family missing")?;

        Ok(database
            .prefix_iterator_cf(cf, digest.to_ne_bytes())
            .next()
            .transpose()?
            .is_some())
    }
}
