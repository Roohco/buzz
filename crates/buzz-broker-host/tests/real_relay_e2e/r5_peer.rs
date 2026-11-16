//! R5 proof: one scoped Figma → QA handoff on a real relay.
//!
//! Two keyless agents (Figma sender, QA recipient) hold only broker bearers.
//! Their brokers share one authority state, which also holds the operator
//! grant. The QA "work" is a deterministic fixture (no model, no browser).
//!
//! Run: `RELAY_URL=ws://localhost:3000 cargo test -p buzz-broker-host --test
//! real_relay_e2e r5_peer::r5_scoped_peer_handoff_against_real_relay -- --ignored --exact`.

use buzz_broker_client::HttpBrokerClient;
use buzz_sdk::broker::actions::job::{single_tag, tag};
use buzz_sdk::broker::{
    ActionArgs, ActionOutcome, BrokerErrorCode, BrokerResult, JobAdmission, JobAdmitArgs,
    JobDecision, JobHandoff, JobRequestArgs, JobResultArgs, JobVerdict, PubkeyHex,
    CAPABILITY_QA_REVIEW_CANDIDATE,
};
use nostr::{Event, EventBuilder, Keys, Kind, Tag};
use serde_json::json;
use sha2::{Digest as _, Sha256};

use super::support::*;

const CANDIDATE_SHA: &str = "0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c";

fn handoff(key: &str, parent: &str) -> JobHandoff {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    JobHandoff {
        capability: CAPABILITY_QA_REVIEW_CANDIDATE.into(),
        pipeline_id: format!("r5-pipeline-{key}"),
        stage_id: "qa-review".into(),
        parent_event_id: parent.into(),
        candidate_sha: CANDIDATE_SHA.into(),
        preview_bundle_digest: format!("sha256:{}", hex::encode(Sha256::digest(b"r5 bundle"))),
        budget_seconds: 300,
        deadline: now + 3600,
        attempt: 1,
        idempotency_key: format!("r5-{key}"),
    }
}

async fn add_member(relay: &Relay, owner: &Keys, channel: &str, member: &Keys) {
    let event = EventBuilder::new(Kind::Custom(9000), "")
        .tags([
            Tag::parse(["h", channel]).unwrap(),
            Tag::parse(["p", &member.public_key().to_hex()]).unwrap(),
        ])
        .sign_with_keys(owner)
        .unwrap();
    let (accepted, message) = relay.submit(owner, &event).await;
    assert!(accepted, "owner adds member: {message}");
}

fn request_args(channel: &str, qa: &Keys, handoff: JobHandoff) -> ActionArgs {
    ActionArgs::JobRequest(JobRequestArgs {
        channel_id: channel.to_owned(),
        recipient_pubkey: PubkeyHex::parse(qa.public_key().to_hex()).unwrap(),
        handoff,
    })
}

fn admit_args(channel: &str) -> ActionArgs {
    ActionArgs::JobAdmit(JobAdmitArgs {
        channel_id: channel.to_owned(),
        request_event_id: None,
    })
}

fn published(verdict: &Verdict) -> String {
    match &verdict.result {
        BrokerResult::Succeeded {
            outcome:
                ActionOutcome::JobRequest(event)
                | ActionOutcome::JobResult(event)
                | ActionOutcome::JobCancel(event),
        } => event.event_id.clone(),
        _ => panic!("expected a published job event, got {}", verdict.label()),
    }
}

fn admission(verdict: &Verdict) -> JobAdmission {
    match &verdict.result {
        BrokerResult::Succeeded {
            outcome: ActionOutcome::JobAdmit(admission),
        } => admission.as_ref().clone(),
        _ => panic!("expected a job admission, got {}", verdict.label()),
    }
}

async fn relay_event(relay: &Relay, owner: &Keys, channel: &str, id: &str) -> Event {
    let (_, events) = relay
        .query(
            owner,
            json!({"ids": [id], "kinds": [43001, 43002, 43004, 43005, 43006], "#h": [channel], "limit": 2}),
        )
        .await;
    assert_eq!(events.len(), 1, "exactly one relay copy of {id}");
    assert!(events[0].verify().is_ok(), "relay event verifies");
    events[0].clone()
}

