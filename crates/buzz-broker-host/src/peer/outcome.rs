//! `job.outcome`: the sender reads where its own request stands.
//!
//! Read-only: nothing is signed or stored. The caller must be the generation
//! whose broker journaled the request (checked before any relay query); the
//! answer is built only from relay events that verify, link the request, come
//! from the right author and repeat the request's exact identity. Everything
//! else is ignored and counted.

use buzz_sdk::broker::actions::job::{
    single_tag, tag, JobOutcome, JobOutcomeArgs, JobOutcomeState,
};
use buzz_sdk::broker::{ActionOutcome, BrokerError, BrokerErrorCode, BrokerMessage, BrokerResult};

use crate::credential::CredentialDigest;
use crate::publisher::{JobPageCursor, RelayPublisher};
use crate::signing::HostSigner;
use crate::store::peer_outcome::OutcomeTarget;
use crate::store::{AuthorityStore, StoreError};

/// Older pages one `job.outcome` call may read before giving up.
const MAX_OUTCOME_PAGES: usize = 64;

/// How far before the request a recipient's lagging clock may date its events.
const CLOCK_SKEW_SECONDS: u64 = 15 * 60;

/// Run `job.outcome` for `credential`.
pub(super) async fn read(
    store: &AuthorityStore,
    signer: &HostSigner,
    publisher: &RelayPublisher,
    credential: &CredentialDigest,
    args: &JobOutcomeArgs,
) -> Result<BrokerResult, StoreError> {
    let target = match store.job_outcome_target(credential, signer, args).await? {
        Ok(target) => target,
        Err(error) => return Ok(BrokerResult::failed(error)),
    };
    let sender = target.request.pubkey.to_hex();
    let authors = [target.recipient.as_str(), sender.as_str()];
    let since = target
        .request
        .created_at
        .as_secs()
        .saturating_sub(CLOCK_SKEW_SECONDS);
    let mut events = Vec::new();
    let mut cursor: Option<JobPageCursor> = None;
    let mut complete = false;
    for _ in 0..MAX_OUTCOME_PAGES {
        let (page, next) = match publisher
            .query_job_outcomes(
                signer,
                &target.relay_url,
                &args.channel_id,
                &authors,
                since,
                cursor.as_ref(),
            )
            .await
        {
            Ok(page) => page,
            Err(_) => return Ok(failed("relay job outcome query failed")),
        };
        events.extend(page);
        match next {
            Some(next) => cursor = Some(next),
            None => {
                complete = true;
                break;
            }
        }
    }
    if !complete {
        return Ok(failed(
            "too many job events in this channel to read the outcome",
        ));
    }
    let (outcome, ignored) = classify(&target, &events);
    if ignored > 0 {
        eprintln!(
            "job.outcome {}: ignored {ignored} linked event(s) that failed verification",
            args.request_event_id
        );
    }
    outcome.validate().map_err(|_| StoreError::Integrity)?;
    Ok(BrokerResult::succeeded(ActionOutcome::JobOutcome(
        Box::new(outcome),
    )))
}

fn failed(message: &str) -> BrokerResult {
    BrokerResult::failed(BrokerError::new(BrokerErrorCode::ActionFailed, message))
}

/// What one verified event says about the request.
enum Finding {
    Accepted,
    Terminal(JobOutcomeState),
    Cancelled,
}

