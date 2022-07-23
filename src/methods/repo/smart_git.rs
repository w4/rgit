use std::{io::ErrorKind, path::PathBuf, process::Stdio, str::FromStr};

use anyhow::{bail, Context};
use axum::{
    body::{boxed, Body},
    extract::BodyStream,
    headers::{ContentType, HeaderName, HeaderValue},
    http::{Method, Uri},
    response::Response,
    Extension, TypedHeader,
};
use futures::TryStreamExt;
use httparse::Status;
use tokio_util::io::StreamReader;
use tracing::warn;

use crate::methods::repo::{Repository, RepositoryPath, Result};
use crate::StatusCode;

#[allow(clippy::unused_async)]
pub async fn handle(
    Extension(RepositoryPath(repository_path)): Extension<RepositoryPath>,
    Extension(Repository(repository)): Extension<Repository>,
    method: Method,
    uri: Uri,
    body: BodyStream,
    content_type: Option<TypedHeader<ContentType>>,
) -> Result<Response> {
    let path = extract_path(&uri, &repository)?;

    let mut command = tokio::process::Command::new("git");

    if let Some(content_type) = content_type {
        command.env("CONTENT_TYPE", content_type.0.to_string());
    }

    let mut child = command
        .arg("http-backend")
        .env("REQUEST_METHOD", method.as_str())
        .env("PATH_INFO", path)
        .env("GIT_PROJECT_ROOT", repository_path)
        .env("QUERY_STRING", uri.query().unwrap_or(""))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("Failed to spawn git http-backend")?;

    {
        let mut body =
            StreamReader::new(body.map_err(|e| std::io::Error::new(ErrorKind::Other, e)));
        let mut stdin = child.stdin.take().context("Stdin already taken")?;

        tokio::io::copy(&mut body, &mut stdin)
            .await
            .context("Failed to copy bytes from request to command stdin")?;
    }

    let out = child
        .wait_with_output()
        .await
        .context("Failed to read git http-backend response")?;
    let resp = cgi_to_response(&out.stdout)?;

    if out.stderr.len() > 0 {
        warn!("Git returned an error: `{}`", String::from_utf8_lossy(&out.stderr));
    }

    Ok(resp)
}

fn extract_path<'a>(uri: &'a Uri, repository: &PathBuf) -> Result<&'a str> {
    let path = uri.path();
    let path = path.strip_prefix("/").unwrap_or(path);

    if let Some(prefix) = repository.as_os_str().to_str() {
        Ok(path.strip_prefix(prefix).unwrap_or(path))
    } else {
        Err(anyhow::Error::msg("Repository name contains invalid bytes").into())
    }
}

// https://en.wikipedia.org/wiki/Common_Gateway_Interface
pub fn cgi_to_response(buffer: &[u8]) -> Result<Response, anyhow::Error> {
    let mut headers = [httparse::EMPTY_HEADER; 10];
    let (body_offset, headers) = match httparse::parse_headers(buffer, &mut headers)? {
        Status::Complete(v) => v,
        Status::Partial => bail!("Git returned a partial response over CGI"),
    };

    let mut response = Response::new(boxed(Body::from(buffer[body_offset..].to_vec())));

    // TODO: extract status header
    for header in headers {
        response.headers_mut().insert(
            HeaderName::from_str(header.name)
                .context("Failed to parse header name from Git over CGI")?,
            HeaderValue::from_bytes(header.value)
                .context("Failed to parse header value from Git over CGI")?,
        );
    }

    if let Some(status) = response.headers_mut().remove("Status").filter(|s| s.len() >= 3) {
        let status = &status.as_ref()[..3];

        if let Ok(status) = StatusCode::from_bytes(status) {
            *response.status_mut() = status;
        }
    }

    Ok(response)
}