#[tokio::test]
#[ignore = "requires a real buzz-relay at RELAY_URL (scripts/start-relay-for-tests.sh)"]
async fn r5_scoped_peer_handoff_against_real_relay() {
    let relay = Relay::from_env();
    let proxy = RelayProxy::start(relay.authority()).await;
    let owner = Keys::generate();
    let figma = Keys::generate();
    let qa = Keys::generate();
    let outsider = Keys::generate();
    // Job kinds are not kind 9, so the owner attests these agents without a
    // kind restriction but with an expiry, as handoff agents should be.
    let expiry = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 3600;
    let conditions = format!("created_at<{expiry}");
    let figma_tag =
        buzz_sdk::nip_oa::compute_auth_tag(&owner, &figma.public_key(), &conditions).unwrap();
    let qa_tag = buzz_sdk::nip_oa::compute_auth_tag(&owner, &qa.public_key(), &conditions).unwrap();

    // One private handoff channel with the owner, both agents and one
    // ungranted member; a second channel stands in for another project.
    let channel = uuid::Uuid::new_v4().to_string();
    let other_channel = uuid::Uuid::new_v4().to_string();
    let (create, _) = create_private_channel(&relay, &owner, &figma, &channel).await;
    add_member(&relay, &owner, &channel, &qa).await;
    add_member(&relay, &owner, &channel, &outsider).await;
    create_private_channel(&relay, &owner, &figma, &other_channel).await;
    add_member(&relay, &owner, &other_channel, &qa).await;
    let parent = create.id.to_hex();

    // Both brokers share one authority state; the operator grant lives there.
    let figma_broker = BrokerHost::new(&figma, &figma_tag, proxy.url());
    let mut qa_broker = BrokerHost::new(&qa, &qa_tag, proxy.url());
    qa_broker.state = figma_broker.state.clone();
    let figma_identity = identity(
        &relay.ws_url,
        &figma,
        &uuid::Uuid::new_v4().to_string(),
        &uuid::Uuid::new_v4().to_string(),
    );
    let qa_identity = identity(
        &relay.ws_url,
        &qa,
        &uuid::Uuid::new_v4().to_string(),
        &uuid::Uuid::new_v4().to_string(),
    );
    let figma_bearer = figma_broker.issue(&figma_identity).await.expect("figma");
    let qa_bearer = qa_broker.issue(&qa_identity).await.expect("qa");
    let grant_id = uuid::Uuid::new_v4().to_string();
    let grant_file = figma_broker.directory.path().join("grant.json");
    std::fs::write(
        &grant_file,
        serde_json::to_vec(&json!({
            "grantId": grant_id,
            "sender": figma_identity,
            "recipientPubkey": qa.public_key().to_hex(),
            "channelId": channel,
            "capability": CAPABILITY_QA_REVIEW_CANDIDATE,
        }))
        .unwrap(),
    )
    .unwrap();
    let granted = figma_broker
        .run(&[
            "grant-issue".as_ref(),
            "--grant-file".as_ref(),
            grant_file.as_os_str(),
        ])
        .await;
    assert_eq!(cli_result(&granted).as_deref(), Ok("granted"));

    let (mut figma_serve, figma_url) = figma_broker.serve().await;
    let (mut qa_serve, qa_url) = qa_broker.serve().await;
    let figma_client = HttpBrokerClient::new(figma_url, figma_bearer).unwrap();
    let qa_client = HttpBrokerClient::new(qa_url, qa_bearer).unwrap();

    // (a) Authorized request; a duplicate send replays the same event.
    let request = prepare(
        "r5-request-1",
        request_args(&channel, &qa, handoff("one", &parent)),
    );
    let request_id = published(&send(&figma_client, &request).await);
    let duplicate = send(&figma_client, &request).await;
    assert!(duplicate.replayed, "duplicate request replays");
    assert_eq!(published(&duplicate), request_id);
    let request_event = relay_event(&relay, &owner, &channel, &request_id).await;
    assert_eq!(request_event.kind.as_u16(), 43001);
    assert_eq!(request_event.pubkey, figma.public_key());
    assert_eq!(
        single_tag(&request_event.tags, "p").unwrap(),
        qa.public_key().to_hex()
    );

    // (b) QA admits it exactly once and runs the deterministic fixture once.
    let mut runs = Vec::new();
    for attempt in 0..3 {
        let decided = admission(
            &send(
                &qa_client,
                &prepare(&format!("r5-admit-{attempt}"), admit_args(&channel)),
            )
            .await,
        );
        match decided.decision {
            JobDecision::Accepted => runs.push(decided),
            JobDecision::None => {}
            other => panic!("unexpected decision {other:?}"),
        }
    }
    assert_eq!(runs.len(), 1, "duplicate delivery yields one run");
    let ticket = runs[0].ticket.clone().expect("ticket");
    assert_eq!(ticket.request_event_id, request_id);
    assert_eq!(ticket.handoff.candidate_sha, CANDIDATE_SHA);
    let accepted_id = runs[0].decision_event_id.clone().expect("acceptance id");
    let accepted = relay_event(&relay, &owner, &channel, &accepted_id).await;
    assert_eq!(accepted.kind.as_u16(), 43002);
    assert_eq!(accepted.pubkey, qa.public_key());
    assert_eq!(
        single_tag(&accepted.tags, tag::REQUEST).unwrap(),
        request_id
    );

    let evidence = hex::encode(Sha256::digest(
        format!(
            "{}|{}",
            ticket.handoff.candidate_sha, ticket.handoff.preview_bundle_digest
        )
        .as_bytes(),
    ));
    let result = send(
        &qa_client,
        &prepare(
            "r5-result-1",
            ActionArgs::JobResult(JobResultArgs {
                request_event_id: request_id.clone(),
                verdict: JobVerdict::Pass,
                summary: "fixture review: candidate bundle identity verified".into(),
                evidence_sha256: Some(evidence.clone()),
            }),
        ),
    )
    .await;
    let result_id = published(&result);
    let result_event = relay_event(&relay, &owner, &channel, &result_id).await;
    assert_eq!(result_event.kind.as_u16(), 43004);
    assert_eq!(result_event.pubkey, qa.public_key());
    for (name, expected) in [
        (tag::REQUEST, request_id.clone()),
        (tag::ACCEPTED, accepted_id.clone()),
        (tag::CANDIDATE_SHA, CANDIDATE_SHA.to_owned()),
        (
            tag::PREVIEW_BUNDLE_DIGEST,
            ticket.handoff.preview_bundle_digest.clone(),
        ),
        (tag::PIPELINE_ID, ticket.handoff.pipeline_id.clone()),
        (tag::VERDICT, "PASS".to_owned()),
        (tag::EVIDENCE_SHA256, evidence),
    ] {
        assert_eq!(
            single_tag(&result_event.tags, name).unwrap(),
            expected,
            "{name}"
        );
    }
    eprintln!("[r5] request={request_id} accepted={accepted_id} result={result_id}");

    // (c) Lost acknowledgement on the sender side reconciles to one event.
    proxy.set(DROP_RESPONSES);
    let lossy = prepare(
        "r5-request-2",
        request_args(&channel, &qa, handoff("two", &parent)),
    );
    let lost = send(&figma_client, &lossy).await;
    proxy.set(PASS);
    assert_eq!(lost.failure_code(), Some(BrokerErrorCode::OutcomeUnknown));
    let reconciled_id = published(&send(&figma_client, &lossy).await);
    relay_event(&relay, &owner, &channel, &reconciled_id).await;

    // (d) Ungranted peer and wrong project channel are denied with signed errors.
    let base = handoff("crafted", &parent);
    let crafted = |keys: &Keys, channel: &str| {
        let mut tags = vec![
            Tag::parse(["h", channel]).unwrap(),
            Tag::parse(["p", &qa.public_key().to_hex()]).unwrap(),
        ];
        for (name, value) in base.tag_pairs() {
            tags.push(Tag::parse([name, value.as_str()]).unwrap());
        }
        EventBuilder::new(Kind::Custom(43001), "crafted request")
            .tags(tags)
            .sign_with_keys(keys)
            .unwrap()
    };
    // Admit the reconciled request first so the scan reaches the crafted ones.
    let reconciled = admission(
        &send(
            &qa_client,
            &prepare(
                "r5-admit-reconciled",
                ActionArgs::JobAdmit(JobAdmitArgs {
                    channel_id: channel.clone(),
                    request_event_id: Some(reconciled_id.clone()),
                }),
            ),
        )
        .await,
    );
    assert_eq!(reconciled.decision, JobDecision::Accepted);
    for (name, keys, target, reason) in [
        (
            "ungranted peer",
            &outsider,
            channel.as_str(),
            "ungranted_peer",
        ),
        (
            "wrong channel",
            &figma,
            other_channel.as_str(),
            "channel_not_granted",
        ),
    ] {
        let event = crafted(keys, target);
        let (accepted, message) = relay.submit(keys, &event).await;
        assert!(accepted, "{name}: member may post a job event: {message}");
        let decided = admission(
            &send(
                &qa_client,
                &prepare(
                    &format!("r5-admit-{reason}"),
                    ActionArgs::JobAdmit(JobAdmitArgs {
                        channel_id: target.to_owned(),
                        request_event_id: Some(event.id.to_hex()),
                    }),
                ),
            )
            .await,
        );
        assert_eq!(decided.decision, JobDecision::Denied, "{name}");
        assert_eq!(decided.reason.as_deref(), Some(reason), "{name}");
        assert!(decided.ticket.is_none(), "{name}: no work input");
        let denial = relay_event(
            &relay,
            &owner,
            target,
            decided.decision_event_id.as_deref().unwrap(),
        )
        .await;
        assert_eq!(denial.kind.as_u16(), 43006);
        assert_eq!(single_tag(&denial.tags, tag::REASON).unwrap(), reason);
    }

    // (e) Fencing the sender generation denies its pending and new requests.
    let pending_id = published(
        &send(
            &figma_client,
            &prepare(
                "r5-request-3",
                request_args(&channel, &qa, handoff("three", &parent)),
            ),
        )
        .await,
    );
    let authority_file = figma_broker.authority_file(&figma_identity);
    let fenced = figma_broker
        .run(&[
            "fence".as_ref(),
            "--authority-file".as_ref(),
            authority_file.as_os_str(),
        ])
        .await;
    assert_eq!(cli_result(&fenced).as_deref(), Ok("fenced"));
    let decided = admission(
        &send(
            &qa_client,
            &prepare(
                "r5-admit-fenced",
                ActionArgs::JobAdmit(JobAdmitArgs {
                    channel_id: channel.clone(),
                    request_event_id: Some(pending_id),
                }),
            ),
        )
        .await,
    );
    assert_eq!(decided.reason.as_deref(), Some("sender_generation_fenced"));
    let refused = send(
        &figma_client,
        &prepare(
            "r5-request-4",
            request_args(&channel, &qa, handoff("four", &parent)),
        ),
    )
    .await;
    assert_eq!(refused.failure_code(), Some(BrokerErrorCode::Unauthorized));

    // Operator status is readable and holds no secrets.
    let status = figma_broker.run(&["peer-status".as_ref()]).await;
    let status = cli_result(&status).expect("peer status");
    assert!(status.contains("\"state\":\"completed\""));
    assert!(status.contains("sender_generation_fenced"));
    assert!(status.contains("\"senderGenerationState\":\"fenced\""));
    assert!(!status.contains("bzh_"), "no bearer in status");

    let revoked = figma_broker
        .run(&[
            "grant-revoke".as_ref(),
            "--grant-id".as_ref(),
            grant_id.as_ref(),
        ])
        .await;
    assert_eq!(cli_result(&revoked).as_deref(), Ok("revoked"));
    figma_serve.kill().await.ok();
    qa_serve.kill().await.ok();
}
