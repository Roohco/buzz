//! Scoped peer handoff state: operator grants, the sender request journal and
//! recipient decisions.
//!
//! All three live in the same broker state as the authorities they bind, so a
//! recipient host can check the sender's exact generation and that a request
//! event was really issued under the grant. A recipient whose state does not
//! hold the sender's authority finds no grant and denies (fail closed).
//!
//! Every signed handoff event is stored before publication and marked
//! published afterwards, so a lost relay acknowledgement is reconciled by
//! repeating the same action: the stored event is republished, never re-signed.

use buzz_sdk::broker::actions::job::{
    single_tag, tag, JobAdmission, JobCancelArgs, JobDecision, JobHandoff, JobRequestArgs,
    JobResultArgs, JobTicket, JobVerdict, CAPABILITY_QA_REVIEW_CANDIDATE,
};
use buzz_sdk::broker::BrokerErrorCode;
use serde::{Deserialize, Serialize};

use super::*;

pub(crate) const PEER_TABLES_SQL: [&str; 5] = [
    "CREATE TABLE IF NOT EXISTS broker_peer_grants (
        grant_id TEXT PRIMARY KEY NOT NULL,
        sender_credential_hash BLOB NOT NULL CHECK(length(sender_credential_hash) = 32),
        sender_pubkey TEXT NOT NULL CHECK(length(sender_pubkey) = 64),
        recipient_pubkey TEXT NOT NULL CHECK(length(recipient_pubkey) = 64),
        channel_id TEXT NOT NULL,
        capability TEXT NOT NULL,
        state TEXT NOT NULL CHECK(state IN ('active', 'revoked')),
        FOREIGN KEY (sender_credential_hash) REFERENCES broker_authorities(credential_hash)
    ) STRICT",
    "CREATE UNIQUE INDEX IF NOT EXISTS one_active_peer_grant
     ON broker_peer_grants(sender_pubkey, recipient_pubkey, channel_id, capability)
     WHERE state = 'active'",
    "CREATE TABLE IF NOT EXISTS broker_peer_requests (
        event_id TEXT PRIMARY KEY NOT NULL,
        request_sequence INTEGER NOT NULL UNIQUE,
        sender_credential_hash BLOB NOT NULL,
        grant_id TEXT NOT NULL,
        idempotency_key TEXT NOT NULL,
        pipeline_id TEXT NOT NULL,
        stage_id TEXT NOT NULL,
        candidate_identity TEXT NOT NULL,
        args_digest BLOB NOT NULL CHECK(length(args_digest) = 32),
        event_json TEXT NOT NULL,
        published INTEGER NOT NULL DEFAULT 0 CHECK(published IN (0, 1)),
        cancel_event_json TEXT,
        cancel_published INTEGER NOT NULL DEFAULT 0 CHECK(cancel_published IN (0, 1)),
        UNIQUE (sender_credential_hash, idempotency_key),
        FOREIGN KEY (sender_credential_hash) REFERENCES broker_authorities(credential_hash),
        FOREIGN KEY (grant_id) REFERENCES broker_peer_grants(grant_id)
    ) STRICT",
    "CREATE TABLE IF NOT EXISTS broker_peer_jobs (
        request_event_id TEXT PRIMARY KEY NOT NULL,
        recipient_credential_hash BLOB NOT NULL,
        channel_id TEXT NOT NULL,
        sender_pubkey TEXT NOT NULL,
        idempotency_key TEXT,
        state TEXT NOT NULL
            CHECK(state IN ('accepted', 'denied', 'completed', 'cancelled')),
        reason TEXT,
        ticket_json TEXT,
        decision_event_json TEXT,
        decision_published INTEGER NOT NULL DEFAULT 0 CHECK(decision_published IN (0, 1)),
        result_event_json TEXT,
        result_digest BLOB,
        result_published INTEGER NOT NULL DEFAULT 0 CHECK(result_published IN (0, 1)),
        FOREIGN KEY (recipient_credential_hash) REFERENCES broker_authorities(credential_hash)
    ) STRICT",
    "CREATE UNIQUE INDEX IF NOT EXISTS one_run_per_idempotency_key
     ON broker_peer_jobs(sender_pubkey, idempotency_key)
     WHERE state IN ('accepted', 'completed', 'cancelled')",
];

/// Operator-issued grant, read from a file by the `grant-issue` command.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PeerGrantFile {
    pub(crate) grant_id: String,
    pub(crate) sender: AuthorityIdentity,
    pub(crate) recipient_pubkey: buzz_sdk::broker::PubkeyHex,
    pub(crate) channel_id: String,
    pub(crate) capability: String,
}

/// One step of a peer effect: publish a stored event, or refuse.
pub(crate) enum PeerStep {
    /// Publish `event` (`retry` when it was stored by an earlier attempt),
    /// then mark it published.
    Publish {
        event: nostr::Event,
        relay_url: String,
        retry: bool,
    },
    /// Already published; replay its identity.
    Published(nostr::Event),
    /// Refused before any signature.
    Refuse(BrokerError),
}

