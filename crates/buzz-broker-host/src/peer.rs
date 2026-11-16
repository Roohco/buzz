//! Broker actions for one scoped peer handoff: `job.request`, `job.cancel`
//! (sender) and `job.admit`, `job.result` (recipient).
//!
//! Admission happens here, in the recipient's broker host, before a ticket is
//! returned: an unadmitted request never reaches the recipient runtime. The
//! runtime's owner-only gate for ordinary messages is untouched; job requests
//! are not part of `channel.read` at all.
//!
//! Every action holds the caller's publication lock from journaling through
//! publication, so `fence` cannot slip between a stored event and its
//! publication.

use buzz_sdk::broker::actions::job::single_tag;
use buzz_sdk::broker::{
    ActionArgs, ActionOutcome, BrokerError, BrokerErrorCode, BrokerResponse, BrokerResult,
    EventPublished, JobAdmission, JobAdmitArgs, JobDecision, ValidatedRequest,
};

use crate::credential::CredentialDigest;
use crate::publisher::{JobPageCursor, RelayPublisher};
use crate::signing::HostSigner;
use crate::store::peer::PeerStep;
use crate::store::peer_admit::AdmitStep;
use crate::store::{AuthorityStore, StoreError};

/// Older request pages one `job.admit` call may scan for an undecided one.
const MAX_ADMIT_PAGES: usize = 64;

/// Execute a job action, or `None` when `request` is not one.
pub(crate) async fn handle(
    store: &AuthorityStore,
    signer: &HostSigner,
    publisher: &RelayPublisher,
    credential: &CredentialDigest,
    request: &ValidatedRequest,
) -> Option<BrokerResponse> {
    if !matches!(
        request.args(),
        ActionArgs::JobRequest(_)
            | ActionArgs::JobCancel(_)
            | ActionArgs::JobResult(_)
            | ActionArgs::JobAdmit(_)
    ) {
        return None;
    }
    Some(respond(
        request.request_id(),
        run(store, signer, publisher, credential, request).await,
    ))
}

async fn run(
    store: &AuthorityStore,
    signer: &HostSigner,
    publisher: &RelayPublisher,
    credential: &CredentialDigest,
    request: &ValidatedRequest,
) -> Result<(BrokerResult, bool), StoreError> {
    // Only an issued credential may create or take a publication lock.
    if !store.credential_known(credential).await? {
        return Err(StoreError::Unauthenticated);
    }
    let _guard = store.publication_lock_for_credential(credential).await?;
    match request.args() {
        ActionArgs::JobRequest(args) => {
            let step = store.prepare_job_request(credential, signer, args).await?;
            finish(store, signer, publisher, step, Outcome::Request).await
        }
        ActionArgs::JobCancel(args) => {
            let step = store.prepare_job_cancel(credential, signer, args).await?;
            finish(store, signer, publisher, step, Outcome::Cancel).await
        }
        ActionArgs::JobResult(args) => {
            let step = store
                .prepare_job_result(credential, signer, args, unix_now())
                .await?;
            finish(store, signer, publisher, step, Outcome::Result).await
        }
        ActionArgs::JobAdmit(args) => admit(store, signer, publisher, credential, args).await,
        _ => Err(StoreError::Integrity),
    }
}

