//! Sender side of `job.outcome`: find the request this generation journaled.
//!
//! Read-only. Reading history is allowed for any state of the generation
//! (active, quiescing or fenced), but only for the generation that issued the
//! request: the journal row is keyed by the caller's own credential, so
//! another sender (or another generation) finds nothing, before any relay
//! query is made.

use buzz_sdk::broker::actions::job::{single_tag, JobHandoff, JobOutcomeArgs};

use super::peer::event_from_json;
use super::*;

/// What `job.outcome` reads the relay for.
pub(crate) struct OutcomeTarget {
    /// Relay the generation publishes to.
    pub(crate) relay_url: String,
    /// The journaled, signed request (43001).
    pub(crate) request: nostr::Event,
    /// The recipient the grant names; only it may accept or finish the job.
    pub(crate) recipient: String,
    /// The request's signed handoff identity.
    pub(crate) handoff: JobHandoff,
}

impl AuthorityStore {
    /// Sender: resolve one request this credential journaled, or refuse.
    pub(crate) async fn job_outcome_target(
        &self,
        credential: &CredentialDigest,
        signer: &HostSigner,
        args: &JobOutcomeArgs,
    ) -> Result<Result<OutcomeTarget, BrokerError>, StoreError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| StoreError::Unavailable)?;
        let authority = load_authority(&mut transaction, credential).await?;
        if !owner_matches(&authority, &signer.owner_hex())
            || !signer.matches_executor(&authority.identity)
        {
            return Ok(Err(BrokerError::unauthorized(
                "host signer does not match this authority generation",
            )));
        }
        let row = sqlx::query(
            "SELECT r.event_json, g.recipient_pubkey, g.channel_id
             FROM broker_peer_requests r
             JOIN broker_peer_grants g ON g.grant_id = r.grant_id
             WHERE r.event_id = ? AND r.sender_credential_hash = ?",
        )
        .bind(&args.request_event_id)
        .bind(credential.as_bytes())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|_| StoreError::Unavailable)?;
        drop(transaction);
        let not_ours = || {
            Ok(Err(BrokerError::unauthorized(
                "request was not issued by this generation in this channel",
            )))
        };
        let Some(row) = row else {
            return not_ours();
        };
        let grant_channel: String = row
            .try_get("channel_id")
            .map_err(|_| StoreError::Integrity)?;
        let recipient: String = row
            .try_get("recipient_pubkey")
            .map_err(|_| StoreError::Integrity)?;
        let request = event_from_json(
            &row.try_get::<String, _>("event_json")
                .map_err(|_| StoreError::Integrity)?,
        )?;
        if grant_channel != args.channel_id
            || single_tag(&request.tags, "h").ok().as_deref() != Some(args.channel_id.as_str())
        {
            return not_ours();
        }
        if request.id.to_hex() != args.request_event_id
            || request.pubkey.to_hex() != signer.pubkey_hex()
            || single_tag(&request.tags, "p").ok().as_deref() != Some(recipient.as_str())
        {
            return Err(StoreError::Integrity);
        }
        let handoff = JobHandoff::from_tags(&request.tags).map_err(|_| StoreError::Integrity)?;
        Ok(Ok(OutcomeTarget {
            relay_url: authority.identity.community_relay_url,
            request,
            recipient,
            handoff,
        }))
    }
}
