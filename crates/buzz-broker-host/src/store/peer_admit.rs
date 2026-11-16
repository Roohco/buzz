//! Recipient side of the peer handoff: admission decisions and results.
//!
//! Decisions are keyed by (request event, recipient identity), so a successor
//! recipient generation sees its predecessor's decision and never re-runs it.

use buzz_sdk::broker::actions::job::{
    single_tag, tag, JobAdmission, JobDecision, JobHandoff, JobResultArgs, JobTicket, JobVerdict,
};
use buzz_sdk::broker::BrokerErrorCode;

use super::peer::{
    candidate_identity, event_from_json, event_json, refuse, PeerKey, PeerStep,
    MAX_PEER_JOBS_PER_SENDER,
};
use super::*;

/// Outcome of a recipient decision.
pub(crate) struct AdmitStep {
    pub(crate) admission: JobAdmission,
    /// Decision event still to be published (accepted or denied).
    pub(crate) publish: Option<(nostr::Event, bool, PeerKey)>,
    pub(crate) replayed: bool,
}

enum Verdict {
    Accept(JobHandoff),
    Cancelled,
    Deny(&'static str),
}

impl AuthorityStore {
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

    /// Recipient: which of `ids` this recipient identity already decided.
    pub(crate) async fn decided_requests(
        &self,
        recipient: &str,
        ids: &[String],
    ) -> Result<Vec<String>, StoreError> {
        let mut decided = Vec::new();
        for id in ids {
            let known: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM broker_peer_jobs
                 WHERE request_event_id = ? AND recipient_pubkey = ?)",
            )
            .bind(id)
            .bind(recipient)
            .fetch_one(&self.pool)
            .await
            .map_err(|_| StoreError::Unavailable)?;
            if known {
                decided.push(id.clone());
            }
        }
        Ok(decided)
    }

    /// Recipient: whether `sender` has used up its decision budget here.
    pub(crate) async fn sender_over_cap(
        &self,
        recipient: &str,
        sender: &str,
    ) -> Result<bool, StoreError> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM broker_peer_jobs WHERE recipient_pubkey = ? AND sender_pubkey = ?",
        )
        .bind(recipient)
        .bind(sender)
        .fetch_one(&self.pool)
        .await
        .map_err(|_| StoreError::Unavailable)?;
        Ok(count >= MAX_PEER_JOBS_PER_SENDER)
    }

    /// Recipient: replay a recorded decision, or the oldest unpublished one in
    /// the channel when `request_event_id` is absent.
    pub(crate) async fn recorded_decision(
        &self,
        recipient: &str,
        channel_id: &str,
        request_event_id: Option<&str>,
    ) -> Result<Option<AdmitStep>, StoreError> {
        let row = match request_event_id {
            Some(id) => {
                sqlx::query(
                    "SELECT * FROM broker_peer_jobs
                     WHERE request_event_id = ? AND recipient_pubkey = ? AND channel_id = ?",
                )
                .bind(id)
                .bind(recipient)
                .bind(channel_id)
                .fetch_optional(&self.pool)
                .await
            }
            None => {
                sqlx::query(
                    "SELECT * FROM broker_peer_jobs
                     WHERE recipient_pubkey = ? AND channel_id = ?
                       AND decision_event_json IS NOT NULL AND decision_published = 0
                     ORDER BY rowid LIMIT 1",
                )
                .bind(recipient)
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
        let event = |name: &str| -> Result<Option<nostr::Event>, StoreError> {
            row.try_get::<Option<String>, _>(name)
                .map_err(|_| StoreError::Integrity)?
                .map(|json| event_from_json(&json))
                .transpose()
        };
        let decision = event("decision_event_json")?;
        let result = event("result_event_json")?;
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
        let id_of = |event: &Option<nostr::Event>| event.as_ref().map(|event| event.id.to_hex());
        let mut admission = JobAdmission {
            decision: JobDecision::Cancelled,
            request_event_id: Some(request_event_id.clone()),
            decision_event_id: None,
            reason: None,
            ticket: None,
        };
        match state.as_str() {
            "accepted" => {
                admission.decision = JobDecision::Accepted;
                admission.decision_event_id = id_of(&decision);
                admission.ticket = ticket;
            }
            "completed" => {
                admission.decision = JobDecision::Completed;
                admission.decision_event_id = id_of(&result);
            }
            "denied" => {
                admission.decision = JobDecision::Denied;
                admission.decision_event_id = id_of(&decision);
                admission.reason = reason;
            }
            "cancelled" => {}
            _ => return Err(StoreError::Integrity),
        }
        let publish = match decision {
            Some(event) if decision_published == 0 => Some((
                event,
                true,
                PeerKey::Decision(request_event_id, recipient.to_owned()),
            )),
            _ => None,
        };
        Ok(Some(AdmitStep {
            replayed: publish.is_none(),
            admission,
            publish,
        }))
    }

    /// Recipient: record a request that could not be decided (store error) as
    /// denied without signing anything, so it never blocks the queue again.
    pub(crate) async fn record_undecidable(
        &self,
        credential: &CredentialDigest,
        recipient: &str,
        channel_id: &str,
        request: &nostr::Event,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT OR IGNORE INTO broker_peer_jobs
             (request_event_id, recipient_pubkey, recipient_credential_hash, channel_id,
              sender_pubkey, idempotency_key, state, reason)
             VALUES (?, ?, ?, ?, ?, NULL, 'denied', 'undecidable_request')",
        )
        .bind(request.id.to_hex())
        .bind(recipient)
        .bind(credential.as_bytes())
        .bind(channel_id)
        .bind(request.pubkey.to_hex())
        .execute(&self.pool)
        .await
        .map_err(|_| StoreError::Unavailable)?;
        Ok(())
    }

    /// Recipient: decide one verified request event before any work runs.
    ///
    /// A request not addressed to this recipient in this channel is refused
    /// without writing or signing anything. Every other check reads this state
    /// inside one immediate transaction, so a concurrent duplicate or cancel
    /// cannot interleave with the decision.
    pub(crate) async fn decide_job(
        &self,
        credential: &CredentialDigest,
        signer: &HostSigner,
        channel_id: &str,
        request: &nostr::Event,
        now: u64,
    ) -> Result<Result<AdmitStep, BrokerError>, StoreError> {
        let me = signer.pubkey_hex();
        let addressed = single_tag(&request.tags, "h").is_ok_and(|h| h == channel_id)
            && single_tag(&request.tags, "p").is_ok_and(|p| p == me);
        if !addressed {
            return Ok(Err(BrokerError::new(
                BrokerErrorCode::InvalidRequest,
                "request is not addressed to this agent in this channel",
            )));
        }
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
                Err(error) => return Ok(Err(error)),
            };
        let request_event_id = request.id.to_hex();
        let known: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM broker_peer_jobs
             WHERE request_event_id = ? AND recipient_pubkey = ?)",
        )
        .bind(&request_event_id)
        .bind(&me)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|_| StoreError::Unavailable)?;
        if known {
            drop(transaction);
            return self
                .recorded_decision(&me, channel_id, Some(&request_event_id))
                .await?
                .map(Ok)
                .ok_or(StoreError::Conflict);
        }
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
             (request_event_id, recipient_pubkey, recipient_credential_hash, channel_id,
              sender_pubkey, idempotency_key, state, reason, ticket_json, decision_event_json)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&request_event_id)
        .bind(&me)
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
            request_event_id: Some(request_event_id.clone()),
            decision_event_id: event.as_ref().map(|event| event.id.to_hex()),
            reason,
            ticket,
        };
        Ok(Ok(AdmitStep {
            admission,
            publish: event.map(|event| (event, false, PeerKey::Decision(request_event_id, me))),
            replayed: false,
        }))
    }

    /// Recipient: sign and journal the one result of an accepted job.
    ///
    /// Publication re-checks the grant, the sender generation and the
    /// deadline: if any no longer holds, the job ends with a signed job error
    /// (43006) instead of a result.
    pub(crate) async fn prepare_job_result(
        &self,
        credential: &CredentialDigest,
        signer: &HostSigner,
        args: &JobResultArgs,
        now: u64,
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
            match Self::peer_authority(&mut transaction, credential, signer, true).await? {
                Ok(authority) => authority,
                Err(error) => return Ok(PeerStep::Refuse(error)),
            };
        let relay_url = authority.identity.community_relay_url.clone();
        let me = signer.pubkey_hex();
        let key = PeerKey::Result(args.request_event_id.clone(), me.clone());
        let Some(row) = sqlx::query(
            "SELECT state, ticket_json, decision_event_json, result_event_json, result_digest,
                    result_published
             FROM broker_peer_jobs
             WHERE request_event_id = ? AND recipient_pubkey = ? AND recipient_credential_hash = ?",
        )
        .bind(&args.request_event_id)
        .bind(&me)
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
                        key,
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
        let journal = sqlx::query(
            "SELECT r.cancel_event_json, g.state AS grant_state, a.state AS sender_state,
                    a.quiescing
             FROM broker_peer_requests r
             JOIN broker_peer_grants g ON g.grant_id = r.grant_id
             JOIN broker_authorities a ON a.credential_hash = r.sender_credential_hash
             WHERE r.event_id = ?",
        )
        .bind(&args.request_event_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|_| StoreError::Integrity)?;
        if journal
            .try_get::<Option<String>, _>("cancel_event_json")
            .map_err(|_| StoreError::Integrity)?
            .is_some()
        {
            sqlx::query(
                "UPDATE broker_peer_jobs SET state = 'cancelled'
                 WHERE request_event_id = ? AND recipient_pubkey = ?",
            )
            .bind(&args.request_event_id)
            .bind(&me)
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
        let sender_active = parse_state(
            &journal
                .try_get::<String, _>("sender_state")
                .map_err(|_| StoreError::Integrity)?,
            journal
                .try_get::<i64, _>("quiescing")
                .map_err(|_| StoreError::Integrity)?,
        )? == AuthorityState::Active;
        let withdrawn = if journal
            .try_get::<String, _>("grant_state")
            .map_err(|_| StoreError::Integrity)?
            != "active"
        {
            Some("grant_revoked")
        } else if !sender_active {
            Some("sender_generation_fenced")
        } else if ticket.handoff.deadline <= now {
            Some("deadline_expired")
        } else if args.verdict == JobVerdict::Error {
            Some("recipient_error")
        } else {
            None
        };
        let mut tags = vec![
            (tag::REQUEST, ticket.request_event_id.clone()),
            (tag::ACCEPTED, accepted.id.to_hex()),
        ];
        let (kind, content) = match withdrawn {
            Some(reason) => {
                tags.push((tag::REASON, reason.to_owned()));
                (43006, format!("job error: {reason}"))
            }
            None => (43004, args.summary.clone()),
        };
        if withdrawn.is_none() || args.verdict == JobVerdict::Error {
            tags.push((tag::VERDICT, args.verdict.as_str().to_owned()));
        }
        tags.extend(ticket.handoff.tag_pairs());
        if let (None, Some(evidence)) = (withdrawn, &args.evidence_sha256) {
            tags.push((tag::EVIDENCE_SHA256, evidence.clone()));
        }
        let event = signer
            .sign_job_event(
                &authority.identity,
                kind,
                &content,
                &ticket.channel_id,
                ticket.sender_pubkey.as_str(),
                &tags,
            )
            .map_err(|_| StoreError::InvalidIdentity)?;
        sqlx::query(
            "UPDATE broker_peer_jobs
             SET state = 'completed', result_event_json = ?, result_digest = ?
             WHERE request_event_id = ? AND recipient_pubkey = ? AND state = 'accepted'",
        )
        .bind(event_json(&event)?)
        .bind(result_digest.as_slice())
        .bind(&args.request_event_id)
        .bind(&me)
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

/// The ordered admission matrix. The first failing check names the reason.
async fn admission_verdict(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    request: &nostr::Event,
    channel_id: &str,
    me: &str,
    now: u64,
) -> Result<Verdict, StoreError> {
    if request.verify().is_err()
        || u32::from(request.kind.as_u16()) != buzz_core::kind::KIND_JOB_REQUEST
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
        "SELECT request_sequence, candidate_identity, cancel_event_json
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
    // Before cancel: a cancelled duplicate must not claim the idempotency key
    // a run already holds.
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
    Ok(Verdict::Accept(handoff))
}
