//! R1 proof: the broker host against a real `buzz-relay`.
//!
//! Runs the actual `buzz-broker-host` binary (issue, serve, quiesce, drain,
//! fence) against the relay at `RELAY_URL`, with a keyless client that holds
//! only a broker bearer. The broker's relay traffic goes through an in-test
//! HTTP proxy that the test can make drop responses or refuse connections, so
//! lost ACKs and relay outages are exercised without stopping the shared relay.
//!
//! Every event read back from the relay is verified here (id + Schnorr
//! signature), independently of the broker. When `R1_EVIDENCE_OUT` is set, a
//! redacted machine-readable timeline is written there: no private keys,
//! bearers or owner attestations.
//!
//! The broker addresses the relay by the authority's canonical community URL,
//! which maps loopback hosts to `127.0.0.1`; the relay's `communities` table
//! must map `127.0.0.1:<port>` (CI seeds it after `start-relay-for-tests.sh`).
//!
//! Run: `RELAY_URL=ws://localhost:3000 cargo test -p buzz-broker-host --test
//! real_relay_e2e r1_broker_authority_against_real_relay -- --ignored --exact`.
//! The ACP liveness test also needs `BUZZ_E2E_ACP_BIN` and `BUZZ_E2E_CLI_BIN`.
//! `BUZZ_E2E_BROKER_HOST_BIN` runs an exact published broker binary instead of
//! the Cargo-built one; the rollback rehearsal also needs
//! `BUZZ_E2E_ROLLBACK_BROKER_HOST_BIN`.

#![cfg(unix)]

mod acp_liveness;
mod rollback;
mod support;

use std::path::Path;

use buzz_broker_client::HttpBrokerClient;
use buzz_sdk::broker::{
    ActionArgs, ActionOutcome, BrokerErrorCode, BrokerResult, ChannelReadArgs, MessageReplyArgs,
};
use nostr::Keys;
use serde_json::json;

use support::*;

// ── The proof ───────────────────────────────────────────────────────────────

