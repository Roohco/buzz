//! R5.1 `job.outcome`: the sender's read-only view of its own request.

use super::regressions::{fake_relay, Served};
use super::*;

use buzz_sdk::broker::{
    ActionOutcome, BrokerErrorCode, BrokerRequest, BrokerResult, JobOutcome, JobOutcomeArgs,
    JobOutcomeState,
};
use nostr::Timestamp;

use crate::peer::outcome::classify;
use crate::publisher::RelayPublisher;
use crate::store::peer_outcome::OutcomeTarget;

/// Pilot on a fake relay with one published Figma request.
async fn relay_pilot() -> (tokio::task::JoinHandle<()>, Served, Pilot, nostr::Event) {
    let (relay, server, served) = fake_relay().await;
    let pilot = Pilot::with_relay(&relay).await;
    let request = pilot.published_request(handoff("outcome", SHA_A)).await;
    served.lock().unwrap().push(request.clone());
    (server, served, pilot, request)
}

/// Run `job.outcome` as `agent` and return the broker result.
async fn read_as(pilot: &Pilot, agent: &Agent, channel: &str, request_id: &str) -> BrokerResult {
    let request = BrokerRequest::new(
        "outcome-read",
        ActionArgs::JobOutcome(JobOutcomeArgs {
            channel_id: channel.into(),
            request_event_id: request_id.into(),
        }),
    )
    .unwrap()
    .validated()
    .unwrap();
    let publisher = RelayPublisher::new().unwrap();
    crate::peer::handle(
        &pilot.store,
        &agent.signer,
        &publisher,
        &agent.credential,
        &request,
    )
    .await
    .expect("job action")
    .result
}

fn outcome_of(result: BrokerResult) -> JobOutcome {
    match result {
        BrokerResult::Succeeded {
            outcome: ActionOutcome::JobOutcome(outcome),
        } => *outcome,
        other => panic!("expected a job outcome, got {other:?}"),
    }
}

fn refusal_code(result: &BrokerResult) -> Option<BrokerErrorCode> {
    match result {
        BrokerResult::Failed { error } => Some(error.code),
        _ => None,
    }
}

fn target(pilot: &Pilot, request: &nostr::Event) -> OutcomeTarget {
    OutcomeTarget {
        relay_url: pilot.figma.identity.community_relay_url.clone(),
        request: request.clone(),
        recipient: pilot.qa.keys.public_key().to_hex(),
        handoff: JobHandoff::from_tags(&request.tags).unwrap(),
    }
}

/// A job event signed directly with `keys`, as a recipient or forger would.
fn job_event(
    keys: &Keys,
    kind: u16,
    addressee: &Keys,
    tags: Vec<(&str, String)>,
    created_at: u64,
) -> nostr::Event {
    let mut all = vec![
        Tag::parse(["h", CHANNEL_ID]).unwrap(),
        Tag::parse(["p", &addressee.public_key().to_hex()]).unwrap(),
    ];
    for (name, value) in tags {
        all.push(Tag::parse([name, value.as_str()]).unwrap());
    }
    EventBuilder::new(Kind::Custom(kind), "job event")
        .tags(all)
        .custom_created_at(Timestamp::from(created_at))
        .sign_with_keys(keys)
        .unwrap()
}

/// The request, acceptance link and handoff identity tags a result repeats.
fn linked(request: &nostr::Event, accepted: Option<&str>) -> Vec<(&'static str, String)> {
    let handoff = JobHandoff::from_tags(&request.tags).unwrap();
    let mut tags = vec![(tag::REQUEST, request.id.to_hex())];
    if let Some(accepted) = accepted {
        tags.push((tag::ACCEPTED, accepted.to_owned()));
    }
    tags.extend(handoff.tag_pairs());
    tags
}

