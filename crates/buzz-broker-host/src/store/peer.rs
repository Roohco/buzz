//! Scoped peer handoff state: operator grants, the sender request journal and
//! recipient decisions.
//!
//! All three live in the same broker state as the authorities they bind, so a
//! recipient host can check the sender's exact generation and that a request
//! event was really issued under the grant. A recipient whose state does not
//! hold the sender's authority finds no grant and denies (fail closed).
//!
//! The tables are additive and created at startup without changing the schema
//! version, so the previous broker release still opens this state (rollback);
//! it simply ignores them. No broker release deletes authority rows, so the
//! foreign keys below stay valid across that rollback.
//!
//! Every signed handoff event is stored before publication and marked
//! published afterwards. An unpublished one is a pending effect: it blocks
//! `fence` until `drain` (or repeating the same action) reconciles it.

use buzz_sdk::broker::actions::job::{
    single_tag, tag, JobCancelArgs, JobHandoff, JobRequestArgs, CAPABILITY_QA_REVIEW_CANDIDATE,
};
use buzz_sdk::broker::BrokerErrorCode;
use serde::Deserialize;

use super::*;

/// Most request journal rows one sender generation may hold.
pub(crate) const MAX_PEER_REQUESTS_PER_CREDENTIAL: i64 = 256;
/// Most decisions one recipient records for one sender identity.
pub(crate) const MAX_PEER_JOBS_PER_SENDER: i64 = 256;

const PEER_TABLES_SQL: [&str; 5] = [
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
        request_event_id TEXT NOT NULL,
        recipient_pubkey TEXT NOT NULL CHECK(length(recipient_pubkey) = 64),
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
        PRIMARY KEY (request_event_id, recipient_pubkey),
        FOREIGN KEY (recipient_credential_hash) REFERENCES broker_authorities(credential_hash)
    ) STRICT",
    "CREATE UNIQUE INDEX IF NOT EXISTS one_run_per_idempotency_key
     ON broker_peer_jobs(sender_pubkey, idempotency_key)
     WHERE state IN ('accepted', 'completed', 'cancelled')",
];

/// SQL fragment: does credential `?1` own any unpublished peer event?
const PEER_PENDING_SQL: &str = "SELECT EXISTS(
        SELECT 1 FROM broker_peer_requests
        WHERE sender_credential_hash = ?1
          AND (published = 0 OR (cancel_event_json IS NOT NULL AND cancel_published = 0))
    ) OR EXISTS(
        SELECT 1 FROM broker_peer_jobs
        WHERE recipient_credential_hash = ?1
          AND ((decision_event_json IS NOT NULL AND decision_published = 0)
            OR (result_event_json IS NOT NULL AND result_published = 0))
    )";

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

/// Which stored event a publication completes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PeerKey {
    Request(String),
    Cancel(String),
    /// Request event id and recipient pubkey.
    Decision(String, String),
    /// Request event id and recipient pubkey.
    Result(String, String),
}

/// One step of a peer effect: publish a stored event, or refuse.
pub(crate) enum PeerStep {
    /// Publish `event` (`retry` when it was stored by an earlier attempt),
    /// then mark `key` published.
    Publish {
        event: nostr::Event,
        relay_url: String,
        retry: bool,
        key: PeerKey,
    },
    /// Already published; replay its identity.
    Published(nostr::Event),
    /// Refused before any signature.
    Refuse(BrokerError),
}

/// A stored, not yet published peer event of one generation.
pub(crate) struct PendingPeerEvent {
    pub(crate) key: PeerKey,
    pub(crate) event: nostr::Event,
}

pub(super) fn refuse(code: BrokerErrorCode, message: &str) -> PeerStep {
    PeerStep::Refuse(BrokerError::new(code, message))
}

pub(super) fn event_from_json(value: &str) -> Result<nostr::Event, StoreError> {
    decode_effect_event(value)
}

pub(super) fn event_json(event: &nostr::Event) -> Result<String, StoreError> {
    serde_json::to_string(event).map_err(|_| StoreError::Integrity)
}

pub(super) fn candidate_identity(handoff: &JobHandoff) -> String {
    format!(
        "{}|{}",
        handoff.candidate_sha, handoff.preview_bundle_digest
    )
}