fn respond(request_id: &str, result: Result<(BrokerResult, bool), StoreError>) -> BrokerResponse {
    match result {
        Ok((result, replayed)) => {
            let response = BrokerResponse::new(request_id, result);
            if replayed {
                response.replayed()
            } else {
                response
            }
        }
        Err(StoreError::Unauthenticated) => BrokerResponse::new(
            request_id,
            BrokerResult::failed(BrokerError::new(
                BrokerErrorCode::Unauthenticated,
                "broker credential is not authenticated",
            )),
        ),
        Err(_) => BrokerResponse::new(
            request_id,
            BrokerResult::failed(BrokerError::new(
                BrokerErrorCode::Internal,
                "broker authority state is unavailable",
            )),
        ),
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(u64::MAX, |elapsed| elapsed.as_secs())
}

#[derive(Clone, Copy)]
enum Outcome {
    Request,
    Cancel,
    Result,
}

fn outcome_for(outcome: Outcome, event: &nostr::Event) -> ActionOutcome {
    let published = EventPublished {
        event_id: event.id.to_hex(),
        kind: u32::from(event.kind.as_u16()),
        created_at: event.created_at.as_secs(),
    };
    match outcome {
        Outcome::Request => ActionOutcome::JobRequest(published),
        Outcome::Cancel => ActionOutcome::JobCancel(published),
        Outcome::Result => ActionOutcome::JobResult(published),
    }
}

fn indeterminate() -> BrokerResult {
    BrokerResult::indeterminate(BrokerError::new(
        BrokerErrorCode::OutcomeUnknown,
        "relay publication outcome is not yet reconciled; repeat the same action",
    ))
}

async fn publish(
    publisher: &RelayPublisher,
    signer: &HostSigner,
    relay_url: &str,
    event: &nostr::Event,
    retry: bool,
) -> bool {
    if retry {
        publisher.publish_pending(signer, relay_url, event).await
    } else {
        publisher.publish(signer, relay_url, event).await
    }
    .is_ok()
}

async fn finish(
    store: &AuthorityStore,
    signer: &HostSigner,
    publisher: &RelayPublisher,
    step: PeerStep,
    outcome: Outcome,
) -> Result<(BrokerResult, bool), StoreError> {
    match step {
        PeerStep::Refuse(error) => Ok((BrokerResult::failed(error), false)),
        PeerStep::Published(event) => {
            Ok((BrokerResult::succeeded(outcome_for(outcome, &event)), true))
        }
        PeerStep::Publish {
            event,
            relay_url,
            retry,
            key,
        } => {
            if !publish(publisher, signer, &relay_url, &event, retry).await {
                return Ok((indeterminate(), false));
            }
            store.mark_peer_published(&key).await?;
            Ok((BrokerResult::succeeded(outcome_for(outcome, &event)), retry))
        }
    }
}

fn admitted(admission: JobAdmission, replayed: bool) -> Result<(BrokerResult, bool), StoreError> {
    Ok((
        BrokerResult::succeeded(ActionOutcome::JobAdmit(Box::new(admission))),
        replayed,
    ))
}

fn failed(code: BrokerErrorCode, message: &str) -> Result<(BrokerResult, bool), StoreError> {
    Ok((BrokerResult::failed(BrokerError::new(code, message)), false))
}

async fn admit(
    store: &AuthorityStore,
    signer: &HostSigner,
    publisher: &RelayPublisher,
    credential: &CredentialDigest,
    args: &JobAdmitArgs,
) -> Result<(BrokerResult, bool), StoreError> {
    let relay_url = match store.recipient_relay(credential, signer).await? {
        Ok(url) => url,
        Err(error) => return Ok((BrokerResult::failed(error), false)),
    };
    let me = signer.pubkey_hex();
    let request_id = args.request_event_id.as_deref();
    if let Some(step) = store
        .recorded_decision(&me, &args.channel_id, request_id)
        .await?
    {
        return complete_admission(store, signer, publisher, &relay_url, step).await;
    }
    let mut cursor: Option<JobPageCursor> = None;
    for _ in 0..MAX_ADMIT_PAGES {
        let (events, next) = match publisher
            .query_job_requests(
                signer,
                &relay_url,
                &args.channel_id,
                request_id,
                cursor.as_ref(),
            )
            .await
        {
            Ok(page) => page,
            Err(_) => {
                return failed(
                    BrokerErrorCode::ActionFailed,
                    "relay job request query failed",
                )
            }
        };
        if request_id.is_some() {
            let Some(event) = events.first() else {
                return failed(
                    BrokerErrorCode::InvalidRequest,
                    "request is not a job request in this channel",
                );
            };
            if store.sender_over_cap(&me, &event.pubkey.to_hex()).await? {
                return failed(
                    BrokerErrorCode::ActionFailed,
                    "this sender has reached its decision limit for this recipient",
                );
            }
            return decide_one(
                store, signer, publisher, credential, args, &relay_url, event,
            )
            .await
            .map(|decided| {
                decided.unwrap_or_else(|| {
                    (
                        BrokerResult::failed(BrokerError::new(
                            BrokerErrorCode::ActionFailed,
                            "request could not be decided and was recorded as denied",
                        )),
                        false,
                    )
                })
            });
        }
        let ids: Vec<String> = events.iter().map(|event| event.id.to_hex()).collect();
        let decided = store.decided_requests(&me, &ids).await?;
        for event in &events {
            let id = event.id.to_hex();
            // Only requests addressed solely to this recipient are candidates;
            // anything else is skipped without writing or signing.
            if decided.contains(&id)
                || single_tag(&event.tags, "p").ok().as_deref() != Some(me.as_str())
                || store.sender_over_cap(&me, &event.pubkey.to_hex()).await?
            {
                continue;
            }
            if let Some(done) = decide_one(
                store, signer, publisher, credential, args, &relay_url, event,
            )
            .await?
            {
                return Ok(done);
            }
        }
        match next {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    admitted(
        JobAdmission {
            decision: JobDecision::None,
            request_event_id: None,
            decision_event_id: None,
            reason: None,
            ticket: None,
        },
        false,
    )
}

/// Decide one event. `None` means it was undecidable and is now recorded as
/// denied, so the caller moves on instead of failing the whole call.
async fn decide_one(
    store: &AuthorityStore,
    signer: &HostSigner,
    publisher: &RelayPublisher,
    credential: &CredentialDigest,
    args: &JobAdmitArgs,
    relay_url: &str,
    event: &nostr::Event,
) -> Result<Option<(BrokerResult, bool)>, StoreError> {
    match store
        .decide_job(credential, signer, &args.channel_id, event, unix_now())
        .await
    {
        Ok(Ok(step)) => complete_admission(store, signer, publisher, relay_url, step)
            .await
            .map(Some),
        Ok(Err(refusal)) => Ok(Some((BrokerResult::failed(refusal), false))),
        Err(error @ (StoreError::Unavailable | StoreError::Unauthenticated)) => Err(error),
        Err(error) => {
            eprintln!(
                "peer job request {} could not be decided ({error}); recorded as denied",
                event.id.to_hex()
            );
            store
                .record_undecidable(credential, &signer.pubkey_hex(), &args.channel_id, event)
                .await?;
            Ok(None)
        }
    }
}

async fn complete_admission(
    store: &AuthorityStore,
    signer: &HostSigner,
    publisher: &RelayPublisher,
    relay_url: &str,
    step: AdmitStep,
) -> Result<(BrokerResult, bool), StoreError> {
    if let Some((event, retry, key)) = &step.publish {
        if !publish(publisher, signer, relay_url, event, *retry).await {
            return Ok((indeterminate(), false));
        }
        store.mark_peer_published(key).await?;
    }
    admitted(step.admission, step.replayed)
}
