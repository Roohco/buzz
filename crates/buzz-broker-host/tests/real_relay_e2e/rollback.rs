//! R1 rollback rehearsal: retire a generation on the current broker artifact,
//! stop it, start the rollback artifact on the same authority state, and prove
//! the fenced generation stays dead while a fresh generation publishes once.
//!
//! The current artifact is `BUZZ_E2E_BROKER_HOST_BIN` (or the Cargo-built
//! binary); the rollback artifact is `BUZZ_E2E_ROLLBACK_BROKER_HOST_BIN`.

use std::path::{Path, PathBuf};

use buzz_broker_client::HttpBrokerClient;
use buzz_sdk::broker::BrokerErrorCode;
use nostr::Keys;
use serde_json::json;

use super::support::*;

#[tokio::test]
#[ignore = "requires a real buzz-relay at RELAY_URL plus BUZZ_E2E_ROLLBACK_BROKER_HOST_BIN"]
async fn r1_broker_rollback_keeps_fenced_generation_dead() {
    let rollback_bin = PathBuf::from(
        std::env::var_os("BUZZ_E2E_ROLLBACK_BROKER_HOST_BIN")
            .expect("rollback buzz-broker-host binary path"),
    );
    let relay = Relay::from_env();
    let proxy = RelayProxy::start(relay.authority()).await;
    let mut timeline = Timeline::default();
    let owner = Keys::generate();
    let agent = Keys::generate();
    let auth_tag =
        buzz_sdk::nip_oa::compute_auth_tag(&owner, &agent.public_key(), "kind=9").unwrap();
    let run_id = uuid::Uuid::new_v4().to_string();
    let channel = uuid::Uuid::new_v4().to_string();
    create_private_channel(&relay, &owner, &agent, &channel).await;

    // Current artifact: one generation publishes, then is retired.
    let mut broker = BrokerHost::new(&agent, &auth_tag, proxy.url());
    let generation_1 = uuid::Uuid::new_v4().to_string();
    let identity_1 = identity(&relay.ws_url, &agent, &run_id, &generation_1);
    let bearer_1 = broker.issue(&identity_1).await.expect("issue on current");
    let (mut serve, broker_url) = broker.serve().await;
    let current = HttpBrokerClient::new(broker_url, bearer_1.clone()).unwrap();
    let current_request_id = format!("r1-rollback-current-{run_id}");
    let current_content = format!("R1 rollback current run_id={run_id}");
    let verdict = send(
        &current,
        &prepare(&current_request_id, post_args(&channel, &current_content)),
    )
    .await;
    let current_id = verdict.published_event_id();
    let copies = copies_with_content(
        &relay,
        &owner,
        &channel,
        &agent,
        &current_content,
        &mut timeline,
    )
    .await;
    assert_eq!(copies.len(), 1);
    assert_eq!(copies[0].id.to_hex(), current_id);
    timeline.push(Step {
        step: "current_artifact_publishes",
        request_id: Some(current_request_id),
        generation: Some(generation_1.clone()),
        action: Some("message.post"),
        outcome: verdict.label(),
        event_ids: vec![current_id],
        relay_copies: Some(1),
        signatures_verified: Some(true),
        ..Step::default()
    });
    assert_eq!(
        broker.lifecycle("quiesce", &identity_1).await.as_deref(),
        Ok("quiescing")
    );
    assert_eq!(
        broker.drain(&identity_1).await.as_deref(),
        Ok("no pending effect")
    );
    assert_eq!(
        broker.lifecycle("fence", &identity_1).await.as_deref(),
        Ok("fenced")
    );
    let _ = serve.kill().await;
    timeline.push(Step {
        step: "current_generation_retired_and_artifact_stopped",
        generation: Some(generation_1.clone()),
        outcome: "quiescing; no pending effect; fenced; stopped".into(),
        ..Step::default()
    });

    // Rollback artifact on the same authority state.
    broker.bin = rollback_bin;
    let (mut serve, broker_url) = broker.serve().await;
    let stale = HttpBrokerClient::new(broker_url.clone(), bearer_1.clone()).unwrap();
    let stale_content = format!("R1 rollback stale run_id={run_id}");
    let verdict = send(
        &stale,
        &prepare(
            &format!("r1-rollback-stale-{run_id}"),
            post_args(&channel, &stale_content),
        ),
    )
    .await;
    assert_eq!(
        verdict.failure_code(),
        Some(BrokerErrorCode::Unauthorized),
        "fenced bearer stays denied after rollback"
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
        step: "rollback_fenced_bearer_denied",
        generation: Some(generation_1.clone()),
        action: Some("message.post"),
        outcome: verdict.label(),
        relay_copies: Some(0),
        ..Step::default()
    });
    let reissue = broker
        .issue(&identity_1)
        .await
        .expect_err("rollback cannot reissue the fenced generation");
    assert!(reissue.contains("GenerationAlreadyIssued"), "{reissue}");
    timeline.push(Step {
        step: "rollback_fenced_generation_reissue_denied",
        generation: Some(generation_1.clone()),
        outcome: "GenerationAlreadyIssued".into(),
        ..Step::default()
    });

    let generation_2 = uuid::Uuid::new_v4().to_string();
    let identity_2 = identity(&relay.ws_url, &agent, &run_id, &generation_2);
    let bearer_2 = broker.issue(&identity_2).await.expect("issue on rollback");
    let fresh = HttpBrokerClient::new(broker_url, bearer_2.clone()).unwrap();
    let fresh_request_id = format!("r1-rollback-fresh-{run_id}");
    let fresh_content = format!("R1 rollback fresh run_id={run_id}");
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
        step: "rollback_fresh_generation_publishes",
        request_id: Some(fresh_request_id),
        generation: Some(generation_2.clone()),
        action: Some("message.post"),
        outcome: verdict.label(),
        event_ids: vec![fresh_id],
        relay_copies: Some(1),
        signatures_verified: Some(true),
        ..Step::default()
    });
    assert_eq!(
        broker.lifecycle("quiesce", &identity_2).await.as_deref(),
        Ok("quiescing")
    );
    assert_eq!(
        broker.lifecycle("fence", &identity_2).await.as_deref(),
        Ok("fenced")
    );
    let _ = serve.kill().await;
    timeline.push(Step {
        step: "rollback_teardown_fenced",
        generation: Some(generation_2.clone()),
        outcome: "fenced".into(),
        ..Step::default()
    });

    let evidence = json!({
        "schema": "rooh.r1.broker-rollback-real-relay/v1",
        "source_revision": std::env::var("GITHUB_SHA").ok(),
        "relay_url": relay.ws_url,
        "channel_id": channel,
        "run_id": run_id,
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
        "bzh_",
    ] {
        assert!(!rendered.contains(secret), "evidence must stay redacted");
    }
    if let Some(path) = std::env::var_os("R1_ROLLBACK_EVIDENCE_OUT") {
        let path = Path::new(&path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("evidence directory");
        }
        std::fs::write(path, rendered.as_bytes()).expect("evidence written");
    }
}