/// Whether credential `hash` owns an unpublished peer event (fence gate).
pub(super) async fn peer_effects_pending(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    hash: &[u8],
) -> Result<bool, StoreError> {
    sqlx::query_scalar(PEER_PENDING_SQL)
        .bind(hash)
        .fetch_one(&mut **transaction)
        .await
        .map_err(|_| StoreError::Unavailable)
}

impl AuthorityStore {
    /// Create the additive peer tables; the schema version is unchanged.
    pub(super) async fn ensure_peer_tables(&self) -> Result<(), StoreError> {
        let mut connection = self
            .pool
            .acquire()
            .await
            .map_err(|_| StoreError::Unavailable)?;
        let mut transaction = connection
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|_| StoreError::Unavailable)?;
        for statement in PEER_TABLES_SQL {
            sqlx::query(statement)
                .execute(&mut *transaction)
                .await
                .map_err(|_| StoreError::Unavailable)?;
        }
        transaction
            .commit()
            .await
            .map_err(|_| StoreError::Unavailable)
    }

    /// Whether `credential` names an issued authority (before taking a lock).
    pub(crate) async fn credential_known(
        &self,
        credential: &CredentialDigest,
    ) -> Result<bool, StoreError> {
        sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM broker_authorities WHERE credential_hash = ?)",
        )
        .bind(credential.as_bytes())
        .fetch_one(&self.pool)
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
            let text = |name: &str| existing.try_get::<String, _>(name).ok();
            let same = existing
                .try_get::<Vec<u8>, _>("sender_credential_hash")
                .is_ok_and(|hash| hash == credential_hash)
                && text("recipient_pubkey").as_deref() == Some(recipient.as_str())
                && text("channel_id").as_deref() == Some(channel_id.as_str())
                && text("capability").as_deref() == Some(grant.capability.as_str())
                && text("state").as_deref() == Some("active");
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

    /// Readable operator status: grants, sender requests and recipient
    /// decisions with their publication state. No secrets.
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
            let text = |name: &str| {
                row.try_get::<String, _>(name)
                    .map_err(|_| StoreError::Integrity)
            };
            let quiescing: i64 = row
                .try_get("quiescing")
                .map_err(|_| StoreError::Integrity)?;
            lines.push(serde_json::json!({
                "type": "grant",
                "grantId": text("grant_id")?,
                "senderPubkey": text("sender_pubkey")?,
                "senderGeneration": text("generation")?,
                "senderGenerationState": state_label(parse_state(&text("sender_state")?, quiescing)?),
                "recipientPubkey": text("recipient_pubkey")?,
                "channelId": text("channel_id")?,
                "capability": text("capability")?,
                "state": text("state")?,
            }));
        }
        let requests = sqlx::query(
            "SELECT event_id, grant_id, idempotency_key, published, cancel_event_json,
                    cancel_published
             FROM broker_peer_requests ORDER BY request_sequence",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|_| StoreError::Unavailable)?;
        for row in requests {
            let cancel = row
                .try_get::<Option<String>, _>("cancel_event_json")
                .map_err(|_| StoreError::Integrity)?
                .map(|json| event_from_json(&json).map(|event| event.id.to_hex()))
                .transpose()?;
            let flag = |name: &str| {
                row.try_get::<i64, _>(name)
                    .map(|value| value == 1)
                    .map_err(|_| StoreError::Integrity)
            };
            lines.push(serde_json::json!({
                "type": "request",
                "requestEventId": row.try_get::<String, _>("event_id").map_err(|_| StoreError::Integrity)?,
                "grantId": row.try_get::<String, _>("grant_id").map_err(|_| StoreError::Integrity)?,
                "idempotencyKey": row.try_get::<String, _>("idempotency_key").map_err(|_| StoreError::Integrity)?,
                "published": flag("published")?,
                "cancelEventId": cancel,
                "cancelPublished": flag("cancel_published")?,
            }));
        }
        let jobs = sqlx::query(
            "SELECT request_event_id, recipient_pubkey, channel_id, sender_pubkey, state, reason,
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
            let text = |name: &str| {
                row.try_get::<String, _>(name)
                    .map_err(|_| StoreError::Integrity)
            };
            let flag = |name: &str| {
                row.try_get::<i64, _>(name)
                    .map(|value| value == 1)
                    .map_err(|_| StoreError::Integrity)
            };
            lines.push(serde_json::json!({
                "type": "job",
                "requestEventId": text("request_event_id")?,
                "recipientPubkey": text("recipient_pubkey")?,
                "channelId": text("channel_id")?,
                "senderPubkey": text("sender_pubkey")?,
                "state": text("state")?,
                "reason": row.try_get::<Option<String>, _>("reason").map_err(|_| StoreError::Integrity)?,
                "decisionEventId": id_of("decision_event_json")?,
                "decisionPublished": flag("decision_published")?,
                "resultEventId": id_of("result_event_json")?,
                "resultPublished": flag("result_published")?,
            }));
        }
        Ok(lines)
    }

    /// Unpublished peer events of one quiescing or fenced generation, for
    /// `drain`. An active generation reconciles by repeating its action.
    pub(crate) async fn pending_peer_events(
        &self,
        identity: &AuthorityIdentity,
    ) -> Result<(String, Vec<PendingPeerEvent>), StoreError> {
        let identity = identity
            .validated()
            .map_err(|_| StoreError::InvalidIdentity)?;
        let authority_json = canonical_identity(&identity)?;
        let row = sqlx::query(
            "SELECT credential_hash, state, quiescing FROM broker_authorities
             WHERE authority_json = ?",
        )
        .bind(authority_json)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| StoreError::Unavailable)?
        .ok_or(StoreError::AuthorityNotFound)?;
        let hash: Vec<u8> = row
            .try_get("credential_hash")
            .map_err(|_| StoreError::Integrity)?;
        let state: String = row.try_get("state").map_err(|_| StoreError::Integrity)?;
        let quiescing: i64 = row
            .try_get("quiescing")
            .map_err(|_| StoreError::Integrity)?;
        if parse_state(&state, quiescing)? == AuthorityState::Active {
            return Err(StoreError::NotQuiescing);
        }
        let mut pending = Vec::new();
        let requests = sqlx::query(
            "SELECT event_id, event_json, published, cancel_event_json, cancel_published
             FROM broker_peer_requests WHERE sender_credential_hash = ?
             ORDER BY request_sequence",
        )
        .bind(&hash)
        .fetch_all(&self.pool)
        .await
        .map_err(|_| StoreError::Unavailable)?;
        for row in requests {
            let id: String = row.try_get("event_id").map_err(|_| StoreError::Integrity)?;
            if row
                .try_get::<i64, _>("published")
                .map_err(|_| StoreError::Integrity)?
                == 0
            {
                pending.push(PendingPeerEvent {
                    key: PeerKey::Request(id.clone()),
                    event: event_from_json(
                        &row.try_get::<String, _>("event_json")
                            .map_err(|_| StoreError::Integrity)?,
                    )?,
                });
            }
            if let Some(cancel) = row
                .try_get::<Option<String>, _>("cancel_event_json")
                .map_err(|_| StoreError::Integrity)?
            {
                if row
                    .try_get::<i64, _>("cancel_published")
                    .map_err(|_| StoreError::Integrity)?
                    == 0
                {
                    pending.push(PendingPeerEvent {
                        key: PeerKey::Cancel(id),
                        event: event_from_json(&cancel)?,
                    });
                }
            }
        }
        let jobs = sqlx::query(
            "SELECT request_event_id, recipient_pubkey, decision_event_json, decision_published,
                    result_event_json, result_published
             FROM broker_peer_jobs WHERE recipient_credential_hash = ? ORDER BY rowid",
        )
        .bind(&hash)
        .fetch_all(&self.pool)
        .await
        .map_err(|_| StoreError::Unavailable)?;
        for row in jobs {
            let request: String = row
                .try_get("request_event_id")
                .map_err(|_| StoreError::Integrity)?;
            let recipient: String = row
                .try_get("recipient_pubkey")
                .map_err(|_| StoreError::Integrity)?;
            for (json, flag, key) in [
                (
                    "decision_event_json",
                    "decision_published",
                    PeerKey::Decision(request.clone(), recipient.clone()),
                ),
                (
                    "result_event_json",
                    "result_published",
                    PeerKey::Result(request.clone(), recipient.clone()),
                ),
            ] {
                let stored: Option<String> =
                    row.try_get(json).map_err(|_| StoreError::Integrity)?;
                let published: i64 = row.try_get(flag).map_err(|_| StoreError::Integrity)?;
                if let (Some(stored), 0) = (stored, published) {
                    pending.push(PendingPeerEvent {
                        key,
                        event: event_from_json(&stored)?,
                    });
                }
            }
        }
        for item in &pending {
            if item.event.pubkey.to_hex() != identity.executor_agent_pubkey.as_str() {
                return Err(StoreError::Integrity);
            }
        }
        Ok((identity.community_relay_url, pending))
    }

    /// Load the caller's authority and require it may sign peer events.
    /// Results for already-accepted jobs may still be published while the
    /// recipient generation is quiescing (draining), never once fenced.
    pub(super) async fn peer_authority(
        transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        credential: &CredentialDigest,
        signer: &HostSigner,
        allow_quiescing: bool,
    ) -> Result<Result<StoredAuthority, BrokerError>, StoreError> {
        let authority = load_authority(transaction, credential).await?;
        if !owner_matches(&authority, &signer.owner_hex()) {
            return Ok(Err(BrokerError::unauthorized(
                "authority generation belongs to another verified owner",
            )));
        }
        let permitted = authority.state == AuthorityState::Active
            || (allow_quiescing && authority.state == AuthorityState::Quiescing);
        if !permitted {
            return Ok(Err(BrokerError::unauthorized(
                "authority generation is not active",
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
            match Self::peer_authority(&mut transaction, credential, signer, false).await? {
                Ok(authority) => authority,
                Err(error) => return Ok(PeerStep::Refuse(error)),
            };
        let relay_url = authority.identity.community_relay_url.clone();
        let existing = sqlx::query(
            "SELECT event_id, args_digest, event_json, published FROM broker_peer_requests
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
                    key: PeerKey::Request(event.id.to_hex()),
                    event,
                    relay_url,
                    retry: true,
                }
            });
        }
        let journaled: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM broker_peer_requests WHERE sender_credential_hash = ?",
        )
        .bind(credential.as_bytes())
        .fetch_one(&mut *transaction)
        .await
        .map_err(|_| StoreError::Unavailable)?;
        if journaled >= MAX_PEER_REQUESTS_PER_CREDENTIAL {
            return Ok(refuse(
                BrokerErrorCode::ActionFailed,
                "peer request journal is full for this generation",
            ));
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
            key: PeerKey::Request(event.id.to_hex()),
            event,
            relay_url,
            retry: false,
        })
    }

    pub(crate) async fn mark_peer_published(&self, key: &PeerKey) -> Result<(), StoreError> {
        let (sql, request, recipient) = match key {
            PeerKey::Request(id) => (
                "UPDATE broker_peer_requests SET published = 1 WHERE event_id = ?1",
                id,
                None,
            ),
            PeerKey::Cancel(id) => (
                "UPDATE broker_peer_requests SET cancel_published = 1
                 WHERE event_id = ?1 AND cancel_event_json IS NOT NULL",
                id,
                None,
            ),
            PeerKey::Decision(id, recipient) => (
                "UPDATE broker_peer_jobs SET decision_published = 1
                 WHERE request_event_id = ?1 AND recipient_pubkey = ?2
                   AND decision_event_json IS NOT NULL",
                id,
                Some(recipient),
            ),
            PeerKey::Result(id, recipient) => (
                "UPDATE broker_peer_jobs SET result_published = 1
                 WHERE request_event_id = ?1 AND recipient_pubkey = ?2
                   AND result_event_json IS NOT NULL",
                id,
                Some(recipient),
            ),
        };
        let mut query = sqlx::query(sql).bind(request);
        if let Some(recipient) = recipient {
            query = query.bind(recipient);
        }
        let result = query
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
            match Self::peer_authority(&mut transaction, credential, signer, false).await? {
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
        let key = PeerKey::Cancel(args.request_event_id.clone());
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
                    key,
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
            key,
        })
    }
}

fn state_label(state: AuthorityState) -> &'static str {
    match state {
        AuthorityState::Active => "active",
        AuthorityState::Quiescing => "quiescing",
        AuthorityState::Fenced => "fenced",
    }
}
