//! Drains the blob queue through blob contract v1 (ADR 0030; port of
//! `Outbox/BlobUploader.cs`): `PUT /sync/v1/blobs/{sha256}` with the raw content, then
//! `POST /sync/v1/blobs/refs` pointing the row at it. Both calls are idempotent, so a crash
//! between them only repeats work. Transient failures, 401 (token refreshed) and 403 keep
//! the entry for the next cycle; any other refusal parks it.

use std::sync::Arc;

use super::batch_uploader::UploadCycleResult;
use super::blobs::{BlobOutboxItem, SqliteBlobOutbox};
use super::error::UploadError;
use crate::config::AgentIdentity;
use crate::contracts::WireJson;
use crate::contracts::wire::{SyncBlobRefBatch, SyncBlobRefsAck, blob_contract};
use crate::reporting::{Logger, Value};
use crate::transport::retry::is_transient;
use crate::transport::{Error, SyncHttpClient, SyncResponse, TokenProvider};
use crate::util::ctx::Ctx;

/// `MM.PdaOrder.SyncAgent.Outbox.BlobUploader`: the log category.
pub const BLOB_UPLOADER_CATEGORY: &str = "MM.PdaOrder.SyncAgent.Outbox.BlobUploader";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Done,
    Retry,
}

/// Uploads queued blobs (`BlobUploader`).
pub struct BlobUploader {
    queue: Arc<SqliteBlobOutbox>,
    agent_id: String,
    http: Arc<SyncHttpClient>,
    tokens: Arc<TokenProvider>,
    log: Logger,
}

impl std::fmt::Debug for BlobUploader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlobUploader").finish_non_exhaustive()
    }
}

/// The `missing` hashes of a refs answer.
fn missing(resp: &SyncResponse) -> Result<Vec<String>, Error> {
    match resp.data()? {
        Some(data) if data.is_object() => SyncBlobRefsAck::from_value(&data)
            .map(|a| a.missing)
            .map_err(|e| Error::Unexpected {
                type_name: "System.Text.Json.JsonException",
                message: e.to_string(),
            }),
        _ => Ok(Vec::new()),
    }
}

impl BlobUploader {
    /// An uploader for the enrolled agent.
    pub fn new(
        queue: Arc<SqliteBlobOutbox>,
        identity: &AgentIdentity,
        http: Arc<SyncHttpClient>,
        tokens: Arc<TokenProvider>,
        log: &Logger,
    ) -> BlobUploader {
        BlobUploader {
            queue,
            agent_id: identity.agent_id.to_string(),
            http,
            tokens,
            log: log.for_category(BLOB_UPLOADER_CATEGORY),
        }
    }

    /// Uploads up to `max_blobs` queued blobs, oldest first.
    pub fn run_cycle(&self, max_blobs: i32, ctx: &Ctx) -> Result<UploadCycleResult, UploadError> {
        let items = self.queue.peek(max_blobs)?;
        if items.is_empty() {
            return Ok(UploadCycleResult::Idle);
        }
        let mut progressed = false;
        for item in &items {
            if self.upload_one(item, ctx)? == Outcome::Retry {
                return Ok(if progressed {
                    UploadCycleResult::Progress
                } else {
                    UploadCycleResult::Blocked
                });
            }
            progressed = true;
        }
        Ok(UploadCycleResult::Progress)
    }

    fn upload_one(&self, item: &BlobOutboxItem, ctx: &Ctx) -> Result<Outcome, UploadError> {
        self.queue.mark_attempt(item.id)?;
        let bearer = self.tokens.get_access_token(ctx)?;
        let put = self.http.put_bytes(
            ctx,
            &blob_contract::content_path(&item.ref_.sha256),
            &item.content,
            blob_contract::CONTENT_TYPE,
            Some(&bearer),
        )?;
        if !put.is_success() {
            return self.refused(item, &put, "content");
        }
        let body = SyncBlobRefBatch {
            agent_id: self.agent_id.clone(),
            contract_version: blob_contract::VERSION,
            refs: vec![item.ref_.clone()],
        }
        .to_json();
        let refs = self
            .http
            .post_json(ctx, blob_contract::REFS_PATH, &body, Some(&bearer))?;
        if !refs.is_success() {
            return self.refused(item, &refs, "ref");
        }
        if missing(&refs)?.contains(&item.ref_.sha256) {
            // The cloud lost the content between the two calls; the next cycle uploads it again.
            self.log.warn(
                "Cloud reports blob {Sha256} for {Table}.{Column} missing after upload; retrying",
                &[
                    Value::from(&item.ref_.sha256),
                    Value::from(&item.ref_.table),
                    Value::from(&item.ref_.column),
                ],
            );
            return Ok(Outcome::Retry);
        }
        self.queue.ack(item.id, &item.ref_.sha256)?;
        Ok(Outcome::Done)
    }

    fn refused(
        &self,
        item: &BlobOutboxItem,
        resp: &SyncResponse,
        step: &str,
    ) -> Result<Outcome, UploadError> {
        match resp.status {
            401 => {
                self.tokens.invalidate();
                Ok(Outcome::Retry)
            }
            403 => {
                self.log.error(
                    "Cloud forbids blob uploads for this agent (HTTP 403); backing off",
                    &[],
                );
                Ok(Outcome::Retry)
            }
            status if is_transient(status) => Ok(Outcome::Retry),
            status => {
                self.queue
                    .quarantine(item.id, &format!("{step} refused with HTTP {status}"))?;
                self.log.error(
                    "Blob {Sha256} for {Table}.{Column} permanently refused at the {Step} step (HTTP {Status}); quarantined",
                    &[
                        Value::from(&item.ref_.sha256),
                        Value::from(&item.ref_.table),
                        Value::from(&item.ref_.column),
                        Value::from(step),
                        Value::from(i32::from(status)),
                    ],
                );
                Ok(Outcome::Done)
            }
        }
    }
}