async fn accept(pilot: &Pilot, request: &nostr::Event) -> nostr::Event {
    let step = pilot
        .store
        .decide_job(
            &pilot.qa.credential,
            &pilot.qa.signer,
            CHANNEL_ID,
            request,
            NOW,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(step.admission.decision, JobDecision::Accepted);
    step.publish.expect("acceptance").0
}

async fn finish(pilot: &Pilot, request: &nostr::Event, verdict: JobVerdict) -> nostr::Event {
    let PeerStep::Publish { event, .. } = pilot
        .store
        .prepare_job_result(
            &pilot.qa.credential,
            &pilot.qa.signer,
            &JobResultArgs {
                request_event_id: request.id.to_hex(),
                verdict,
                summary: "fixture review".into(),
                evidence_sha256: None,
            },
            NOW,
        )
        .await
        .unwrap()
    else {
        panic!("accepted job yields a result");
    };
    event
}

/// pending → accepted → result through the action, read by the sender; the
/// read is idempotent and still allowed once the sender generation is fenced.
#[tokio::test]
async fn sender_reads_pending_accepted_then_result() {
    let (server, served, pilot, request) = relay_pilot().await;
    let id = request.id.to_hex();

    let pending = outcome_of(read_as(&pilot, &pilot.figma, CHANNEL_ID, &id).await);
    assert_eq!(pending.state, JobOutcomeState::Pending);
    assert!(pending.acceptance_event_id.is_none() && pending.terminal_event.is_none());

    let accepted = accept(&pilot, &request).await;
    served.lock().unwrap().push(accepted.clone());
    let read = outcome_of(read_as(&pilot, &pilot.figma, CHANNEL_ID, &id).await);
    assert_eq!(read.state, JobOutcomeState::Accepted);
    assert_eq!(read.acceptance_event_id, Some(accepted.id.to_hex()));
    assert!(read.terminal_event.is_none());

    let result = finish(&pilot, &request, JobVerdict::Pass).await;
    served.lock().unwrap().push(result.clone());
    let done = outcome_of(read_as(&pilot, &pilot.figma, CHANNEL_ID, &id).await);
    assert_eq!(done.state, JobOutcomeState::Result);
    assert_eq!(done.acceptance_event_id, Some(accepted.id.to_hex()));
    let terminal = done.terminal_event.clone().expect("terminal event");
    assert_eq!(terminal.event(), &result);
    assert!(terminal.verify().is_ok());
    let wire = serde_json::to_value(&done).unwrap();
    for member in [
        "id",
        "pubkey",
        "created_at",
        "kind",
        "tags",
        "content",
        "sig",
    ] {
        assert!(wire["terminalEvent"].get(member).is_some(), "{member}");
    }

    // Idempotent, and readable after the generation is fenced.
    assert_eq!(
        outcome_of(read_as(&pilot, &pilot.figma, CHANNEL_ID, &id).await),
        done
    );
    pilot.store.fence(&pilot.figma.identity).await.unwrap();
    assert_eq!(
        outcome_of(read_as(&pilot, &pilot.figma, CHANNEL_ID, &id).await),
        done
    );
    server.abort();
}

/// Another sender (the QA generation here) and a mismatched channel are
/// refused from the journal, before any relay query.
#[tokio::test]
async fn foreign_request_or_channel_is_unauthorized() {
    // The relay URL does not resolve: a query attempt would fail differently.
    let pilot = Pilot::new().await;
    let request = pilot.published_request(handoff("foreign", SHA_A)).await;
    let id = request.id.to_hex();
    for (agent, channel) in [(&pilot.qa, CHANNEL_ID), (&pilot.figma, OTHER_CHANNEL)] {
        let result = read_as(&pilot, agent, channel, &id).await;
        assert_eq!(
            refusal_code(&result),
            Some(BrokerErrorCode::Unauthorized),
            "{result:?}"
        );
    }
    let unknown = read_as(&pilot, &pilot.figma, CHANNEL_ID, &"e".repeat(64)).await;
    assert_eq!(refusal_code(&unknown), Some(BrokerErrorCode::Unauthorized));
}

/// Cancel by this sender wins over a recipient result, which wins over the
/// acceptance; the earliest event of a class wins regardless of order.
#[tokio::test]
async fn cancel_takes_precedence_over_result_and_acceptance() {
    let pilot = Pilot::new().await;
    let request = pilot.published_request(handoff("precedence", SHA_A)).await;
    let accepted = accept(&pilot, &request).await;
    let result = finish(&pilot, &request, JobVerdict::Defect).await;
    let target = target(&pilot, &request);
    let cancel = job_event(
        &pilot.figma.keys,
        43005,
        &pilot.qa.keys,
        {
            let handoff = JobHandoff::from_tags(&request.tags).unwrap();
            vec![
                (tag::REQUEST, request.id.to_hex()),
                (tag::REASON, "cancelled_by_sender".into()),
                (tag::PIPELINE_ID, handoff.pipeline_id),
                (tag::CANDIDATE_SHA, handoff.candidate_sha),
                (tag::PREVIEW_BUNDLE_DIGEST, handoff.preview_bundle_digest),
            ]
        },
        request.created_at.as_secs() + 1,
    );

    let (only_result, ignored) = classify(&target, &[result.clone(), accepted.clone()]);
    assert_eq!(ignored, 0);
    assert_eq!(only_result.state, JobOutcomeState::Result);
    assert_eq!(only_result.acceptance_event_id, Some(accepted.id.to_hex()));

    let (cancelled, ignored) = classify(&target, &[result, cancel.clone(), accepted.clone()]);
    assert_eq!(ignored, 0);
    assert_eq!(cancelled.state, JobOutcomeState::Cancelled);
    assert_eq!(cancelled.terminal_event.map(|e| e.0.id), Some(cancel.id));
    assert_eq!(cancelled.acceptance_event_id, Some(accepted.id.to_hex()));

    // A cancel signed by the recipient, not the sender, is not a cancel.
    let forged = job_event(
        &pilot.qa.keys,
        43005,
        &pilot.qa.keys,
        linked(&request, None),
        request.created_at.as_secs(),
    );
    let (still, ignored) = classify(&target, &[accepted, forged]);
    assert_eq!((still.state, ignored), (JobOutcomeState::Accepted, 1));
}

/// A 43006 linked to an acceptance is an error; one without is a denial.
#[tokio::test]
async fn error_and_denial_are_told_apart() {
    let pilot = Pilot::new().await;
    let request = pilot.published_request(handoff("error", SHA_A)).await;
    let accepted = accept(&pilot, &request).await;
    let error = finish(&pilot, &request, JobVerdict::Error).await;
    assert_eq!(error.kind.as_u16(), 43006);
    let (read, ignored) = classify(
        &target(&pilot, &request),
        &[accepted.clone(), error.clone()],
    );
    assert_eq!(ignored, 0);
    assert_eq!(read.state, JobOutcomeState::Error);
    assert_eq!(read.acceptance_event_id, Some(accepted.id.to_hex()));
    assert_eq!(read.terminal_event.map(|e| e.0.id), Some(error.id));

    // A real denial: the recipient decides after the deadline.
    let late = pilot.published_request(handoff("late", SHA_A)).await;
    let step = pilot
        .store
        .decide_job(
            &pilot.qa.credential,
            &pilot.qa.signer,
            CHANNEL_ID,
            &late,
            NOW + 7200,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(step.admission.decision, JobDecision::Denied);
    let denial = step.publish.expect("signed denial").0;
    let (read, ignored) = classify(&target(&pilot, &late), std::slice::from_ref(&denial));
    assert_eq!(ignored, 0);
    assert_eq!(read.state, JobOutcomeState::Denied);
    assert!(read.acceptance_event_id.is_none());
    read.validate().expect("denial outcome shape");
    assert_eq!(read.terminal_event.map(|e| e.0.id), Some(denial.id));
}

/// Events from the wrong author, or repeating another identity, are ignored
/// and counted; unrelated requests' events are skipped silently.
#[tokio::test]
async fn wrong_author_and_tag_mismatch_are_ignored() {
    let pilot = Pilot::new().await;
    let request = pilot.published_request(handoff("forged", SHA_A)).await;
    let accepted = accept(&pilot, &request).await;
    let target = target(&pilot, &request);
    let at = request.created_at.as_secs() + 1;
    let accepted_id = accepted.id.to_hex();
    let outsider = Keys::generate();
    let wrong_author = job_event(
        &outsider,
        43004,
        &pilot.figma.keys,
        linked(&request, Some(&accepted_id)),
        at,
    );
    let mut other_sha = linked(&request, Some(&accepted_id));
    for (name, value) in &mut other_sha {
        if *name == tag::CANDIDATE_SHA {
            *value = SHA_B.into();
        }
    }
    let wrong_sha = job_event(&pilot.qa.keys, 43004, &pilot.figma.keys, other_sha, at);
    let no_pipeline: Vec<_> = linked(&request, Some(&accepted_id))
        .into_iter()
        .filter(|(name, _)| *name != tag::PIPELINE_ID)
        .collect();
    let missing = job_event(&pilot.qa.keys, 43004, &pilot.figma.keys, no_pipeline, at);
    let wrong_addressee = job_event(
        &pilot.qa.keys,
        43004,
        &outsider,
        linked(&request, Some(&accepted_id)),
        at,
    );
    let mut tampered = job_event(
        &pilot.qa.keys,
        43004,
        &pilot.figma.keys,
        linked(&request, Some(&accepted_id)),
        at,
    );
    tampered.content = "tampered".into();
    let unrelated = job_event(
        &pilot.qa.keys,
        43004,
        &pilot.figma.keys,
        vec![(tag::REQUEST, "f".repeat(64))],
        at,
    );
    let (read, ignored) = classify(
        &target,
        &[
            accepted.clone(),
            wrong_author,
            wrong_sha,
            missing,
            wrong_addressee,
            tampered,
            unrelated,
        ],
    );
    assert_eq!(ignored, 5);
    assert_eq!(read.state, JobOutcomeState::Accepted);
    assert_eq!(read.acceptance_event_id, Some(accepted_id));
}

/// The outcome is reached past a full page of the recipient's other job
/// events that share its second: the cursor follows `created_at DESC, id ASC`.
#[tokio::test]
async fn outcome_is_found_across_a_same_second_page_boundary() {
    let (server, served, pilot, request) = relay_pilot().await;
    let accepted = accept(&pilot, &request).await;
    let result = finish(&pilot, &request, JobVerdict::Pass).await;
    let second = result.created_at.as_secs();
    // 12 newer fillers fill most of page one; at least 4 fillers in the
    // result's own second sort before it, so it lands on page two.
    let mut fillers: Vec<nostr::Event> = (0..12)
        .map(|index| {
            job_event(
                &pilot.qa.keys,
                43002,
                &pilot.figma.keys,
                vec![(tag::REQUEST, format!("{index:064x}"))],
                second + 5,
            )
        })
        .collect();
    let mut before = 0;
    let mut attempt = 0_u64;
    while before < 4 {
        attempt += 1;
        assert!(attempt < 200_000, "filler search is bounded");
        let filler = job_event(
            &pilot.qa.keys,
            43002,
            &pilot.figma.keys,
            vec![(tag::REQUEST, format!("{:064x}", 1_000 + attempt))],
            second,
        );
        if filler.id.to_hex() < result.id.to_hex() {
            before += 1;
            fillers.push(filler);
        }
    }
    served.lock().unwrap().extend(fillers);
    served.lock().unwrap().push(result.clone());
    served.lock().unwrap().push(accepted.clone());
    let read = outcome_of(read_as(&pilot, &pilot.figma, CHANNEL_ID, &request.id.to_hex()).await);
    assert_eq!(read.state, JobOutcomeState::Result);
    assert_eq!(read.terminal_event.map(|e| e.0.id), Some(result.id));
    assert_eq!(read.acceptance_event_id, Some(accepted.id.to_hex()));
    server.abort();
}
