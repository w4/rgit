use std::{io::Write, process::Stdio};

use axum::{extract::Query, response::Response, Extension};
use bytes::Bytes;
use serde::Deserialize;

use crate::methods::repo::{RepositoryPath, Result};

#[derive(Deserialize)]
pub struct UriQuery {
    service: String,
}

#[allow(clippy::unused_async)]
pub async fn handle_info_refs(
    Extension(RepositoryPath(repository_path)): Extension<RepositoryPath>,
    Query(query): Query<UriQuery>,
) -> Result<Response> {
    // todo: tokio command
    let out = std::process::Command::new("git")
        .arg("http-backend")
        .env("REQUEST_METHOD", "GET")
        .env("PATH_INFO", "/info/refs")
        .env("GIT_PROJECT_ROOT", repository_path)
        .env("QUERY_STRING", format!("service={}", query.service))
        .output()
        .unwrap();

    Ok(crate::git_cgi::cgi_to_response(&out.stdout)?)
}

#[allow(clippy::unused_async)]
pub async fn handle_git_upload_pack(
    Extension(RepositoryPath(repository_path)): Extension<RepositoryPath>,
    body: Bytes,
) -> Result<Response> {
    // todo: tokio command
    let mut child = std::process::Command::new("git")
        .arg("http-backend")
        // todo: read all this from request
        .env("REQUEST_METHOD", "POST")
        .env("CONTENT_TYPE", "application/x-git-upload-pack-request")
        .env("PATH_INFO", "/git-upload-pack")
        .env("GIT_PROJECT_ROOT", repository_path)
        .stdout(Stdio::piped())
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.as_mut().unwrap().write_all(&body).unwrap();
    let out = child.wait_with_output().unwrap();

    Ok(crate::git_cgi::cgi_to_response(&out.stdout)?)
}
