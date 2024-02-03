use std::sync::Arc;

use anyhow::{anyhow, Context};
use axum::{body::Body, extract::Query, http::Response, Extension};
use serde::Deserialize;
use tokio_stream::wrappers::ReceiverStream;
use tracing::{error, info_span, Instrument};

use super::{RepositoryPath, Result};
use crate::git::Git;

#[derive(Deserialize)]
pub struct UriQuery {
    #[serde(rename = "h")]
    branch: Option<Arc<str>>,
    id: Option<Arc<str>>,
}

pub async fn handle(
    Extension(RepositoryPath(repository_path)): Extension<RepositoryPath>,
    Extension(git): Extension<Arc<Git>>,
    Query(query): Query<UriQuery>,
) -> Result<Response<Body>> {
    let open_repo = git.repo(repository_path, query.branch.clone()).await?;

    // byte stream back to the client
    let (send, recv) = tokio::sync::mpsc::channel(1);

    // channel for `archive` to tell us we can send headers etc back to
    // the user so it has time to return an error
    let (send_cont, recv_cont) = tokio::sync::oneshot::channel();

    let id = query.id.clone();

    let res = tokio::spawn(
        async move {
            if let Err(error) = open_repo
                .archive(send.clone(), send_cont, id.as_deref())
                .await
            {
                error!(%error, "Failed to build archive for client");
                let _res = send.send(Err(anyhow!("archive builder failed"))).await;
                return Err(error);
            }

            Ok(())
        }
        .instrument(info_span!("sender")),
    );

    // don't send any headers until `archive` has told us we're good
    // to continue
    if recv_cont.await.is_err() {
        // sender disappearing means `archive` hit an issue during init, lets
        // wait for the error back from the spawned tokio task to return to
        // the client
        res.await
            .context("Tokio task failed")?
            .context("Failed to build archive")?;

        // ok, well this isn't ideal. the sender disappeared but we never got
        // an error. this shouldn't be possible, i guess lets just return an
        // internal error
        return Err(anyhow!("Ran into inconsistent error state whilst building archive, please file an issue at https://github.com/w4/rgit/issues").into());
    }

    let file_name = query
        .id
        .as_deref()
        .or(query.branch.as_deref())
        .unwrap_or("main");

    Ok(Response::builder()
        .header("Content-Type", "application/gzip")
        .header(
            "Content-Disposition",
            format!("attachment; filename=\"{file_name}.tar.gz\""),
        )
        .body(Body::from_stream(ReceiverStream::new(recv)))
        .context("failed to build response")?)
}