#[tokio::test]
#[ignore = "requires a real buzz-relay at RELAY_URL (scripts/start-relay-for-tests.sh)"]
async fn r1_broker_authority_against_real_relay() {
    let relay = Relay::from_env();
    let proxy = RelayProxy::start(relay.authority()).await;
    let mut timeline = Timeline::default();

    let owner = Keys::generate();
    let agent = Keys::generate();
    let outsider = Keys::generate();
    let auth_tag =
        buzz_sdk::nip_oa::compute_auth_tag(&owner, &agent.public_key(), "kind=9").unwrap();
    let run_id = uuid::Uuid::new_v4().to_string();
    let channel_uuid = uuid::Uuid::new_v4();
    let channel = channel_uuid.to_string();

    // (a) Owner creates a private channel, adds the agent, posts the task.
    let (create, add_agent) = create_private_channel(&relay, &owner, &agent, &channel).await;
    timeline.push(Step {
        step: "owner_private_channel_and_agent_member",
        outcome: "accepted".into(),
        event_ids: vec![create.id.to_hex(), add_agent.id.to_hex()],
        ..Step::default()
    });

    let task_content = format!("R1 task run_id={run_id}: acknowledge with a reply");
    let agent_hex = agent.public_key().to_hex();
    let task =
        buzz_sdk::build_message(channel_uuid, &task_content, None, &[&agent_hex], false, &[])
            .unwrap()
            .sign_with_keys(&owner)
            .unwrap();
    let task_id = task.id.to_hex();
    let (accepted, message) = relay.submit(&owner, &task).await;
    assert!(accepted, "owner task accepted with its event id: {message}");
    let (_, read) = relay
        .query(
            &owner,
            json!({"ids": [task_id], "#h": [channel], "limit": 1}),
        )
        .await;
    assert_eq!(read.len(), 1, "owner authenticated read returns the task");
    assert_eq!(read[0].id, task.id);
    assert_eq!(read[0].kind.as_u16(), 9);
    assert!(read[0]
        .tags
        .iter()
        .any(|tag| { tag.kind().to_string() == "p" && tag.content() == Some(agent_hex.as_str()) }));
    let verified = timeline.verify(&read[0]);
    timeline.push(Step {
        step: "owner_task_ack_and_read",
        outcome: "same_event_id".into(),
        event_ids: vec![task_id.clone()],
        relay_copies: Some(read.len()),
        signatures_verified: Some(verified),
        ..Step::default()
    });

    // (b) Broker issues one generation; the keyless client reads and replies.
    let broker = BrokerHost::new(&agent, &auth_tag, proxy.url());
    let generation_1 = uuid::Uuid::new_v4().to_string();
    let identity_1 = identity(&relay.ws_url, &agent, &run_id, &generation_1);
    let bearer_1 = broker
        .issue(&identity_1)
        .await
        .expect("issue first generation");
    let (mut serve, broker_url) = broker.serve().await;
    timeline.push(Step {
        step: "broker_issue",
        generation: Some(generation_1.clone()),
        outcome: "issued".into(),
        ..Step::default()
    });
    // The client holds only the bearer: no private key, no auth tag, no relay.
    let keyless = HttpBrokerClient::new(broker_url.clone(), bearer_1.clone()).unwrap();

    let read_request_id = format!("r1-read-{run_id}");
    let verdict = send(
        &keyless,
        &prepare(
            &read_request_id,
            ActionArgs::ChannelRead(ChannelReadArgs {
                channel_id: channel.clone(),
                mentions_only: true,
                ..ChannelReadArgs::default()
            }),
        ),
    )
    .await;
    let BrokerResult::Succeeded {
        outcome: ActionOutcome::ChannelRead(page),
    } = &verdict.result
    else {
        panic!("keyless channel.read failed: {}", verdict.label());
    };
    let seen: Vec<String> = page.messages.iter().map(|m| m.0.id.to_hex()).collect();
    assert!(seen.contains(&task_id), "broker read returns the task");
    for message in &page.messages {
        timeline.verify(&message.0);
    }
    timeline.push(Step {
        step: "keyless_channel_read",
        request_id: Some(read_request_id),
        generation: Some(generation_1.clone()),
        action: Some("channel.read"),
        outcome: verdict.label(),
        event_ids: vec![task_id.clone()],
        signatures_verified: Some(true),
        ..Step::default()
    });

    let reply_request_id = format!("r1-reply-{run_id}");
    let reply_content = format!("R1 ack run_id={run_id}");
    let reply_request = prepare(
        &reply_request_id,
        ActionArgs::MessageReply(MessageReplyArgs {
            channel_id: channel.clone(),
            reply_to_event_id: task_id.clone(),
            content: reply_content.clone(),
            mentions: Vec::new(),
        }),
    );
    let verdict = send(&keyless, &reply_request).await;
    let reply_id = verdict.published_event_id();
    let (_, read) = relay
        .query(
            &owner,
            json!({"ids": [reply_id], "#h": [channel], "limit": 1}),
        )
        .await;
    assert_eq!(read.len(), 1, "relay returns the broker reply");
    let reply = &read[0];
    assert_eq!(reply.id.to_hex(), reply_id);
    assert_eq!(
        reply.pubkey,
        agent.public_key(),
        "reply is signed by the agent"
    );
    assert_eq!(reply.kind.as_u16(), 9);
    assert!(
        has_reply_parent(reply, &task_id),
        "NIP-10 reply to the task"
    );
    let attesting_owners: Vec<_> = reply
        .tags
        .iter()
        .filter_map(|tag| {
            let encoded = serde_json::to_string(tag).ok()?;
            buzz_sdk::nip_oa::verify_auth_tag(&encoded, &reply.pubkey).ok()
        })
        .collect();
    assert_eq!(
        attesting_owners,
        vec![owner.public_key()],
        "owner attestation"
    );
    let verified = timeline.verify(reply);
    timeline.push(Step {
        step: "keyless_message_reply",
        request_id: Some(reply_request_id.clone()),
        generation: Some(generation_1.clone()),
        action: Some("message.reply"),
        outcome: verdict.label(),
        replayed: Some(verdict.replayed),
        event_ids: vec![reply_id.clone()],
        relay_copies: Some(1),
        signatures_verified: Some(verified),
    });

    // (c) An outsider can neither read the private channel nor post into it,
    // and a bearer the host never issued gets no verdict but unauthenticated.
    let (status, events) = relay
        .query(
            &outsider,
            json!({"kinds": [9], "#h": [channel], "limit": 50}),
        )
        .await;
    assert!(
        events.is_empty(),
        "outsider reads nothing (status {status})"
    );
    let intrusion =
        buzz_sdk::build_message(channel_uuid, "outsider", None, &[&agent_hex], false, &[])
            .unwrap()
            .sign_with_keys(&outsider)
            .unwrap();
    let (accepted, message) = relay.submit(&outsider, &intrusion).await;
    assert!(!accepted, "outsider post rejected");
    timeline.push(Step {
        step: "outsider_denied",
        outcome: format!("read_events=0 status={status}; post_rejected={message}"),
        ..Step::default()
    });
    let stranger = HttpBrokerClient::new(broker_url.clone(), random_bearer()).unwrap();
    let verdict = send(&stranger, &prepare("r1-stranger", post_args(&channel, "x"))).await;
    assert_eq!(
        verdict.failure_code(),
        Some(BrokerErrorCode::Unauthenticated)
    );
    timeline.push(Step {
        step: "unissued_bearer_denied",
        request_id: Some("r1-stranger".into()),
        action: Some("message.post"),
        outcome: verdict.label(),
        ..Step::default()
    });

    // (d) Exact retry returns the same event; changed bytes conflict.
    let verdict = send(&keyless, &reply_request).await;
    assert_eq!(verdict.published_event_id(), reply_id, "exact retry");
    assert!(verdict.replayed, "exact retry is a replay");
    let copies = copies_with_content(
        &relay,
        &owner,
        &channel,
        &agent,
        &reply_content,
        &mut timeline,
    )
    .await;
    assert_eq!(copies.len(), 1, "relay holds one reply");
    timeline.push(Step {
        step: "exact_retry_same_event",
        request_id: Some(reply_request_id.clone()),
        generation: Some(generation_1.clone()),
        action: Some("message.reply"),
        outcome: verdict.label(),
        replayed: Some(verdict.replayed),
        event_ids: vec![reply_id.clone()],
        relay_copies: Some(copies.len()),
        signatures_verified: Some(true),
    });
    let changed = prepare(
        &reply_request_id,
        ActionArgs::MessageReply(MessageReplyArgs {
            channel_id: channel.clone(),
            reply_to_event_id: task_id.clone(),
            content: format!("{reply_content} (changed)"),
            mentions: Vec::new(),
        }),
    );
    let verdict = send(&keyless, &changed).await;
    assert_eq!(
        verdict.failure_code(),
        Some(BrokerErrorCode::RequestIdConflict)
    );
    timeline.push(Step {
        step: "changed_bytes_conflict",
        request_id: Some(reply_request_id.clone()),
        generation: Some(generation_1.clone()),
        action: Some("message.reply"),
        outcome: verdict.label(),
        ..Step::default()
    });

    // (e1) Lost ACK: the relay stores the event but the broker never sees the
    // answer. The exact retry reconciles by event id without a second copy.
    let lost_request_id = format!("r1-lost-ack-{run_id}");
    let lost_content = format!("R1 lost-ack run_id={run_id}");
    let lost_request = prepare(&lost_request_id, post_args(&channel, &lost_content));
    proxy.set(DROP_RESPONSES);
    let verdict = send(&keyless, &lost_request).await;
    proxy.set(PASS);
    assert_eq!(
        verdict.failure_code(),
        Some(BrokerErrorCode::OutcomeUnknown)
    );
    let copies = copies_with_content(
        &relay,
        &owner,
        &channel,
        &agent,
        &lost_content,
        &mut timeline,
    )
    .await;
    assert_eq!(copies.len(), 1, "relay stored the event whose ACK was lost");
    let stored_id = copies[0].id.to_hex();
    timeline.push(Step {
        step: "lost_ack_outcome_unknown",
        request_id: Some(lost_request_id.clone()),
        generation: Some(generation_1.clone()),
        action: Some("message.post"),
        outcome: verdict.label(),
        event_ids: vec![stored_id.clone()],
        relay_copies: Some(copies.len()),
        signatures_verified: Some(true),
        ..Step::default()
    });
    let verdict = send(&keyless, &lost_request).await;
    assert_eq!(verdict.published_event_id(), stored_id, "retry reconciles");
    let copies = copies_with_content(
        &relay,
        &owner,
        &channel,
        &agent,
        &lost_content,
        &mut timeline,
    )
    .await;
    assert_eq!(copies.len(), 1, "no second copy after reconciliation");
    timeline.push(Step {
        step: "lost_ack_exact_retry_reconciled",
        request_id: Some(lost_request_id),
        generation: Some(generation_1.clone()),
        action: Some("message.post"),
        outcome: verdict.label(),
        replayed: Some(verdict.replayed),
        event_ids: vec![stored_id],
        relay_copies: Some(copies.len()),
        signatures_verified: Some(true),
    });

    // (e2) Relay outage: the effect is journaled but unreachable.
    let outage_request_id = format!("r1-outage-{run_id}");
    let outage_content = format!("R1 outage run_id={run_id}");
    let outage_request = prepare(&outage_request_id, post_args(&channel, &outage_content));
    proxy.set(REFUSE);
    let verdict = send(&keyless, &outage_request).await;
    assert_eq!(
        verdict.failure_code(),
        Some(BrokerErrorCode::OutcomeUnknown)
    );
    let copies = copies_with_content(
        &relay,
        &owner,
        &channel,
        &agent,
        &outage_content,
        &mut timeline,
    )
    .await;
    assert!(
        copies.is_empty(),
        "nothing reached the relay during the cut"
    );
    timeline.push(Step {
        step: "outage_outcome_unknown",
        request_id: Some(outage_request_id.clone()),
        generation: Some(generation_1.clone()),
        action: Some("message.post"),
        outcome: verdict.label(),
        relay_copies: Some(0),
        ..Step::default()
    });
    let verdict = send(
        &keyless,
        &prepare(
            &format!("r1-blocked-{run_id}"),
            post_args(&channel, "blocked while pending"),
        ),
    )
    .await;
    assert_eq!(
        verdict.failure_code(),
        Some(BrokerErrorCode::ActionFailed),
        "a second effect is not admitted while one is pending"
    );
    timeline.push(Step {
        step: "second_effect_blocked_while_pending",
        generation: Some(generation_1.clone()),
        action: Some("message.post"),
        outcome: verdict.label(),
        ..Step::default()
    });

    let quiesced = broker.lifecycle("quiesce", &identity_1).await;
    assert_eq!(quiesced.as_deref(), Ok("quiescing"));
    let fence = broker.lifecycle("fence", &identity_1).await;
    let fence_error = fence.expect_err("fence refused while an effect is pending");
    assert!(fence_error.contains("EffectsPending"), "{fence_error}");
    let generation_2 = uuid::Uuid::new_v4().to_string();
    let identity_2 = identity(&relay.ws_url, &agent, &run_id, &generation_2);
    let successor = broker.issue(&identity_2).await;
    let successor_error = successor.expect_err("successor refused while pending");
    assert!(
        successor_error.contains("ScopeAlreadyActive"),
        "{successor_error}"
    );
    timeline.push(Step {
        step: "quiesce_fence_successor_while_pending",
        generation: Some(generation_1.clone()),
        outcome: "quiescing; fence=EffectsPending; successor=ScopeAlreadyActive".into(),
        ..Step::default()
    });

    // Restore the relay path; operator drain publishes the journaled event.
    proxy.set(PASS);
    let drained = broker.drain(&identity_1).await;
    assert_eq!(drained.as_deref(), Ok("drained"));
    let copies = copies_with_content(
        &relay,
        &owner,
        &channel,
        &agent,
        &outage_content,
        &mut timeline,
    )
    .await;
    assert_eq!(copies.len(), 1, "drain published exactly one copy");
    let drained_id = copies[0].id.to_hex();
    let verdict = send(&keyless, &outage_request).await;
    assert_eq!(verdict.published_event_id(), drained_id, "exact retry");
    let copies = copies_with_content(
        &relay,
        &owner,
        &channel,
        &agent,
        &outage_content,
        &mut timeline,
    )
    .await;
    assert_eq!(copies.len(), 1, "exact retry adds no copy");
    timeline.push(Step {
        step: "drain_and_exact_retry_reconciled",
        request_id: Some(outage_request_id),
        generation: Some(generation_1.clone()),
        action: Some("message.post"),
        outcome: verdict.label(),
        replayed: Some(verdict.replayed),
        event_ids: vec![drained_id],
        relay_copies: Some(copies.len()),
        signatures_verified: Some(true),
    });

    // Quiesce stops new work even with the effect resolved.
    let verdict = send(
        &keyless,
        &prepare(
            &format!("r1-quiesced-{run_id}"),
            post_args(&channel, "after quiesce"),
        ),
    )
    .await;
    assert_eq!(
        verdict.failure_code(),
        Some(BrokerErrorCode::Unauthorized),
        "quiesced generation refuses work"
    );
    timeline.push(Step {
        step: "quiesced_generation_refuses_new_effect",
        generation: Some(generation_1.clone()),
        action: Some("message.post"),
        outcome: verdict.label(),
        ..Step::default()
    });
    let fenced = broker.lifecycle("fence", &identity_1).await;
    assert_eq!(fenced.as_deref(), Ok("fenced"));
    timeline.push(Step {
        step: "fence_after_drain",
        generation: Some(generation_1.clone()),
        outcome: "fenced".into(),
        ..Step::default()
    });

    // (f) The old bearer and generation are dead; a fresh generation works.
    let stale_content = format!("R1 stale run_id={run_id}");
    let verdict = send(
        &keyless,
        &prepare(
            &format!("r1-stale-{run_id}"),
            post_args(&channel, &stale_content),
        ),
    )
    .await;
    assert_eq!(
        verdict.failure_code(),
        Some(BrokerErrorCode::Unauthorized),
        "fenced bearer cannot publish"
    );
    let copies = copies_with_content(
        &relay,
        &owner,
        &channel,
        &agent,
        &stale_content,
        &mut timeline,
    )
    .await;
    assert!(copies.is_empty());
    timeline.push(Step {
        step: "fenced_bearer_denied",
        generation: Some(generation_1.clone()),
        action: Some("message.post"),
        outcome: verdict.label(),
        relay_copies: Some(0),
        ..Step::default()
    });
    let reissue = broker.issue(&identity_1).await;
    let reissue_error = reissue.expect_err("old generation cannot be reissued");
    assert!(
        reissue_error.contains("GenerationAlreadyIssued"),
        "{reissue_error}"
    );
    timeline.push(Step {
        step: "old_generation_reissue_denied",
        generation: Some(generation_1.clone()),
        outcome: "GenerationAlreadyIssued".into(),
        ..Step::default()
    });

    let bearer_2 = broker
        .issue(&identity_2)
        .await
        .expect("successor issues after the prior effect is terminal");
    let fresh = HttpBrokerClient::new(broker_url.clone(), bearer_2.clone()).unwrap();
    let fresh_request_id = format!("r1-fresh-{run_id}");
    let fresh_content = format!("R1 successor run_id={run_id}");
    let verdict = send(
        &fresh,
        &prepare(&fresh_request_id, post_args(&channel, &fresh_content)),
    )
    .await;
    let fresh_id = verdict.published_event_id();
    let copies = copies_with_content(
        &relay,
        &owner,
        &channel,
        &agent,
        &fresh_content,
        &mut timeline,
    )
    .await;
    assert_eq!(copies.len(), 1);
    assert_eq!(copies[0].id.to_hex(), fresh_id);
    timeline.push(Step {
        step: "fresh_generation_publishes",
        request_id: Some(fresh_request_id),
        generation: Some(generation_2.clone()),
        action: Some("message.post"),
        outcome: verdict.label(),
        replayed: Some(verdict.replayed),
        event_ids: vec![fresh_id],
        relay_copies: Some(1),
        signatures_verified: Some(true),
    });
    assert_eq!(
        broker.lifecycle("quiesce", &identity_2).await.as_deref(),
        Ok("quiescing")
    );
    assert_eq!(
        broker.lifecycle("fence", &identity_2).await.as_deref(),
        Ok("fenced")
    );
    timeline.push(Step {
        step: "teardown_successor_fenced",
        generation: Some(generation_2.clone()),
        outcome: "fenced".into(),
        ..Step::default()
    });
    let _ = serve.kill().await;

    // (g) Redacted evidence: no keys, bearers or attestations.
    let evidence = json!({
        "schema": "rooh.r1.broker-real-relay/v1",
        "source_revision": std::env::var("GITHUB_SHA").ok(),
        "relay_url": relay.ws_url,
        "channel_id": channel,
        "run_id": run_id,
        "task_event_id": task_id,
        "reply_event_id": reply_id,
        "independently_verified_event_ids": timeline.verified_event_ids,
        "steps": timeline.steps,
    });
    let rendered = serde_json::to_string_pretty(&evidence).unwrap();
    for secret in [
        bearer_1.as_str(),
        bearer_2.as_str(),
        auth_tag.as_str(),
        &agent.secret_key().to_secret_hex(),
        &owner.secret_key().to_secret_hex(),
        &outsider.secret_key().to_secret_hex(),
        "bzh_",
    ] {
        assert!(!rendered.contains(secret), "evidence must stay redacted");
    }
    if let Some(path) = std::env::var_os("R1_EVIDENCE_OUT") {
        let path = Path::new(&path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("evidence directory");
        }
        std::fs::write(path, rendered.as_bytes()).expect("evidence written");
        eprintln!("[r1] evidence written to {}", path.display());
    }
}