/// Outcome of a recipient decision.
pub(crate) struct AdmitStep {
    pub(crate) admission: JobAdmission,
    /// Decision event still to be published (accepted or denied).
    pub(crate) publish: Option<(nostr::Event, bool)>,
    pub(crate) replayed: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JobStatus {
    request_event_id: String,
    channel_id: String,
    sender_pubkey: String,
    state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    decision_event_id: Option<String>,
    decision_published: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    result_event_id: Option<String>,
    result_published: bool,
}

fn refuse(code: BrokerErrorCode, message: &str) -> PeerStep {
    PeerStep::Refuse(BrokerError::new(code, message))
}

fn event_from_json(value: &str) -> Result<nostr::Event, StoreError> {
    decode_effect_event(value)
}

fn event_json(event: &nostr::Event) -> Result<String, StoreError> {
    serde_json::to_string(event).map_err(|_| StoreError::Integrity)
}

impl AuthorityStore {
    pub(super) async fn migrate_schema_v7(&self) -> Result<(), StoreError> {
        let mut connection = self
            .pool
            .acquire()
            .await
            .map_err(|_| StoreError::Unavailable)?;
        let mut transaction = connection
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|_| StoreError::Unavailable)?;
        let current: i64 = sqlx::query_scalar("PRAGMA user_version")
            .fetch_one(&mut *transaction)
            .await
            .map_err(|_| StoreError::Unavailable)?;
        if current != 6 {
            return Ok(());
        }
        for statement in PEER_TABLES_SQL {
            sqlx::query(statement)
                .execute(&mut *transaction)
                .await
                .map_err(|_| StoreError::Unavailable)?;
        }
        sqlx::query("PRAGMA user_version = 7")
            .execute(&mut *transaction)
            .await
            .map_err(|_| StoreError::Unavailable)?;
        transaction
            .commit()
            .await
            .map_err(|_| StoreError::Unavailable)
    }

