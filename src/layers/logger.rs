//! Logs each and every request out in a format similar to that of Apache's logs.

use std::{
    fmt::Debug,
    future::Future,
    net::SocketAddr,
    task::{Context, Poll},
    time::Instant,
};

use axum::{
    extract,
    http::{HeaderValue, Method, Request, Response},
};
use futures_util::future::{FutureExt, Join, Map, Ready};
use tokio::task::futures::TaskLocalFuture;
use tower_service::Service;
use tracing::{error, info, instrument::Instrumented, Instrument, Span};
use uuid::Uuid;

use super::UnwrapInfallible;

pub trait GenericError: std::error::Error + Debug + Send + Sync {}

#[derive(Clone)]
pub struct LoggingMiddleware<S>(pub S);

impl<S, ReqBody, ResBody> Service<Request<ReqBody>> for LoggingMiddleware<S>
where
    S: Service<Request<ReqBody>, Response = Response<ResBody>, Error = std::convert::Infallible>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
    S::Response: Default + Debug,
    ReqBody: Send + Debug + 'static,
    ResBody: Default + Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = Map<
        Join<TaskLocalFuture<Instant, Instrumented<S::Future>>, Ready<PendingLogMessage>>,
        fn((<S::Future as Future>::Output, PendingLogMessage)) -> <S::Future as Future>::Output,
    >;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.0.poll_ready(cx)
    }

    fn call(&mut self, req: Request<ReqBody>) -> Self::Future {
        let request_id = Uuid::new_v4();
        let span = tracing::info_span!("web", "request_id" = request_id.to_string().as_str());

        let log_message = PendingLogMessage {
            span: span.clone(),
            request_id,
            ip: req
                .extensions()
                .get::<extract::ConnectInfo<std::net::SocketAddr>>()
                .map_or_else(|| "0.0.0.0:0".parse().unwrap(), |v| v.0),
            method: req.method().clone(),
            uri: req.uri().path().to_string(),
            start: Instant::now(),
            user_agent: req.headers().get(axum::http::header::USER_AGENT).cloned(),
        };

        futures_util::future::join(
            REQ_TIMESTAMP.scope(log_message.start, self.0.call(req).instrument(span)),
            futures_util::future::ready(log_message),
        )
        .map(|(response, pending_log_message)| {
            let mut response = response.unwrap_infallible();
            pending_log_message.log(&response);
            response.headers_mut().insert(
                "X-Request-ID",
                HeaderValue::try_from(pending_log_message.request_id.to_string()).unwrap(),
            );
            Ok(response)
        })
    }
}

tokio::task_local! {
    pub static REQ_TIMESTAMP: Instant;
}

pub struct PendingLogMessage {
    span: Span,
    request_id: Uuid,
    ip: SocketAddr,
    method: Method,
    uri: String,
    start: Instant,
    user_agent: Option<HeaderValue>,
}

impl PendingLogMessage {
    pub fn log<ResBody>(&self, response: &Response<ResBody>) {
        let _enter = self.span.enter();

        if response.status().is_server_error() {
            error!(
                "{ip} - \"{method} {uri}\" {status} {duration:?} \"{user_agent}\" \"{error:?}\"",
                ip = self.ip,
                method = self.method,
                uri = self.uri,
                status = response.status().as_u16(),
                duration = self.start.elapsed(),
                user_agent = self
                    .user_agent
                    .as_ref()
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("unknown"),
                error = match response.extensions().get::<Box<dyn GenericError>>() {
                    Some(e) => Err(e),
                    None => Ok(()),
                }
            );
        } else {
            info!(
                "{ip} - \"{method} {uri}\" {status} {duration:?} \"{user_agent}\" \"{error:?}\"",
                ip = self.ip,
                method = self.method,
                uri = self.uri,
                status = response.status().as_u16(),
                duration = self.start.elapsed(),
                user_agent = self
                    .user_agent
                    .as_ref()
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("unknown"),
                error = match response.extensions().get::<Box<dyn GenericError>>() {
                    Some(e) => Err(e),
                    None => Ok(()),
                }
            );
        }
    }
}