/// Fold relay events into the request's outcome, plus how many events that
/// name this request were ignored as unverifiable.
///
/// Precedence: this sender's cancel, then the recipient's terminal event,
/// then its acceptance. Within one class the earliest event (`created_at`,
/// then id) wins, so the answer does not depend on page order.
pub(crate) fn classify(target: &OutcomeTarget, events: &[nostr::Event]) -> (JobOutcome, usize) {
    let request_id = target.request.id.to_hex();
    let mut ignored = 0;
    let mut accepted: Vec<&nostr::Event> = Vec::new();
    let mut terminal: Vec<(&nostr::Event, JobOutcomeState)> = Vec::new();
    let mut cancelled: Vec<&nostr::Event> = Vec::new();
    for event in events {
        let names_request = event.tags.iter().any(|tag| {
            let parts = tag.as_slice();
            parts.first().map(String::as_str) == Some(tag::REQUEST)
                && parts.get(1) == Some(&request_id)
        });
        if !names_request {
            continue;
        }
        match finding(target, &request_id, event) {
            Some(Finding::Accepted) => accepted.push(event),
            Some(Finding::Terminal(state)) => terminal.push((event, state)),
            Some(Finding::Cancelled) => cancelled.push(event),
            None => ignored += 1,
        }
    }
    let order = |event: &nostr::Event| (event.created_at, event.id);
    let first_accepted = accepted.iter().copied().min_by_key(|event| order(event));
    let first_terminal = terminal.into_iter().min_by_key(|(event, _)| order(event));
    let first_cancel = cancelled.into_iter().min_by_key(|event| order(event));
    let acceptance = first_accepted.map(|event| event.id.to_hex());
    let (state, acceptance_event_id, terminal_event) = match (first_cancel, first_terminal) {
        (Some(cancel), _) => (JobOutcomeState::Cancelled, acceptance, Some(cancel)),
        (None, Some((event, state))) => {
            // A result or error names the acceptance it completes.
            let linked = match state {
                JobOutcomeState::Result | JobOutcomeState::Error => {
                    single_tag(&event.tags, tag::ACCEPTED).ok()
                }
                _ => None,
            };
            (state, linked, Some(event))
        }
        (None, None) => match acceptance {
            Some(id) => (JobOutcomeState::Accepted, Some(id), None),
            None => (JobOutcomeState::Pending, None, None),
        },
    };
    (
        JobOutcome {
            state,
            request_event_id: request_id,
            acceptance_event_id,
            terminal_event: terminal_event.map(|event| BrokerMessage(event.clone())),
        },
        ignored,
    )
}

/// Check one event that names the request; `None` means ignore it.
fn finding(target: &OutcomeTarget, request_id: &str, event: &nostr::Event) -> Option<Finding> {
    let one = |name: &str| single_tag(&event.tags, name).ok();
    let sender = target.request.pubkey.to_hex();
    let author = event.pubkey.to_hex();
    let channel = one("h")?;
    if event.verify().is_err()
        || one(tag::REQUEST).as_deref() != Some(request_id)
        || single_tag(&target.request.tags, "h").ok().as_deref() != Some(channel.as_str())
    {
        return None;
    }
    let (author_ok, addressee) = match event.kind.as_u16() {
        43005 => (author == sender, &target.recipient),
        43002 | 43004 | 43006 => (author == target.recipient, &sender),
        _ => return None,
    };
    if !author_ok || one("p").as_deref() != Some(addressee.as_str()) {
        return None;
    }
    let identity = [
        (tag::PIPELINE_ID, target.handoff.pipeline_id.as_str()),
        (tag::CANDIDATE_SHA, target.handoff.candidate_sha.as_str()),
        (
            tag::PREVIEW_BUNDLE_DIGEST,
            target.handoff.preview_bundle_digest.as_str(),
        ),
    ];
    let present = |name: &str| {
        event
            .tags
            .iter()
            .any(|tag| tag.as_slice().first().map(String::as_str) == Some(name))
    };
    let identity_matches = |required: bool| {
        identity.iter().all(|(name, expected)| match one(name) {
            Some(value) => value == *expected,
            None => !required && !present(name),
        })
    };
    let accepted_link = present(tag::ACCEPTED);
    let linked_hex = || {
        one(tag::ACCEPTED)
            .is_some_and(|id| id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit()))
    };
    match event.kind.as_u16() {
        43005 if identity_matches(true) => Some(Finding::Cancelled),
        43002 if identity_matches(true) => Some(Finding::Accepted),
        43004 if identity_matches(true) && linked_hex() => {
            Some(Finding::Terminal(JobOutcomeState::Result))
        }
        // An error after acceptance links it and repeats the identity; a
        // denial before any work may carry only the request and a reason.
        43006 if accepted_link && linked_hex() && identity_matches(true) => {
            Some(Finding::Terminal(JobOutcomeState::Error))
        }
        43006 if !accepted_link && identity_matches(false) => {
            Some(Finding::Terminal(JobOutcomeState::Denied))
        }
        _ => None,
    }
}
