// sorry clippy, we don't have a choice. askama forces this on us
#![allow(clippy::unnecessary_wraps, clippy::trivially_copy_pass_by_ref)]

use std::borrow::Borrow;
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

pub fn file_perms(s: &i32) -> Result<String, askama::Error> {
    Ok(unix_mode::to_string(s.unsigned_abs()))
}

pub fn hex(s: &[u8]) -> Result<String, askama::Error> {
    Ok(hex::encode(s))
}

pub fn md5(s: &str) -> Result<String, askama::Error> {
    Ok(hex::encode(md5::compute(s).0))
}

#[allow(dead_code)]
pub fn md(md: &str) -> Result<String, askama::Error> {
    Ok(comrak::markdown_to_html(
        md,
        &comrak::ComrakOptions::default(),
    ))
}
