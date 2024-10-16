// sorry clippy, we don't have a choice. askama forces this on us
#![allow(clippy::unnecessary_wraps, clippy::trivially_copy_pass_by_ref)]

use std::{
    borrow::Borrow,
    collections::HashMap,
    sync::{Arc, LazyLock},
};

use arc_swap::ArcSwap;
use rkyv::{
    rend::{i32_le, i64_le},
    tuple::ArchivedTuple2,
};
use time::{format_description::well_known::Rfc3339, OffsetDateTime, UtcOffset};

// pub fn format_time(s: impl Borrow<time::OffsetDateTime>) -> Result<String, askama::Error> {
pub fn format_time(s: impl Into<Timestamp>) -> Result<String, askama::Error> {
    let s = s.into().0;

    (*s.borrow())
        .format(&Rfc3339)
        .map_err(Box::from)
        .map_err(askama::Error::Custom)
}

pub fn branch_query(branch: Option<&str>) -> String {
    if let Some(b) = branch {
        format!("?h={b}")
    } else {
        String::new()
    }
}

pub fn timeago(s: impl Into<Timestamp>) -> Result<String, askama::Error> {
    Ok(timeago::Formatter::new()
        .convert((OffsetDateTime::now_utc() - s.into().0).try_into().unwrap()))
}

pub fn file_perms(s: &u16) -> Result<String, askama::Error> {
    Ok(unix_mode::to_string(u32::from(*s)))
}

pub fn hex(s: &[u8]) -> Result<String, askama::Error> {
    Ok(const_hex::encode(s))
}

pub fn gravatar(email: &str) -> Result<&'static str, askama::Error> {
    static CACHE: LazyLock<ArcSwap<HashMap<&'static str, &'static str>>> =
        LazyLock::new(|| ArcSwap::new(Arc::new(HashMap::new())));

    if let Some(res) = CACHE.load().get(email).copied() {
        return Ok(res);
    }

    let url = format!(
        "https://www.gravatar.com/avatar/{}",
        const_hex::encode(md5::compute(email).0)
    );
    let key = Box::leak(Box::from(email));
    let url = url.leak();

    CACHE.rcu(|curr| {
        let mut r = (**curr).clone();
        r.insert(key, url);
        r
    });

    Ok(url)
}

pub struct Timestamp(OffsetDateTime);

impl From<&ArchivedTuple2<i64_le, i32_le>> for Timestamp {
    fn from(value: &ArchivedTuple2<i64_le, i32_le>) -> Self {
        Self(
            OffsetDateTime::from_unix_timestamp(value.0.to_native())
                .unwrap()
                .to_offset(UtcOffset::from_whole_seconds(value.1.to_native()).unwrap()),
        )
    }
}

impl From<(i64, i32)> for Timestamp {
    fn from(value: (i64, i32)) -> Self {
        Self(
            OffsetDateTime::from_unix_timestamp(value.0)
                .unwrap()
                .to_offset(UtcOffset::from_whole_seconds(value.1).unwrap()),
        )
    }
}

impl From<&(i64, i32)> for Timestamp {
    fn from(value: &(i64, i32)) -> Self {
        Self(
            OffsetDateTime::from_unix_timestamp(value.0)
                .unwrap()
                .to_offset(UtcOffset::from_whole_seconds(value.1).unwrap()),
        )
    }
}

impl From<OffsetDateTime> for Timestamp {
    fn from(value: OffsetDateTime) -> Self {
        Self(value)
    }
}
