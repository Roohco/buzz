//! Broker actions for one scoped peer handoff: `job.request`, `job.cancel`
//! (sender) and `job.admit`, `job.result` (recipient).
//!
//! Admission happens here, in the recipient's broker host, before a ticket is
//! returned: an unadmitted request never reaches the recipient runtime. The
//! runtime's owner-only gate for ordinary messages is untouched; job requests
//! are not part of `channel.read` at all.

use buzz_sdk::broker::{
    ActionArgs, ActionOutcome, BrokerError, BrokerErrorCode, BrokerResponse, BrokerResult,
    EventPublished, JobAdmission, JobAdmitArgs, JobDecision, ValidatedRequest,
};

use crate::credential::CredentialDigest;
use crate::publisher::RelayPublisher;
use crate::signing::HostSigner;
use crate::store::peer::{AdmitStep, PeerColumn, PeerStep};
use crate::store::{AuthorityStore, StoreError};

/// Execute a job action, or `None` when `request` is not one.
pub(crate) async fn handle(
    store: &AuthorityStore,
    signer: &HostSigner,
    publisher: &RelayPublisher,
    credential: &CredentialDigest,
    request: &ValidatedRequest,
) -> Option<BrokerResponse> {
    let request_id = request.request_id();
    let result = match request.args() {
        ActionArgs::JobRequest(args) => {
            let step = store.prepare_job_request(credential, signer, args).await;
            finish(store, signer, publisher, step, PeerColumn::Request, None).await
        }
        ActionArgs::JobCancel(args) => {
            let step = store.prepare_job_cancel(credential, signer, args).await;
            let key = args.request_event_id.clone();
            finish(
                store,
                signer,
                publisher,
                step,
                PeerColumn::Cancel,
                Some(key),
            )
            .await
        }
        ActionArgs::JobResult(args) => {
            let step = store.prepare_job_result(credential, signer, args).await;
            let key = args.request_event_id.clone();
            finish(
                store,
                signer,
                publisher,
                step,
                PeerColumn::Result,
                Some(key),
            )
            .await
        }
        ActionArgs::JobAdmit(args) => admit(store, signer, publisher, credential, args).await,
        _ => return None,
    };
    Some(match result {
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
    })
}

fn outcome_for(column: PeerColumn, event: &nostr::Event) -> ActionOutcome {
    let published = EventPublished {
        event_id: event.id.to_hex(),
        kind: u32::from(event.kind.as_u16()),
        created_at: event.created_at.as_secs(),
    };
    match column {
        PeerColumn::Request => ActionOutcome::JobRequest(published),
        PeerColumn::Cancel => ActionOutcome::JobCancel(published),
        PeerColumn::Result | PeerColumn::Decision => ActionOutcome::JobResult(published),
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
    step: Result<PeerStep, StoreError>,
    column: PeerColumn,
    key: Option<String>,
) -> Result<(BrokerResult, bool), StoreError> {
    match step? {
        PeerStep::Refuse(error) => Ok((BrokerResult::failed(error), false)),
        PeerStep::Published(event) => {
            Ok((BrokerResult::succeeded(outcome_for(column, &event)), true))
        }
        PeerStep::Publish {
            event,
            relay_url,
            retry,
        } => {
            if !publish(publisher, signer, &relay_url, &event, retry).await {
                return Ok((indeterminate(), false));
            }
            let key = key.unwrap_or_else(|| event.id.to_hex());
            store.mark_peer_published(column, &key).await?;
            Ok((BrokerResult::succeeded(outcome_for(column, &event)), retry))
        }
    }
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
    let request_id = args.request_event_id.as_deref();
    if let Some(step) = store
        .recorded_decision(credential, &args.channel_id, request_id)
        .await?
    {
        return complete_admission(store, signer, publisher, &relay_url, step).await;
    }
    let events = match publisher
        .query_job_requests(signer, &relay_url, &args.channel_id, request_id)
        .await
    {
        Ok(events) => events,
        Err(_) => {
            return Ok((
                BrokerResult::failed(BrokerError::new(
                    BrokerErrorCode::ActionFailed,
                    "relay job request query failed",
                )),
                false,
            ))
        }
    };
    let ids: Vec<String> = events.iter().map(|event| event.id.to_hex()).collect();
    let decided = store.decided_requests(&ids).await?;
    let Some(request) = events
        .iter()
        .find(|event| !decided.contains(&event.id.to_hex()))
    else {
        if request_id.is_some() && decided.is_empty() {
            return Ok((
                BrokerResult::failed(BrokerError::new(
                    BrokerErrorCode::InvalidRequest,
                    "request is not a job request in this channel",
                )),
                false,
            ));
        }
        return Ok((
            BrokerResult::succeeded(ActionOutcome::JobAdmit(Box::new(JobAdmission {
                decision: JobDecision::None,
                request_event_id: None,
                decision_event_id: None,
                reason: None,
                ticket: None,
            }))),
            false,
        ));
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(u64::MAX, |elapsed| elapsed.as_secs());
    match store
        .decide_job(credential, signer, &args.channel_id, request, now)
        .await?
    {
        Ok(step) => complete_admission(store, signer, publisher, &relay_url, step).await,
        Err(error) => Ok((BrokerResult::failed(error), false)),
    }
}

async fn complete_admission(
    store: &AuthorityStore,
    signer: &HostSigner,
    publisher: &RelayPublisher,
    relay_url: &str,
    step: AdmitStep,
) -> Result<(BrokerResult, bool), StoreError> {
    if let Some((event, retry)) = &step.publish {
        if !publish(publisher, signer, relay_url, event, *retry).await {
            return Ok((indeterminate(), false));
        }
        let request_event_id = step
            .admission
            .request_event_id
            .as_deref()
            .ok_or(StoreError::Integrity)?;
        store
            .mark_peer_published(PeerColumn::Decision, request_event_id)
            .await?;
    }
    Ok((
        BrokerResult::succeeded(ActionOutcome::JobAdmit(Box::new(step.admission))),
        step.replayed,
    ))
}