    /// Issue one grant bound to the sender's exact active generation.
    pub(crate) async fn issue_peer_grant(&self, grant: &PeerGrantFile) -> Result<(), StoreError> {
        let grant_id = uuid::Uuid::parse_str(&grant.grant_id)
            .map_err(|_| StoreError::InvalidIdentity)?
            .as_hyphenated()
            .to_string();
        let channel_id = uuid::Uuid::parse_str(&grant.channel_id)
            .map_err(|_| StoreError::InvalidIdentity)?
            .as_hyphenated()
            .to_string();
        let recipient = buzz_sdk::broker::PubkeyHex::parse(grant.recipient_pubkey.as_str())
            .map_err(|_| StoreError::InvalidIdentity)?;
        if grant.capability != CAPABILITY_QA_REVIEW_CANDIDATE {
            return Err(StoreError::InvalidIdentity);
        }
        let sender = grant
            .sender
            .validated()
            .map_err(|_| StoreError::InvalidIdentity)?;
        if sender.executor_agent_pubkey == recipient {
            return Err(StoreError::InvalidIdentity);
        }
        let authority_json = canonical_identity(&sender)?;
        let mut connection = self
            .pool
            .acquire()
            .await
            .map_err(|_| StoreError::Unavailable)?;
        let mut transaction = connection
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|_| StoreError::Unavailable)?;
        let row = sqlx::query(
            "SELECT credential_hash, state, quiescing FROM broker_authorities
             WHERE authority_json = ?",
        )
        .bind(&authority_json)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|_| StoreError::Unavailable)?
        .ok_or(StoreError::AuthorityNotFound)?;
        let state: String = row.try_get("state").map_err(|_| StoreError::Integrity)?;
        let quiescing: i64 = row
            .try_get("quiescing")
            .map_err(|_| StoreError::Integrity)?;
        if parse_state(&state, quiescing)? != AuthorityState::Active {
            return Err(StoreError::AuthorityFenced);
        }
        let credential_hash: Vec<u8> = row
            .try_get("credential_hash")
            .map_err(|_| StoreError::Integrity)?;
        let existing = sqlx::query(
            "SELECT sender_credential_hash, recipient_pubkey, channel_id, capability, state
             FROM broker_peer_grants WHERE grant_id = ?",
        )
        .bind(&grant_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|_| StoreError::Unavailable)?;
        if let Some(existing) = existing {
            let same = existing
                .try_get::<Vec<u8>, _>("sender_credential_hash")
                .is_ok_and(|hash| hash == credential_hash)
                && existing
                    .try_get::<String, _>("recipient_pubkey")
                    .is_ok_and(|value| value == recipient.as_str())
                && existing
                    .try_get::<String, _>("channel_id")
                    .is_ok_and(|value| value == channel_id)
                && existing
                    .try_get::<String, _>("state")
                    .is_ok_and(|value| value == "active");
            return if same {
                Ok(())
            } else {
                Err(StoreError::Conflict)
            };
        }
        sqlx::query(
            "INSERT INTO broker_peer_grants
             (grant_id, sender_credential_hash, sender_pubkey, recipient_pubkey,
              channel_id, capability, state)
             VALUES (?, ?, ?, ?, ?, ?, 'active')",
        )
        .bind(&grant_id)
        .bind(&credential_hash)
        .bind(sender.executor_agent_pubkey.as_str())
        .bind(recipient.as_str())
        .bind(&channel_id)
        .bind(&grant.capability)
        .execute(&mut *transaction)
        .await
        .map_err(|_| StoreError::ScopeAlreadyActive)?;
        transaction
            .commit()
            .await
            .map_err(|_| StoreError::Unavailable)
    }

    /// Revoke one grant. Idempotent; an unknown grant is an error.
    pub(crate) async fn revoke_peer_grant(&self, grant_id: &str) -> Result<(), StoreError> {
        let grant_id = uuid::Uuid::parse_str(grant_id)
            .map_err(|_| StoreError::InvalidIdentity)?
            .as_hyphenated()
            .to_string();
        let result =
            sqlx::query("UPDATE broker_peer_grants SET state = 'revoked' WHERE grant_id = ?")
                .bind(&grant_id)
                .execute(&self.pool)
                .await
                .map_err(|_| StoreError::Unavailable)?;
        if result.rows_affected() != 1 {
            return Err(StoreError::AuthorityNotFound);
        }
        Ok(())
    }

    /// Readable operator status: grants and recipient decisions, no secrets.
    pub(crate) async fn peer_status(&self) -> Result<Vec<serde_json::Value>, StoreError> {
        let mut lines = Vec::new();
        let grants = sqlx::query(
            "SELECT g.grant_id, g.sender_pubkey, g.recipient_pubkey, g.channel_id,
                    g.capability, g.state, a.generation, a.state AS sender_state,
                    a.quiescing
             FROM broker_peer_grants g
             JOIN broker_authorities a ON a.credential_hash = g.sender_credential_hash
             ORDER BY g.grant_id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|_| StoreError::Unavailable)?;
        for row in grants {
            let sender_state: String = row
                .try_get("sender_state")
                .map_err(|_| StoreError::Integrity)?;
            let quiescing: i64 = row
                .try_get("quiescing")
                .map_err(|_| StoreError::Integrity)?;
            let get = |name: &str| {
                row.try_get::<String, _>(name)
                    .map_err(|_| StoreError::Integrity)
            };
            lines.push(serde_json::json!({
                "type": "grant",
                "grantId": get("grant_id")?,
                "senderPubkey": get("sender_pubkey")?,
                "senderGeneration": get("generation")?,
                "senderGenerationState": match parse_state(&sender_state, quiescing)? {
                    AuthorityState::Active => "active",
                    AuthorityState::Quiescing => "quiescing",
                    AuthorityState::Fenced => "fenced",
                },
                "recipientPubkey": get("recipient_pubkey")?,
                "channelId": get("channel_id")?,
                "capability": get("capability")?,
                "state": get("state")?,
            }));
        }
        let jobs = sqlx::query(
            "SELECT request_event_id, channel_id, sender_pubkey, state, reason,
                    decision_event_json, decision_published, result_event_json, result_published
             FROM broker_peer_jobs ORDER BY rowid",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|_| StoreError::Unavailable)?;
        for row in jobs {
            let id_of = |name: &str| -> Result<Option<String>, StoreError> {
                row.try_get::<Option<String>, _>(name)
                    .map_err(|_| StoreError::Integrity)?
                    .map(|json| event_from_json(&json).map(|event| event.id.to_hex()))
                    .transpose()
            };
            let status = JobStatus {
                request_event_id: row
                    .try_get("request_event_id")
                    .map_err(|_| StoreError::Integrity)?,
                channel_id: row
                    .try_get("channel_id")
                    .map_err(|_| StoreError::Integrity)?,
                sender_pubkey: row
                    .try_get("sender_pubkey")
                    .map_err(|_| StoreError::Integrity)?,
                state: row.try_get("state").map_err(|_| StoreError::Integrity)?,
                reason: row.try_get("reason").map_err(|_| StoreError::Integrity)?,
                decision_event_id: id_of("decision_event_json")?,
                decision_published: row
                    .try_get::<i64, _>("decision_published")
                    .map_err(|_| StoreError::Integrity)?
                    == 1,
                result_event_id: id_of("result_event_json")?,
                result_published: row
                    .try_get::<i64, _>("result_published")
                    .map_err(|_| StoreError::Integrity)?
                    == 1,
            };
            let mut value = serde_json::to_value(status).map_err(|_| StoreError::Integrity)?;
            value["type"] = serde_json::json!("job");
            lines.push(value);
        }
        Ok(lines)
    }

    /// Load the caller's authority and require it active for this signer.
    async fn active_peer_authority(
        transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        credential: &CredentialDigest,
        signer: &HostSigner,
    ) -> Result<Result<StoredAuthority, BrokerError>, StoreError> {
        let authority = load_authority(transaction, credential).await?;
        if !owner_matches(&authority, &signer.owner_hex()) {
            return Ok(Err(BrokerError::unauthorized(
                "authority generation belongs to another verified owner",
            )));
        }
        if authority.state != AuthorityState::Active {
            return Ok(Err(BrokerError::unauthorized(
                "authority generation is fenced",
            )));
        }
        if !signer.matches_executor(&authority.identity) {
            return Ok(Err(BrokerError::unauthorized(
                "host signer does not match the authority executor",
            )));
        }
        bind_owner(transaction, credential, &signer.owner_hex()).await?;
        Ok(Ok(authority))
    }

    /// Sender: sign and journal one request under its active grant.
    pub(crate) async fn prepare_job_request(
        &self,
        credential: &CredentialDigest,
        signer: &HostSigner,
        args: &JobRequestArgs,
    ) -> Result<PeerStep, StoreError> {
        let args_digest: [u8; 32] =
            Sha256::digest(serde_json::to_vec(args).map_err(|_| StoreError::Integrity)?).into();
        let mut connection = self
            .pool
            .acquire()
            .await
            .map_err(|_| StoreError::Unavailable)?;
        let mut transaction = connection
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|_| StoreError::Unavailable)?;
        let authority =
            match Self::active_peer_authority(&mut transaction, credential, signer).await? {
                Ok(authority) => authority,
                Err(error) => return Ok(PeerStep::Refuse(error)),
            };
        let relay_url = authority.identity.community_relay_url.clone();
        let existing = sqlx::query(
            "SELECT args_digest, event_json, published FROM broker_peer_requests
             WHERE sender_credential_hash = ? AND idempotency_key = ?",
        )
        .bind(credential.as_bytes())
        .bind(&args.handoff.idempotency_key)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|_| StoreError::Unavailable)?;
        if let Some(row) = existing {
            let digest: Vec<u8> = row
                .try_get("args_digest")
                .map_err(|_| StoreError::Integrity)?;
            if digest != args_digest {
                return Ok(refuse(
                    BrokerErrorCode::RequestIdConflict,
                    "idempotencyKey was already used with a different request",
                ));
            }
            let event = event_from_json(
                &row.try_get::<String, _>("event_json")
                    .map_err(|_| StoreError::Integrity)?,
            )?;
            let published: i64 = row
                .try_get("published")
                .map_err(|_| StoreError::Integrity)?;
            transaction
                .commit()
                .await
                .map_err(|_| StoreError::Unavailable)?;
            return Ok(if published == 1 {
                PeerStep::Published(event)
            } else {
                PeerStep::Publish {
                    event,
                    relay_url,
                    retry: true,
                }
            });
        }
        let grant_id: Option<String> = sqlx::query_scalar(
            "SELECT grant_id FROM broker_peer_grants
             WHERE sender_credential_hash = ? AND recipient_pubkey = ? AND channel_id = ?
               AND capability = ? AND state = 'active'",
        )
        .bind(credential.as_bytes())
        .bind(args.recipient_pubkey.as_str())
        .bind(&args.channel_id)
        .bind(&args.handoff.capability)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|_| StoreError::Unavailable)?;
        let Some(grant_id) = grant_id else {
            return Ok(refuse(
                BrokerErrorCode::Unauthorized,
                "no active peer grant for this generation, recipient, channel and capability",
            ));
        };
        let short_sha: String = args.handoff.candidate_sha.chars().take(12).collect();
        let event = signer
            .sign_job_event(
                &authority.identity,
                43001,
                &format!(
                    "{} request for candidate {short_sha}",
                    args.handoff.capability
                ),
                &args.channel_id,
                args.recipient_pubkey.as_str(),
                &args.handoff.tag_pairs(),
            )
            .map_err(|_| StoreError::InvalidIdentity)?;
        sqlx::query(
            "INSERT INTO broker_peer_requests
             (event_id, request_sequence, sender_credential_hash, grant_id, idempotency_key,
              pipeline_id, stage_id, candidate_identity, args_digest, event_json)
             VALUES (?, (SELECT COALESCE(MAX(request_sequence), 0) + 1 FROM broker_peer_requests),
                     ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(event.id.to_hex())
        .bind(credential.as_bytes())
        .bind(&grant_id)
        .bind(&args.handoff.idempotency_key)
        .bind(&args.handoff.pipeline_id)
        .bind(&args.handoff.stage_id)
        .bind(candidate_identity(&args.handoff))
        .bind(args_digest.as_slice())
        .bind(event_json(&event)?)
        .execute(&mut *transaction)
        .await
        .map_err(|_| StoreError::Conflict)?;
        transaction
            .commit()
            .await
            .map_err(|_| StoreError::Unavailable)?;
        Ok(PeerStep::Publish {
            event,
            relay_url,
            retry: false,
        })
    }

    pub(crate) async fn mark_peer_published(
        &self,
        column: PeerColumn,
        key: &str,
    ) -> Result<(), StoreError> {
        let sql = match column {
            PeerColumn::Request => {
                "UPDATE broker_peer_requests SET published = 1 WHERE event_id = ?"
            }
            PeerColumn::Cancel => {
                "UPDATE broker_peer_requests SET cancel_published = 1 WHERE event_id = ?"
            }
            PeerColumn::Decision => {
                "UPDATE broker_peer_jobs SET decision_published = 1 WHERE request_event_id = ?"
            }
            PeerColumn::Result => {
                "UPDATE broker_peer_jobs SET result_published = 1 WHERE request_event_id = ?"
            }
        };
        let result = sqlx::query(sql)
            .bind(key)
            .execute(&self.pool)
            .await
            .map_err(|_| StoreError::Unavailable)?;
        if result.rows_affected() != 1 {
            return Err(StoreError::Integrity);
        }
        Ok(())
    }

    /// Sender: withdraw its own request; terminal unless already completed.
    pub(crate) async fn prepare_job_cancel(
        &self,
        credential: &CredentialDigest,
        signer: &HostSigner,
        args: &JobCancelArgs,
    ) -> Result<PeerStep, StoreError> {
        let mut connection = self
            .pool
            .acquire()
            .await
            .map_err(|_| StoreError::Unavailable)?;
        let mut transaction = connection
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|_| StoreError::Unavailable)?;
        let authority =
            match Self::active_peer_authority(&mut transaction, credential, signer).await? {
                Ok(authority) => authority,
                Err(error) => return Ok(PeerStep::Refuse(error)),
            };
        let relay_url = authority.identity.community_relay_url.clone();
        let Some(row) = sqlx::query(
            "SELECT event_json, cancel_event_json, cancel_published FROM broker_peer_requests
             WHERE event_id = ? AND sender_credential_hash = ?",
        )
        .bind(&args.request_event_id)
        .bind(credential.as_bytes())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|_| StoreError::Unavailable)?
        else {
            return Ok(refuse(
                BrokerErrorCode::InvalidRequest,
                "request was not issued by this generation",
            ));
        };
        if let Some(cancel_json) = row
            .try_get::<Option<String>, _>("cancel_event_json")
            .map_err(|_| StoreError::Integrity)?
        {
            let event = event_from_json(&cancel_json)?;
            let published: i64 = row
                .try_get("cancel_published")
                .map_err(|_| StoreError::Integrity)?;
            transaction
                .commit()
                .await
                .map_err(|_| StoreError::Unavailable)?;
            return Ok(if published == 1 {
                PeerStep::Published(event)
            } else {
                PeerStep::Publish {
                    event,
                    relay_url,
                    retry: true,
                }
            });
        }
        let completed: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM broker_peer_jobs
             WHERE request_event_id = ? AND state = 'completed')",
        )
        .bind(&args.request_event_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|_| StoreError::Unavailable)?;
        if completed {
            return Ok(refuse(
                BrokerErrorCode::ActionFailed,
                "job already completed; its result is terminal",
            ));
        }
        let request = event_from_json(
            &row.try_get::<String, _>("event_json")
                .map_err(|_| StoreError::Integrity)?,
        )?;
        let handoff = JobHandoff::from_tags(&request.tags).map_err(|_| StoreError::Integrity)?;
        let channel = single_tag(&request.tags, "h").map_err(|_| StoreError::Integrity)?;
        let recipient = single_tag(&request.tags, "p").map_err(|_| StoreError::Integrity)?;
        let event = signer
            .sign_job_event(
                &authority.identity,
                43005,
                &args.reason,
                &channel,
                &recipient,
                &[
                    (tag::REQUEST, args.request_event_id.clone()),
                    (tag::REASON, "cancelled_by_sender".to_owned()),
                    (tag::PIPELINE_ID, handoff.pipeline_id.clone()),
                    (tag::CANDIDATE_SHA, handoff.candidate_sha.clone()),
                    (
                        tag::PREVIEW_BUNDLE_DIGEST,
                        handoff.preview_bundle_digest.clone(),
                    ),
                ],
            )
            .map_err(|_| StoreError::InvalidIdentity)?;
        sqlx::query("UPDATE broker_peer_requests SET cancel_event_json = ? WHERE event_id = ?")
            .bind(event_json(&event)?)
            .bind(&args.request_event_id)
            .execute(&mut *transaction)
            .await
            .map_err(|_| StoreError::Unavailable)?;
        sqlx::query(
            "UPDATE broker_peer_jobs SET state = 'cancelled'
             WHERE request_event_id = ? AND state = 'accepted'",
        )
        .bind(&args.request_event_id)
        .execute(&mut *transaction)
        .await
        .map_err(|_| StoreError::Unavailable)?;
        transaction
            .commit()
            .await
            .map_err(|_| StoreError::Unavailable)?;
        Ok(PeerStep::Publish {
            event,
            relay_url,
            retry: false,
        })
    }

    /// Recipient: its relay URL, or a refusal when it may not admit.
    pub(crate) async fn recipient_relay(
        &self,
        credential: &CredentialDigest,
        signer: &HostSigner,
    ) -> Result<Result<String, BrokerError>, StoreError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| StoreError::Unavailable)?;
        let authority = load_authority(&mut transaction, credential).await?;
        if authority.state != AuthorityState::Active
            || !signer.matches_executor(&authority.identity)
            || !owner_matches(&authority, &signer.owner_hex())
        {
            return Ok(Err(BrokerError::unauthorized(
                "active authority and matching host signer are required",
            )));
        }
        Ok(Ok(authority.identity.community_relay_url))
    }

    /// Recipient: request ids already decided by this state.
    pub(crate) async fn decided_requests(&self, ids: &[String]) -> Result<Vec<String>, StoreError> {
        let mut decided = Vec::new();
        for id in ids {
            let known: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM broker_peer_jobs WHERE request_event_id = ?)",
            )
            .bind(id)
            .fetch_one(&self.pool)
            .await
            .map_err(|_| StoreError::Unavailable)?;
            if known {
                decided.push(id.clone());
            }
        }
        Ok(decided)
    }

    /// Recipient: replay a recorded decision, or the oldest unpublished one in
    /// the channel when `request_event_id` is absent.
    pub(crate) async fn recorded_decision(
        &self,
        credential: &CredentialDigest,
        channel_id: &str,
        request_event_id: Option<&str>,
    ) -> Result<Option<AdmitStep>, StoreError> {
        let row = match request_event_id {
            Some(id) => {
                sqlx::query(
                    "SELECT * FROM broker_peer_jobs
                 WHERE request_event_id = ? AND recipient_credential_hash = ? AND channel_id = ?",
                )
                .bind(id)
                .bind(credential.as_bytes())
                .bind(channel_id)
                .fetch_optional(&self.pool)
                .await
            }
            None => {
                sqlx::query(
                    "SELECT * FROM broker_peer_jobs
                 WHERE recipient_credential_hash = ? AND channel_id = ?
                   AND decision_event_json IS NOT NULL AND decision_published = 0
                 ORDER BY rowid LIMIT 1",
                )
                .bind(credential.as_bytes())
                .bind(channel_id)
                .fetch_optional(&self.pool)
                .await
            }
        }
        .map_err(|_| StoreError::Unavailable)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let request_event_id: String = row
            .try_get("request_event_id")
            .map_err(|_| StoreError::Integrity)?;
        let state: String = row.try_get("state").map_err(|_| StoreError::Integrity)?;
        let decision = row
            .try_get::<Option<String>, _>("decision_event_json")
            .map_err(|_| StoreError::Integrity)?
            .map(|json| event_from_json(&json))
            .transpose()?;
        let decision_published: i64 = row
            .try_get("decision_published")
            .map_err(|_| StoreError::Integrity)?;
        let ticket = row
            .try_get::<Option<String>, _>("ticket_json")
            .map_err(|_| StoreError::Integrity)?
            .map(|json| serde_json::from_str::<JobTicket>(&json))
            .transpose()
            .map_err(|_| StoreError::Integrity)?;
        let reason: Option<String> = row.try_get("reason").map_err(|_| StoreError::Integrity)?;
        let admission = match state.as_str() {
            "accepted" | "completed" => JobAdmission {
                decision: JobDecision::Accepted,
                request_event_id: Some(request_event_id),
                decision_event_id: decision.as_ref().map(|event| event.id.to_hex()),
                reason: None,
                ticket,
            },
            "denied" => JobAdmission {
                decision: JobDecision::Denied,
                request_event_id: Some(request_event_id),
                decision_event_id: decision.as_ref().map(|event| event.id.to_hex()),
                reason,
                ticket: None,
            },
            "cancelled" => JobAdmission {
                decision: JobDecision::Cancelled,
                request_event_id: Some(request_event_id),
                decision_event_id: None,
                reason: None,
                ticket: None,
            },
            _ => return Err(StoreError::Integrity),
        };
        let publish = match decision {
            Some(event) if decision_published == 0 => Some((event, true)),
            _ => None,
        };
        Ok(Some(AdmitStep {
            replayed: publish.is_none(),
            admission,
            publish,
        }))
    }

    /// Recipient: decide one verified request event before any work runs.
    ///
    /// Every check reads this state inside one immediate transaction, so a
    /// concurrent duplicate or cancel cannot interleave with the decision.
    pub(crate) async fn decide_job(
        &self,
        credential: &CredentialDigest,
        signer: &HostSigner,
        channel_id: &str,
        request: &nostr::Event,
        now: u64,
    ) -> Result<Result<AdmitStep, BrokerError>, StoreError> {
        let mut connection = self
            .pool
            .acquire()
            .await
            .map_err(|_| StoreError::Unavailable)?;
        let mut transaction = connection
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|_| StoreError::Unavailable)?;
        let authority =
            match Self::active_peer_authority(&mut transaction, credential, signer).await? {
                Ok(authority) => authority,
                Err(error) => return Ok(Err(error)),
            };
        let request_event_id = request.id.to_hex();
        let known: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM broker_peer_jobs WHERE request_event_id = ?)",
        )
        .bind(&request_event_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|_| StoreError::Unavailable)?;
        if known {
            drop(transaction);
            return self
                .recorded_decision(credential, channel_id, Some(&request_event_id))
                .await?
                .map(Ok)
                .ok_or(StoreError::Conflict);
        }
        let me = signer.pubkey_hex();
        let sender = request.pubkey.to_hex();
        let verdict = admission_verdict(&mut transaction, request, channel_id, &me, now).await?;
        let (state, reason, ticket, event) = match verdict {
            Verdict::Accept(handoff) => {
                let ticket = JobTicket {
                    request_event_id: request_event_id.clone(),
                    sender_pubkey: buzz_sdk::broker::PubkeyHex::parse(&sender)
                        .map_err(|_| StoreError::Integrity)?,
                    channel_id: channel_id.to_owned(),
                    handoff,
                };
                let mut tags = vec![(tag::REQUEST, request_event_id.clone())];
                tags.extend(ticket.handoff.tag_pairs());
                let event = signer
                    .sign_job_event(
                        &authority.identity,
                        43002,
                        &format!("accepted {}", ticket.handoff.capability),
                        channel_id,
                        &sender,
                        &tags,
                    )
                    .map_err(|_| StoreError::InvalidIdentity)?;
                ("accepted", None, Some(ticket), Some(event))
            }
            Verdict::Cancelled => ("cancelled", None, None, None),
            Verdict::Deny(reason) => {
                let event = signer
                    .sign_job_event(
                        &authority.identity,
                        43006,
                        &format!("denied: {reason}"),
                        channel_id,
                        &sender,
                        &[
                            (tag::REQUEST, request_event_id.clone()),
                            (tag::REASON, reason.to_owned()),
                        ],
                    )
                    .map_err(|_| StoreError::InvalidIdentity)?;
                eprintln!(
                    "peer job request {request_event_id} from {sender} denied before any work: {reason}"
                );
                ("denied", Some(reason.to_owned()), None, Some(event))
            }
        };
        let idempotency_key = single_tag(&request.tags, tag::IDEMPOTENCY_KEY).ok();
        sqlx::query(
            "INSERT INTO broker_peer_jobs
             (request_event_id, recipient_credential_hash, channel_id, sender_pubkey,
              idempotency_key, state, reason, ticket_json, decision_event_json)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&request_event_id)
        .bind(credential.as_bytes())
        .bind(channel_id)
        .bind(&sender)
        .bind(&idempotency_key)
        .bind(state)
        .bind(&reason)
        .bind(
            ticket
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(|_| StoreError::Integrity)?,
        )
        .bind(event.as_ref().map(event_json).transpose()?)
        .execute(&mut *transaction)
        .await
        .map_err(|_| StoreError::Conflict)?;
        transaction
            .commit()
            .await
            .map_err(|_| StoreError::Unavailable)?;
        let admission = JobAdmission {
            decision: match state {
                "accepted" => JobDecision::Accepted,
                "denied" => JobDecision::Denied,
                _ => JobDecision::Cancelled,
            },
            request_event_id: Some(request_event_id),
            decision_event_id: event.as_ref().map(|event| event.id.to_hex()),
            reason,
            ticket,
        };
        Ok(Ok(AdmitStep {
            admission,
            publish: event.map(|event| (event, false)),
            replayed: false,
        }))
    }

    /// Recipient: sign and journal the one result of an accepted job.
    pub(crate) async fn prepare_job_result(
        &self,
        credential: &CredentialDigest,
        signer: &HostSigner,
        args: &JobResultArgs,
    ) -> Result<PeerStep, StoreError> {
        let result_digest: [u8; 32] =
            Sha256::digest(serde_json::to_vec(args).map_err(|_| StoreError::Integrity)?).into();
        let mut connection = self
            .pool
            .acquire()
            .await
            .map_err(|_| StoreError::Unavailable)?;
        let mut transaction = connection
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|_| StoreError::Unavailable)?;
        let authority =
            match Self::active_peer_authority(&mut transaction, credential, signer).await? {
                Ok(authority) => authority,
                Err(error) => return Ok(PeerStep::Refuse(error)),
            };
        let relay_url = authority.identity.community_relay_url.clone();
        let Some(row) = sqlx::query(
            "SELECT state, ticket_json, decision_event_json, result_event_json, result_digest,
                    result_published
             FROM broker_peer_jobs
             WHERE request_event_id = ? AND recipient_credential_hash = ?",
        )
        .bind(&args.request_event_id)
        .bind(credential.as_bytes())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|_| StoreError::Unavailable)?
        else {
            return Ok(refuse(
                BrokerErrorCode::InvalidRequest,
                "request was not admitted by this generation",
            ));
        };
        let state: String = row.try_get("state").map_err(|_| StoreError::Integrity)?;
        match state.as_str() {
            "completed" => {
                let digest: Option<Vec<u8>> = row
                    .try_get("result_digest")
                    .map_err(|_| StoreError::Integrity)?;
                if digest.as_deref() != Some(result_digest.as_slice()) {
                    return Ok(refuse(
                        BrokerErrorCode::RequestIdConflict,
                        "job already has a different result",
                    ));
                }
                let event = event_from_json(
                    &row.try_get::<String, _>("result_event_json")
                        .map_err(|_| StoreError::Integrity)?,
                )?;
                let published: i64 = row
                    .try_get("result_published")
                    .map_err(|_| StoreError::Integrity)?;
                transaction
                    .commit()
                    .await
                    .map_err(|_| StoreError::Unavailable)?;
                return Ok(if published == 1 {
                    PeerStep::Published(event)
                } else {
                    PeerStep::Publish {
                        event,
                        relay_url,
                        retry: true,
                    }
                });
            }
            "cancelled" => {
                return Ok(refuse(
                    BrokerErrorCode::ActionFailed,
                    "job was cancelled; no result is published",
                ))
            }
            "accepted" => {}
            _ => {
                return Ok(refuse(
                    BrokerErrorCode::InvalidRequest,
                    "request was denied; no result is published",
                ))
            }
        }
        let cancelled: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM broker_peer_requests
             WHERE event_id = ? AND cancel_event_json IS NOT NULL)",
        )
        .bind(&args.request_event_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|_| StoreError::Unavailable)?;
        if cancelled {
            sqlx::query(
                "UPDATE broker_peer_jobs SET state = 'cancelled' WHERE request_event_id = ?",
            )
            .bind(&args.request_event_id)
            .execute(&mut *transaction)
            .await
            .map_err(|_| StoreError::Unavailable)?;
            transaction
                .commit()
                .await
                .map_err(|_| StoreError::Unavailable)?;
            return Ok(refuse(
                BrokerErrorCode::ActionFailed,
                "job was cancelled; no result is published",
            ));
        }
        let ticket: JobTicket = serde_json::from_str(
            &row.try_get::<String, _>("ticket_json")
                .map_err(|_| StoreError::Integrity)?,
        )
        .map_err(|_| StoreError::Integrity)?;
        let accepted = event_from_json(
            &row.try_get::<String, _>("decision_event_json")
                .map_err(|_| StoreError::Integrity)?,
        )?;
        let mut tags = vec![
            (tag::REQUEST, ticket.request_event_id.clone()),
            (tag::ACCEPTED, accepted.id.to_hex()),
            (tag::VERDICT, args.verdict.as_str().to_owned()),
        ];
        if args.verdict == JobVerdict::Error {
            tags.push((tag::REASON, "recipient_error".to_owned()));
        }
        tags.extend(ticket.handoff.tag_pairs());
        if let Some(evidence) = &args.evidence_sha256 {
            tags.push((tag::EVIDENCE_SHA256, evidence.clone()));
        }
        let kind = if args.verdict == JobVerdict::Error {
            43006
        } else {
            43004
        };
        let event = signer
            .sign_job_event(
                &authority.identity,
                kind,
                &args.summary,
                &ticket.channel_id,
                ticket.sender_pubkey.as_str(),
                &tags,
            )
            .map_err(|_| StoreError::InvalidIdentity)?;
        sqlx::query(
            "UPDATE broker_peer_jobs
             SET state = 'completed', result_event_json = ?, result_digest = ?
             WHERE request_event_id = ? AND state = 'accepted'",
        )
        .bind(event_json(&event)?)
        .bind(result_digest.as_slice())
        .bind(&args.request_event_id)
        .execute(&mut *transaction)
        .await
        .map_err(|_| StoreError::Unavailable)?;
        transaction
            .commit()
            .await
            .map_err(|_| StoreError::Unavailable)?;
        Ok(PeerStep::Publish {
            event,
            relay_url,
            retry: false,
        })
    }
}

