pub fn timeago(s: time::OffsetDateTime) -> Result<String, askama::Error> {
    Ok(timeago::Formatter::new().convert((time::OffsetDateTime::now_utc() - s).unsigned_abs()))
}

pub fn file_perms(s: &i32) -> Result<String, askama::Error> {
    Ok(unix_mode::to_string(s.unsigned_abs()))
}

pub fn hex(s: &[u8]) -> Result<String, askama::Error> {
    Ok(hex::encode(s))
}

pub fn md5(s: &str) -> Result<String, askama::Error> {
    Ok(hex::encode(&md5::compute(s).0))
}
