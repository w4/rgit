// sorry clippy, we don't have a choice. askama forces this on us
#![allow(clippy::unnecessary_wraps, clippy::trivially_copy_pass_by_ref)]

use std::{
    borrow::Borrow,
    collections::HashMap,
    sync::{Arc, LazyLock},
};

use arc_swap::ArcSwap;
use time::format_description::well_known::Rfc3339;

pub fn format_time(s: impl Borrow<time::OffsetDateTime>) -> Result<String, askama::Error> {
    (*s.borrow())
        .format(&Rfc3339)
        .map_err(Box::from)
        .map_err(askama::Error::Custom)
}

pub fn timeago(s: impl Borrow<time::OffsetDateTime>) -> Result<String, askama::Error> {
    Ok(timeago::Formatter::new()
        .convert((time::OffsetDateTime::now_utc() - *s.borrow()).unsigned_abs()))
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

#[allow(dead_code)]
pub fn md(md: &str) -> Result<String, askama::Error> {
    Ok(comrak::markdown_to_html(
        md,
        &comrak::ComrakOptions::default(),
    ))
}