/// Which published flag a completed publication sets.
#[derive(Clone, Copy)]
pub(crate) enum PeerColumn {
    Request,
    Cancel,
    Decision,
    Result,
}

enum Verdict {
    Accept(JobHandoff),
    Cancelled,
    Deny(&'static str),
}

fn candidate_identity(handoff: &JobHandoff) -> String {
    format!(
        "{}|{}",
        handoff.candidate_sha, handoff.preview_bundle_digest
    )
}

/// The ordered admission matrix. The first failing check names the reason.
async fn admission_verdict(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    request: &nostr::Event,
    channel_id: &str,
    me: &str,
    now: u64,
) -> Result<Verdict, StoreError> {
    let addressed = single_tag(&request.tags, "h").is_ok_and(|h| h == channel_id)
        && single_tag(&request.tags, "p").is_ok_and(|p| p == me);
    if request.verify().is_err()
        || u32::from(request.kind.as_u16()) != buzz_core::kind::KIND_JOB_REQUEST
        || !addressed
    {
        return Ok(Verdict::Deny("malformed_request"));
    }
    let identity_present = single_tag(&request.tags, tag::CANDIDATE_SHA).is_ok()
        && single_tag(&request.tags, tag::PREVIEW_BUNDLE_DIGEST).is_ok();
    let handoff = match JobHandoff::from_tags(&request.tags) {
        Ok(handoff) => handoff,
        Err(error) => {
            let text = error.to_string();
            let identity_error = !identity_present
                || text.contains("candidate")
                || text.contains("previewBundleDigest")
                || text.contains("preview_bundle_digest");
            return Ok(Verdict::Deny(if identity_error {
                "invalid_candidate_identity"
            } else {
                "invalid_handoff"
            }));
        }
    };
    let sender = request.pubkey.to_hex();
    let grants = sqlx::query(
        "SELECT grant_id, sender_credential_hash, channel_id, capability, state
         FROM broker_peer_grants WHERE sender_pubkey = ? AND recipient_pubkey = ?",
    )
    .bind(&sender)
    .bind(me)
    .fetch_all(&mut **transaction)
    .await
    .map_err(|_| StoreError::Unavailable)?;
    let field = |row: &sqlx::sqlite::SqliteRow, name: &str| -> Result<String, StoreError> {
        row.try_get(name).map_err(|_| StoreError::Integrity)
    };
    let mut active = Vec::new();
    let mut revoked_exact = false;
    for row in &grants {
        let exact = field(row, "channel_id")? == channel_id
            && field(row, "capability")? == handoff.capability;
        match field(row, "state")?.as_str() {
            "active" => active.push(row),
            _ if exact => revoked_exact = true,
            _ => {}
        }
    }
    if active.is_empty() {
        return Ok(Verdict::Deny(if revoked_exact {
            "grant_revoked"
        } else {
            "ungranted_peer"
        }));
    }
    let mut in_channel = Vec::new();
    for row in active {
        if field(row, "channel_id")? == channel_id {
            in_channel.push(row);
        }
    }
    if in_channel.is_empty() {
        return Ok(Verdict::Deny("channel_not_granted"));
    }
    let mut granted = None;
    for row in in_channel {
        if field(row, "capability")? == handoff.capability {
            granted = Some(row);
        }
    }
    let Some(grant) = granted else {
        return Ok(Verdict::Deny("capability_not_granted"));
    };
    let grant_id = field(grant, "grant_id")?;
    let sender_credential: Vec<u8> = grant
        .try_get("sender_credential_hash")
        .map_err(|_| StoreError::Integrity)?;
    let sender_state =
        sqlx::query("SELECT state, quiescing FROM broker_authorities WHERE credential_hash = ?")
            .bind(&sender_credential)
            .fetch_one(&mut **transaction)
            .await
            .map_err(|_| StoreError::Integrity)?;
    let state: String = sender_state
        .try_get("state")
        .map_err(|_| StoreError::Integrity)?;
    let quiescing: i64 = sender_state
        .try_get("quiescing")
        .map_err(|_| StoreError::Integrity)?;
    if parse_state(&state, quiescing)? != AuthorityState::Active {
        return Ok(Verdict::Deny("sender_generation_fenced"));
    }
    let journal = sqlx::query(
        "SELECT request_sequence, pipeline_id, stage_id, candidate_identity, cancel_event_json
         FROM broker_peer_requests
         WHERE event_id = ? AND sender_credential_hash = ? AND grant_id = ?",
    )
    .bind(request.id.to_hex())
    .bind(&sender_credential)
    .bind(&grant_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|_| StoreError::Unavailable)?;
    let Some(journal) = journal else {
        return Ok(Verdict::Deny("request_not_issued_under_grant"));
    };
    if journal
        .try_get::<Option<String>, _>("cancel_event_json")
        .map_err(|_| StoreError::Integrity)?
        .is_some()
    {
        return Ok(Verdict::Cancelled);
    }
    if journal
        .try_get::<String, _>("candidate_identity")
        .map_err(|_| StoreError::Integrity)?
        != candidate_identity(&handoff)
    {
        return Ok(Verdict::Deny("invalid_candidate_identity"));
    }
    let sequence: i64 = journal
        .try_get("request_sequence")
        .map_err(|_| StoreError::Integrity)?;
    let superseded: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM broker_peer_requests
         WHERE sender_credential_hash = ? AND pipeline_id = ? AND stage_id = ?
           AND request_sequence > ? AND candidate_identity != ?)",
    )
    .bind(&sender_credential)
    .bind(&handoff.pipeline_id)
    .bind(&handoff.stage_id)
    .bind(sequence)
    .bind(candidate_identity(&handoff))
    .fetch_one(&mut **transaction)
    .await
    .map_err(|_| StoreError::Unavailable)?;
    if superseded {
        return Ok(Verdict::Deny("stale_candidate_identity"));
    }
    if handoff.deadline <= now {
        return Ok(Verdict::Deny("deadline_expired"));
    }
    if now.saturating_add(u64::from(handoff.budget_seconds)) > handoff.deadline {
        return Ok(Verdict::Deny("budget_exceeds_deadline"));
    }
    let duplicate: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM broker_peer_jobs
         WHERE sender_pubkey = ? AND idempotency_key = ?
           AND state IN ('accepted', 'completed', 'cancelled'))",
    )
    .bind(&sender)
    .bind(&handoff.idempotency_key)
    .fetch_one(&mut **transaction)
    .await
    .map_err(|_| StoreError::Unavailable)?;
    if duplicate {
        return Ok(Verdict::Deny("duplicate_idempotency_key"));
    }
    Ok(Verdict::Accept(handoff))
}
