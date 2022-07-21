use axum::body::{boxed, Body};
use axum::http::header::HeaderName;
use axum::http::HeaderValue;
use axum::response::Response;
use std::str::FromStr;

// https://en.wikipedia.org/wiki/Common_Gateway_Interface
pub fn cgi_to_response(buffer: &[u8]) -> Response {
    let mut headers = [httparse::EMPTY_HEADER; 10];
    let (body_offset, headers) = httparse::parse_headers(buffer, &mut headers)
        .unwrap()
        .unwrap();

    let mut response = Response::new(boxed(Body::from(buffer[body_offset..].to_vec())));

    // TODO: extract status header
    for header in headers {
        response.headers_mut().insert(
            HeaderName::from_str(header.name).unwrap(),
            HeaderValue::from_bytes(header.value).unwrap(),
        );
    }

    response
}
