//! Managed ACP liveness through the broker against the real relay.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use nostr::{Event, Keys};
use serde_json::json;

use super::support::*;

async fn owner_task(
    relay: &Relay,
    owner: &Keys,
    agent: &Keys,
    channel: uuid::Uuid,
    marker: &str,
) -> Event {
    let agent_hex = agent.public_key().to_hex();
    let task = buzz_sdk::build_message(
        channel,
        &format!("R1 liveness task AE-ID:{marker}"),
        None,
        &[&agent_hex],
        false,
        &[],
    )
    .unwrap()
    .sign_with_keys(owner)
    .unwrap();
    let (accepted, message) = relay.submit(owner, &task).await;
    assert!(accepted, "owner task accepted: {message}");
    task
}

/// Wait for the agent's `AE-ACK:<marker>` reply to `task` at the relay.
async fn wait_for_ack(
    relay: &Relay,
    owner: &Keys,
    agent: &Keys,
    channel: &str,
    task: &Event,
    marker: &str,
    timeline: &mut Timeline,
) -> Option<Event> {
    let expected = format!("AE-ACK:{marker}");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(45);
    while tokio::time::Instant::now() < deadline {
        let copies = copies_with_content(relay, owner, channel, agent, &expected, timeline).await;
        if let Some(reply) = copies.first() {
            assert_eq!(copies.len(), 1, "one ACP reply per task");
            assert!(has_reply_parent(reply, &task.id.to_hex()), "NIP-10 parent");
            return Some(reply.clone());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    None
}

#[tokio::test]
#[ignore = "requires a real buzz-relay at RELAY_URL plus BUZZ_E2E_ACP_BIN and BUZZ_E2E_CLI_BIN"]
async fn r1_keyless_acp_stays_live_against_real_relay() {
    let acp_bin = std::env::var("BUZZ_E2E_ACP_BIN").expect("absolute buzz-acp binary path");
    let cli_bin = std::env::var("BUZZ_E2E_CLI_BIN").expect("absolute buzz binary path");
    let idle_secs: u64 = std::env::var("R1_ACP_IDLE_SECS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(65);
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/fake-acp-post.mjs")
        .canonicalize()
        .expect("fake ACP fixture");
    let relay = Relay::from_env();
    let proxy = RelayProxy::start(relay.authority()).await;
    let mut timeline = Timeline::default();
    let owner = Keys::generate();
    let agent = Keys::generate();
    let auth_tag =
        buzz_sdk::nip_oa::compute_auth_tag(&owner, &agent.public_key(), "kind=9").unwrap();
    let run_id = uuid::Uuid::new_v4().to_string();
    let channel_uuid = uuid::Uuid::new_v4();
    let channel = channel_uuid.to_string();
    create_private_channel(&relay, &owner, &agent, &channel).await;

    let broker = BrokerHost::new(&agent, &auth_tag, proxy.url());
    let generation = uuid::Uuid::new_v4().to_string();
    let authority = identity(&relay.ws_url, &agent, &run_id, &generation);
    let bearer = broker.issue(&authority).await.expect("issue generation");
    let (mut serve, broker_url) = broker.serve().await;

    let first_marker = format!("r1-live-1-{run_id}");
    let first = owner_task(&relay, &owner, &agent, channel_uuid, &first_marker).await;
    let log_path = broker.directory.path().join("acp.log");
    let mut acp = tokio::process::Command::new(&acp_bin)
        .env("BUZZ_AGENT_MODE", "broker")
        .env("BUZZ_BROKER_URL", &broker_url)
        .env("BUZZ_BROKER_CREDENTIAL", &bearer)
        .env("BUZZ_BROKER_RELAY_URL", &relay.ws_url)
        .env(
            "BUZZ_MANAGED_ACP_AUTHORITY",
            serde_json::to_string(&authority).unwrap(),
        )
        .env("BUZZ_ACP_CHANNELS", &channel)
        .env("BUZZ_ACP_AGENT_OWNER", owner.public_key().to_hex())
        .env("BUZZ_ACP_AGENT_COMMAND", "node")
        .env("BUZZ_ACP_AGENT_ARGS", &fixture)
        .env("BUZZ_E2E_CLI_BIN", &cli_bin)
        .env("RUST_LOG", "buzz_acp=info")
        .env_remove("BUZZ_PRIVATE_KEY")
        .env_remove("BUZZ_RELAY_URL")
        .env_remove("BUZZ_AUTH_TAG")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(std::fs::File::create(&log_path).unwrap()))
        .kill_on_drop(true)
        .spawn()
        .expect("keyless buzz-acp starts");
    let redacted_log = |path: &Path| -> Vec<String> {
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .rev()
            .take(40)
            .map(|line| line.replace(&bearer, "<redacted>"))
            .collect()
    };

    let reply = wait_for_ack(
        &relay,
        &owner,
        &agent,
        &channel,
        &first,
        &first_marker,
        &mut timeline,
    )
    .await
    .unwrap_or_else(|| panic!("no first ACP reply: {:?}", redacted_log(&log_path)));
    timeline.push(Step {
        step: "acp_first_reply",
        generation: Some(generation.clone()),
        action: Some("message.send(reply)"),
        outcome: "relay_read".into(),
        event_ids: vec![first.id.to_hex(), reply.id.to_hex()],
        relay_copies: Some(1),
        signatures_verified: Some(true),
        ..Step::default()
    });

    // The earlier live run lost the poller ~30s after its reply. Stay idle
    // well past that window, then prove the poller still answers.
    tokio::time::sleep(Duration::from_secs(idle_secs)).await;
    assert!(
        acp.try_wait().unwrap().is_none(),
        "ACP exited during idle polling: {:?}",
        redacted_log(&log_path)
    );
    let second_marker = format!("r1-live-2-{run_id}");
    let second = owner_task(&relay, &owner, &agent, channel_uuid, &second_marker).await;
    let reply = wait_for_ack(
        &relay,
        &owner,
        &agent,
        &channel,
        &second,
        &second_marker,
        &mut timeline,
    )
    .await
    .unwrap_or_else(|| panic!("no reply after idle: {:?}", redacted_log(&log_path)));
    assert!(acp.try_wait().unwrap().is_none(), "ACP still live");
    timeline.push(Step {
        step: "acp_reply_after_idle",
        generation: Some(generation.clone()),
        action: Some("message.send(reply)"),
        outcome: format!("live_after_{idle_secs}s"),
        event_ids: vec![second.id.to_hex(), reply.id.to_hex()],
        relay_copies: Some(1),
        signatures_verified: Some(true),
        ..Step::default()
    });

    assert_eq!(
        broker.lifecycle("quiesce", &authority).await.as_deref(),
        Ok("quiescing")
    );
    let exit = tokio::time::timeout(Duration::from_secs(20), acp.wait())
        .await
        .expect("quiesced ACP stops")
        .expect("ACP wait");
    assert!(exit.success(), "ACP exits cleanly after quiesce");
    assert_eq!(
        broker.lifecycle("fence", &authority).await.as_deref(),
        Ok("fenced")
    );
    timeline.push(Step {
        step: "acp_quiesce_exit_and_fence",
        generation: Some(generation.clone()),
        outcome: "clean_exit; fenced".into(),
        ..Step::default()
    });
    let _ = serve.kill().await;

    let evidence = json!({
        "schema": "rooh.r1.acp-liveness-real-relay/v1",
        "source_revision": std::env::var("GITHUB_SHA").ok(),
        "relay_url": relay.ws_url,
        "channel_id": channel,
        "run_id": run_id,
        "idle_seconds": idle_secs,
        "independently_verified_event_ids": timeline.verified_event_ids,
        "steps": timeline.steps,
    });
    let rendered = serde_json::to_string_pretty(&evidence).unwrap();
    for secret in [
        bearer.as_str(),
        auth_tag.as_str(),
        &agent.secret_key().to_secret_hex(),
        &owner.secret_key().to_secret_hex(),
        "bzh_",
    ] {
        assert!(!rendered.contains(secret), "evidence must stay redacted");
    }
    if let Some(path) = std::env::var_os("R1_ACP_EVIDENCE_OUT") {
        let path = Path::new(&path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("evidence directory");
        }
        std::fs::write(path, rendered.as_bytes()).expect("evidence written");
    }
}
