use std::str::FromStr;

use anyhow::{bail, Context, Result};
use axum::{
    body::{boxed, Body},
    http::{header::HeaderName, HeaderValue},
    response::Response,
};
use httparse::Status;

// https://en.wikipedia.org/wiki/Common_Gateway_Interface
pub fn cgi_to_response(buffer: &[u8]) -> Result<Response> {
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

    Ok(response)
}
