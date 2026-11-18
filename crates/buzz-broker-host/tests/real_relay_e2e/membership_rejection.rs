//! A post into a private channel the agent is not a member of: the real relay
//! rejects it, the broker resolves it as failed instead of leaving it
//! pending, and the same credential keeps publishing.
//!
//! Run: `RELAY_URL=ws://localhost:3000 cargo test -p buzz-broker-host --test
//! real_relay_e2e membership_rejection -- --ignored`.

use buzz_broker_client::HttpBrokerClient;
use buzz_sdk::broker::{BrokerErrorCode, BrokerResult};
use nostr::Keys;

use super::support::*;

#[tokio::test]
#[ignore = "requires a real buzz-relay at RELAY_URL (scripts/start-relay-for-tests.sh)"]
async fn post_outside_membership_fails_without_blocking_the_credential() {
    let relay = Relay::from_env();
    let proxy = RelayProxy::start(relay.authority()).await;
    let owner = Keys::generate();
    let agent = Keys::generate();
    let other = Keys::generate();
    let auth_tag =
        buzz_sdk::nip_oa::compute_auth_tag(&owner, &agent.public_key(), "kind=9").unwrap();
    let member_channel = uuid::Uuid::new_v4().to_string();
    let foreign_channel = uuid::Uuid::new_v4().to_string();
    create_private_channel(&relay, &owner, &agent, &member_channel).await;
    create_private_channel(&relay, &owner, &other, &foreign_channel).await;

    let broker = BrokerHost::new(&agent, &auth_tag, proxy.url());
    let run_id = uuid::Uuid::new_v4().to_string();
    let identity = identity(
        &relay.ws_url,
        &agent,
        &run_id,
        &uuid::Uuid::new_v4().to_string(),
    );
    let bearer = broker.issue(&identity).await.expect("issue generation");
    let (mut serve, broker_url) = broker.serve().await;
    let keyless = HttpBrokerClient::new(broker_url, bearer).unwrap();

    let outside = prepare(
        &format!("outside-{run_id}"),
        post_args(&foreign_channel, "not a member here"),
    );
    let rejected = send(&keyless, &outside).await;
    let BrokerResult::Failed { error } = &rejected.result else {
        panic!("non-member post must fail, got {}", rejected.label());
    };
    assert_eq!(error.code, BrokerErrorCode::ActionFailed);
    assert!(
        error.message.contains("not a channel member"),
        "relay reason is surfaced: {}",
        error.message
    );

    let replay = send(&keyless, &outside).await;
    assert!(replay.replayed, "exact retry replays the failure");
    assert_eq!(replay.failure_code(), Some(BrokerErrorCode::ActionFailed));

    let inside = send(
        &keyless,
        &prepare(
            &format!("inside-{run_id}"),
            post_args(&member_channel, "still publishing"),
        ),
    )
    .await;
    assert!(
        matches!(inside.result, BrokerResult::Succeeded { .. }),
        "later post is not blocked: {}",
        inside.label()
    );
    assert!(broker.lifecycle("quiesce", &identity).await.is_ok());
    assert!(
        broker.lifecycle("fence", &identity).await.is_ok(),
        "no unresolved effect blocks fence"
    );
    let _ = serve.kill().await;
}
