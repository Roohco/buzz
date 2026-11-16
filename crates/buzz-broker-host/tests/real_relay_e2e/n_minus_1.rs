//! R5 rollback compatibility: authority state written by this broker (with
//! peer grants and a journaled handoff) must open on the previously published
//! broker (`BUZZ_E2E_N1_BROKER_HOST_BIN`), which keeps serving the generation
//! and can fence it; rolling forward again keeps the peer state.

use buzz_broker_client::HttpBrokerClient;
use buzz_sdk::broker::{
    ActionArgs, ActionOutcome, BrokerResult, JobHandoff, JobRequestArgs, PubkeyHex,
    CAPABILITY_QA_REVIEW_CANDIDATE,
};
use nostr::Keys;
use serde_json::json;

use super::support::*;

#[tokio::test]
#[ignore = "requires a real buzz-relay at RELAY_URL plus BUZZ_E2E_N1_BROKER_HOST_BIN"]
async fn r5_peer_state_opens_on_published_n_minus_1_broker() {
    let n1_bin = std::path::PathBuf::from(
        std::env::var_os("BUZZ_E2E_N1_BROKER_HOST_BIN").expect("published N-1 broker binary"),
    );
    let relay = Relay::from_env();
    let proxy = RelayProxy::start(relay.authority()).await;
    let owner = Keys::generate();
    let figma = Keys::generate();
    let qa = Keys::generate();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let conditions = format!("created_at<{}", now + 3600);
    let figma_tag =
        buzz_sdk::nip_oa::compute_auth_tag(&owner, &figma.public_key(), &conditions).unwrap();
    let qa_tag = buzz_sdk::nip_oa::compute_auth_tag(&owner, &qa.public_key(), &conditions).unwrap();
    let channel = uuid::Uuid::new_v4().to_string();
    let (create, _) = create_private_channel(&relay, &owner, &figma, &channel).await;
    let add = nostr::EventBuilder::new(nostr::Kind::Custom(9000), "")
        .tags([
            nostr::Tag::parse(["h", &channel]).unwrap(),
            nostr::Tag::parse(["p", &qa.public_key().to_hex()]).unwrap(),
        ])
        .sign_with_keys(&owner)
        .unwrap();
    assert!(relay.submit(&owner, &add).await.0, "owner adds QA");

    // Current broker writes peer state into the shared authority state.
    let mut figma_broker = BrokerHost::new(&figma, &figma_tag, proxy.url());
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
    qa_broker.issue(&qa_identity).await.expect("qa");
    let grant_file = figma_broker.directory.path().join("grant.json");
    std::fs::write(
        &grant_file,
        serde_json::to_vec(&json!({
            "grantId": uuid::Uuid::new_v4().to_string(),
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
    let (mut serve, url) = figma_broker.serve().await;
    let client = HttpBrokerClient::new(url, figma_bearer.clone()).unwrap();
    let request = send(
        &client,
        &prepare(
            "n1-request",
            ActionArgs::JobRequest(JobRequestArgs {
                channel_id: channel.clone(),
                recipient_pubkey: PubkeyHex::parse(qa.public_key().to_hex()).unwrap(),
                handoff: JobHandoff {
                    capability: CAPABILITY_QA_REVIEW_CANDIDATE.into(),
                    pipeline_id: "n1-pipeline".into(),
                    stage_id: "qa-review".into(),
                    parent_event_id: create.id.to_hex(),
                    candidate_sha: "0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c".into(),
                    preview_bundle_digest: format!("sha256:{}", "ab".repeat(32)),
                    budget_seconds: 300,
                    deadline: now + 1800,
                    attempt: 1,
                    idempotency_key: "n1-key".into(),
                },
            }),
        ),
    )
    .await;
    assert!(
        matches!(
            request.result,
            BrokerResult::Succeeded {
                outcome: ActionOutcome::JobRequest(_)
            }
        ),
        "current broker journals and publishes the request: {}",
        request.label()
    );
    let _ = serve.kill().await;

    // Roll back: the published N-1 broker opens the same state and serves it.
    let current_bin = std::mem::replace(&mut figma_broker.bin, n1_bin);
    let (mut serve, url) = figma_broker.serve().await;
    let client = HttpBrokerClient::new(url, figma_bearer).unwrap();
    let content = format!("N-1 serves R5 state {}", uuid::Uuid::new_v4());
    let posted = send(&client, &prepare("n1-post", post_args(&channel, &content))).await;
    let posted_id = posted.published_event_id();
    let (_, copies) = relay
        .query(
            &owner,
            json!({"ids": [posted_id], "kinds": [9], "#h": [channel], "limit": 1}),
        )
        .await;
    assert_eq!(copies.len(), 1, "N-1 broker publishes from the R5 state");
    let _ = serve.kill().await;
    assert_eq!(
        figma_broker
            .lifecycle("quiesce", &figma_identity)
            .await
            .as_deref(),
        Ok("quiescing")
    );
    assert_eq!(
        figma_broker
            .lifecycle("fence", &figma_identity)
            .await
            .as_deref(),
        Ok("fenced")
    );

    // Roll forward: the peer state survived the N-1 run.
    figma_broker.bin = current_bin;
    let status = figma_broker.run(&["peer-status".as_ref()]).await;
    let status = cli_result(&status).expect("peer status after roll-forward");
    assert!(
        status.contains("\"senderGenerationState\":\"fenced\""),
        "{status}"
    );
    assert!(status.contains("\"published\":true"), "{status}");
}
