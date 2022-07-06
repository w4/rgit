pub fn timeago(s: time::OffsetDateTime) -> Result<String, askama::Error> {
    Ok(timeago::Formatter::new().convert((time::OffsetDateTime::now_utc() - s).unsigned_abs()))
}
