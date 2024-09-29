use std::{io, io::ErrorKind, path::Path, process::Stdio, str::FromStr};

use anyhow::{anyhow, Context};
use axum::{
    body::Body,
    http::{
        header::{HeaderMap, HeaderName, HeaderValue},
        Method, Uri,
    },
    response::{IntoResponse, Response},
    Extension,
};
use bytes::{Buf, Bytes, BytesMut};
use futures_util::TryStreamExt;
use httparse::Status;
use tokio::{
    io::AsyncReadExt,
    process::{Child, ChildStderr, ChildStdout, Command},
    sync::mpsc,
};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::io::StreamReader;
use tracing::{debug, error, info_span, warn, Instrument};

use crate::{
    methods::repo::{Repository, RepositoryPath, Result},
    StatusCode,
};

#[allow(clippy::unused_async)]
pub async fn handle(
    Extension(RepositoryPath(repository_path)): Extension<RepositoryPath>,
    Extension(Repository(repository)): Extension<Repository>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
) -> Result<impl IntoResponse> {
    let path = extract_path(&uri, &repository)?;

    let mut command = Command::new("git");

    for (header, env) in [
        ("Content-Type", "CONTENT_TYPE"),
        ("Content-Length", "CONTENT_LENGTH"),
        ("Git-Protocol", "GIT_PROTOCOL"),
        ("Content-Encoding", "HTTP_CONTENT_ENCODING"),
    ] {
        extract_header(&headers, &mut command, header, env)?;
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
        .kill_on_drop(true)
        .spawn()
        .context("Failed to spawn git http-backend")?;

    let mut stdout = child.stdout.take().context("Stdout already taken")?;
    let mut stderr = child.stderr.take().context("Stderr already taken")?;
    let mut stdin = child.stdin.take().context("Stdin already taken")?;

    // read request body and forward to stdin
    let mut body = StreamReader::new(
        body.into_data_stream()
            .map_err(|e| std::io::Error::new(ErrorKind::Other, e)),
    );
    tokio::io::copy_buf(&mut body, &mut stdin)
        .await
        .context("Failed to copy bytes from request to command stdin")?;

    // wait for the headers back from git http-backend
    let mut out_buf = BytesMut::with_capacity(1024);
    let headers = loop {
        let n = stdout
            .read_buf(&mut out_buf)
            .await
            .context("Failed to read headers")?;
        if n == 0 {
            break None;
        }

        if let Some((body_offset, headers)) = parse_cgi_headers(&out_buf)? {
            out_buf.advance(body_offset);
            break Some(headers);
        }
    };

    // if the `headers` loop broke with `None`, the `git http-backend` didn't return any parseable
    // headers so there's no reason for us to continue. there may be something in stderr for us
    // though.
    let Some(headers) = headers else {
        print_status(&mut child, &mut stderr).await;
        return Err(anyhow!("Received incomplete response from git http-backend").into());
    };

    // stream the response back to the client
    let (body_send, body_recv) = mpsc::channel(8);
    tokio::spawn(
        forward_response_to_client(out_buf, body_send, stdout, stderr, child)
            .instrument(info_span!("git http-backend reader")),
    );

    Ok((headers, Body::from_stream(ReceiverStream::new(body_recv))))
}

/// Forwards the entirety of `stdout` to `body_send`, printing subprocess stderr and status on
/// completion.
async fn forward_response_to_client(
    mut out_buf: BytesMut,
    body_send: mpsc::Sender<Result<Bytes, io::Error>>,
    mut stdout: ChildStdout,
    mut stderr: ChildStderr,
    mut child: Child,
) {
    loop {
        let (out, mut end) = match stdout.read_buf(&mut out_buf).await {
            Ok(0) => (Ok(out_buf.split().freeze()), true),
            Ok(n) => (Ok(out_buf.split_to(n).freeze()), false),
            Err(e) => (Err(e), true),
        };

        if body_send.send(out).await.is_err() {
            warn!("Receiver went away during git http-backend call");
            end = true;
        }

        if end {
            break;
        }
    }

    print_status(&mut child, &mut stderr).await;
}

/// Prints the exit status of the `git` subprocess.
async fn print_status(child: &mut Child, stderr: &mut ChildStderr) {
    match tokio::try_join!(child.wait(), read_stderr(stderr)) {
        Ok((status, stderr)) if status.success() => {
            debug!(stderr, "git http-backend successfully shutdown");
        }
        Ok((status, stderr)) => error!(stderr, "git http-backend exited with status code {status}"),
        Err(e) => error!("Failed to wait on git http-backend shutdown: {e}"),
    }
}

/// Reads the entirety of stderr for the given handle.
async fn read_stderr(stderr: &mut ChildStderr) -> io::Result<String> {
    let mut stderr_out = Vec::new();
    stderr.read_to_end(&mut stderr_out).await?;
    Ok(String::from_utf8_lossy(&stderr_out).into_owned())
}

/// Extracts a single header (`header`) from the `input` and passes it as `env` to
/// `output`.
fn extract_header(input: &HeaderMap, output: &mut Command, header: &str, env: &str) -> Result<()> {
    if let Some(value) = input.get(header) {
        output.env(env, value.to_str().context("Invalid header")?);
    }

    Ok(())
}

/// Extract the path from the URL to determine the repository path.
fn extract_path<'a>(uri: &'a Uri, repository: &Path) -> Result<&'a str> {
    let path = uri.path();
    let path = path.strip_prefix('/').unwrap_or(path);

    if let Some(prefix) = repository.as_os_str().to_str() {
        Ok(path.strip_prefix(prefix).unwrap_or(path))
    } else {
        Err(anyhow::Error::msg("Repository name contains invalid bytes").into())
    }
}

// Intercept headers from the spawned `git http-backend` CGI and rewrite them to
// an `axum::Response`.
pub fn parse_cgi_headers(buffer: &[u8]) -> Result<Option<(usize, Response<()>)>, anyhow::Error> {
    let mut headers = [httparse::EMPTY_HEADER; 10];
    let (body_offset, headers) = match httparse::parse_headers(buffer, &mut headers)? {
        Status::Complete(v) => v,
        Status::Partial => return Ok(None),
    };

    let mut response = Response::new(());

    for header in headers {
        response.headers_mut().insert(
            HeaderName::from_str(header.name)
                .context("Failed to parse header name from Git over CGI")?,
            HeaderValue::from_bytes(header.value)
                .context("Failed to parse header value from Git over CGI")?,
        );
    }

    if let Some(status) = response
        .headers_mut()
        .remove("Status")
        .filter(|s| s.len() >= 3)
    {
        let status = &status.as_ref()[..3];

        if let Ok(status) = StatusCode::from_bytes(status) {
            *response.status_mut() = status;
        }
    }

    Ok(Some((body_offset, response)))
}
