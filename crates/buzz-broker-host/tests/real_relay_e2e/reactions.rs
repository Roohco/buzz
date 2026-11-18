//! `channel.read` with `reactionsTo` against a real relay: reactions carry no
//! `h` tag, so the broker scopes them through their kind-9 targets. A target
//! in another channel refuses the read, and a NIP-09 deletion removes the
//! reaction from the relay's `/query` result.
//!
//! Run: `RELAY_URL=ws://localhost:3000 cargo test -p buzz-broker-host --test
//! real_relay_e2e reactions::channel_read_reactions_against_real_relay --
//! --ignored --exact`.

use buzz_broker_client::HttpBrokerClient;
use buzz_sdk::broker::{ActionArgs, ActionOutcome, BrokerErrorCode, BrokerResult, ChannelReadArgs};
use nostr::{EventBuilder, Keys, Kind, Tag};

use super::support::*;

fn reactions_read(channel: &str, targets: &[String]) -> ActionArgs {
    ActionArgs::ChannelRead(ChannelReadArgs {
        channel_id: channel.into(),
        reactions_to: Some(targets.to_vec()),
        ..ChannelReadArgs::default()
    })
}

async fn read_ids(client: &HttpBrokerClient, request_id: &str, args: ActionArgs) -> Vec<String> {
    let verdict = send(client, &prepare(request_id, args)).await;
    let BrokerResult::Succeeded {
        outcome: ActionOutcome::ChannelRead(page),
    } = &verdict.result
    else {
        panic!("reactions read failed: {}", verdict.label());
    };
    assert!(page.next_cursor.is_none());
    page.messages
        .iter()
        .map(|message| {
            assert!(message.verify().is_ok());
            assert_eq!(message.0.kind.as_u16(), 7);
            message.0.id.to_hex()
        })
        .collect()
}

#[tokio::test]
#[ignore = "requires a real buzz-relay at RELAY_URL (scripts/start-relay-for-tests.sh)"]
async fn channel_read_reactions_against_real_relay() {
    let relay = Relay::from_env();
    let proxy = RelayProxy::start(relay.authority()).await;
    let owner = Keys::generate();
    let agent = Keys::generate();
    let auth_tag =
        buzz_sdk::nip_oa::compute_auth_tag(&owner, &agent.public_key(), "kind=9").unwrap();
    let channel = uuid::Uuid::new_v4();
    let other_channel = uuid::Uuid::new_v4();
    create_private_channel(&relay, &owner, &agent, &channel.to_string()).await;
    create_private_channel(&relay, &owner, &agent, &other_channel.to_string()).await;

    let post = |channel_id: uuid::Uuid, content: &str| {
        buzz_sdk::build_message(channel_id, content, None, &[], false, &[])
            .unwrap()
            .sign_with_keys(&owner)
            .unwrap()
    };
    let result = post(channel, "QA result");
    let foreign = post(other_channel, "result in another channel");
    for message in [&result, &foreign] {
        let (accepted, reason) = relay.submit(&owner, message).await;
        assert!(accepted, "message accepted: {reason}");
    }
    let react = |target: nostr::EventId| {
        buzz_sdk::build_reaction(target, "✅")
            .unwrap()
            .sign_with_keys(&owner)
            .unwrap()
    };
    let verdict = react(result.id);
    let foreign_verdict = react(foreign.id);
    for reaction in [&verdict, &foreign_verdict] {
        assert!(
            !reaction
                .tags
                .iter()
                .any(|tag| tag.kind().to_string() == "h"),
            "reactions carry no h tag"
        );
        let (accepted, reason) = relay.submit(&owner, reaction).await;
        assert!(accepted, "reaction accepted: {reason}");
    }

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
    let channel = channel.to_string();

    let seen = read_ids(
        &keyless,
        &format!("reactions-{run_id}"),
        reactions_read(&channel, &[result.id.to_hex()]),
    )
    .await;
    assert_eq!(seen, vec![verdict.id.to_hex()]);

    // The agent can see the other channel too, but this read was granted
    // `channel`; a target outside it refuses the whole read.
    let refused = send(
        &keyless,
        &prepare(
            &format!("reactions-foreign-{run_id}"),
            reactions_read(&channel, &[result.id.to_hex(), foreign.id.to_hex()]),
        ),
    )
    .await;
    assert_eq!(
        refused.failure_code(),
        Some(BrokerErrorCode::InvalidRequest)
    );

    // NIP-09: the relay soft-deletes the reaction, and /query omits it.
    let deletion = EventBuilder::new(Kind::Custom(5), "")
        .tags([Tag::parse(["e", &verdict.id.to_hex()]).unwrap()])
        .sign_with_keys(&owner)
        .unwrap();
    let (accepted, reason) = relay.submit(&owner, &deletion).await;
    assert!(accepted, "reaction deletion accepted: {reason}");
    let seen = read_ids(
        &keyless,
        &format!("reactions-after-delete-{run_id}"),
        reactions_read(&channel, &[result.id.to_hex()]),
    )
    .await;
    assert!(
        seen.is_empty(),
        "deleted reaction is not returned: {seen:?}"
    );

    serve.kill().await.ok();
}
