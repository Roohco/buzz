//! End-to-end tests for the agent job handoff kinds (43001/43002/43004/43005/43006).
//!
//! The relay accepts these kinds only as channel events with exactly one `h`
//! tag, written by channel members, and serves them only to channel members.
//! Progress (43003) stays rejected.
//!
//! Requires a running relay; ignored by default:
//!
//! ```text
//! RELAY_URL=ws://localhost:3000 cargo test -p buzz-test-client --test e2e_job_kinds -- --ignored
//! ```

use std::time::Duration;

use buzz_test_client::{BuzzTestClient, RelayMessage};
use nostr::{Alphabet, EventBuilder, Filter, Keys, Kind, SingleLetterTag, Tag};

const HANDOFF_KINDS: [u16; 5] = [43001, 43002, 43004, 43005, 43006];
const KIND_JOB_PROGRESS: u16 = 43003;

fn relay_url() -> String {
    std::env::var("RELAY_URL").unwrap_or_else(|_| "ws://localhost:3000".to_string())
}

fn job_event(keys: &Keys, kind: u16, channels: &[&str]) -> nostr::Event {
    let mut tags: Vec<Tag> = channels
        .iter()
        .map(|channel| Tag::parse(["h", channel]).unwrap())
        .collect();
    tags.push(Tag::parse(["capability", "qa.review_candidate"]).unwrap());
    EventBuilder::new(Kind::Custom(kind), format!("job kind {kind}"))
        .tags(tags)
        .sign_with_keys(keys)
        .unwrap()
}

async fn create_private_channel(client: &mut BuzzTestClient, owner: &Keys) -> String {
    let channel = uuid::Uuid::new_v4().to_string();
    let event = EventBuilder::new(Kind::Custom(9007), "")
        .tags([
            Tag::parse(["h", &channel]).unwrap(),
            Tag::parse(["name", &format!("job-kinds-{channel}")]).unwrap(),
            Tag::parse(["channel_type", "stream"]).unwrap(),
            Tag::parse(["visibility", "private"]).unwrap(),
        ])
        .sign_with_keys(owner)
        .unwrap();
    let ok = client.send_event(event).await.expect("create channel");
    assert!(
        ok.accepted,
        "private channel creation failed: {}",
        ok.message
    );
    channel
}

async fn add_member(client: &mut BuzzTestClient, owner: &Keys, channel: &str, member: &Keys) {
    let event = EventBuilder::new(Kind::Custom(9000), "")
        .tags([
            Tag::parse(["h", channel]).unwrap(),
            Tag::parse(["p", &member.public_key().to_hex()]).unwrap(),
        ])
        .sign_with_keys(owner)
        .unwrap();
    let ok = client.send_event(event).await.expect("add member");
    assert!(ok.accepted, "member add failed: {}", ok.message);
}

/// Read job events in `channel`; a CLOSED subscription counts as no events.
async fn read_job_events(client: &mut BuzzTestClient, channel: &str) -> Vec<nostr::Event> {
    let sub = format!("job-kinds-{}", uuid::Uuid::new_v4());
    let filter = Filter::new()
        .kinds(HANDOFF_KINDS.map(Kind::Custom))
        .custom_tags(SingleLetterTag::lowercase(Alphabet::H), [channel]);
    client
        .subscribe(&sub, vec![filter])
        .await
        .expect("subscribe");
    let mut events = Vec::new();
    loop {
        match client
            .recv_event(Duration::from_secs(5))
            .await
            .expect("relay answers the subscription")
        {
            RelayMessage::Event {
                subscription_id,
                event,
            } if subscription_id == sub => events.push(*event),
            RelayMessage::Eose { subscription_id }
            | RelayMessage::Closed {
                subscription_id, ..
            } if subscription_id == sub => {
                return events;
            }
            _ => {}
        }
    }
}

#[tokio::test]
#[ignore = "requires a running relay at RELAY_URL"]
async fn job_handoff_kinds_are_member_scoped_channel_events() {
    let url = relay_url();
    let owner = Keys::generate();
    let agent = Keys::generate();
    let outsider = Keys::generate();

    let mut owner_client = BuzzTestClient::connect(&url, &owner).await.expect("owner");
    let channel = create_private_channel(&mut owner_client, &owner).await;
    add_member(&mut owner_client, &owner, &channel, &agent).await;
    let other_channel = create_private_channel(&mut owner_client, &owner).await;
    add_member(&mut owner_client, &owner, &other_channel, &agent).await;

    let mut agent_client = BuzzTestClient::connect(&url, &agent).await.expect("agent");
    let mut accepted_ids = Vec::new();
    for kind in HANDOFF_KINDS {
        let event = job_event(&agent, kind, &[&channel]);
        let id = event.id;
        let ok = agent_client
            .send_event(event)
            .await
            .expect("send job event");
        assert!(
            ok.accepted,
            "member kind {kind} must be accepted: {}",
            ok.message
        );
        accepted_ids.push(id);
    }

    let progress = job_event(&agent, KIND_JOB_PROGRESS, &[&channel]);
    let ok = agent_client
        .send_event(progress)
        .await
        .expect("send progress");
    assert!(!ok.accepted, "kind 43003 must stay rejected");

    let unscoped = job_event(&agent, 43001, &[]);
    let ok = agent_client
        .send_event(unscoped)
        .await
        .expect("send unscoped");
    assert!(
        !ok.accepted,
        "a job event without an h tag must be rejected"
    );

    let ambiguous = job_event(&agent, 43001, &[&channel, &other_channel]);
    let ok = agent_client
        .send_event(ambiguous)
        .await
        .expect("send ambiguous");
    assert!(
        !ok.accepted && ok.message.contains("exactly one h tag"),
        "a job event with two h tags must be rejected: {}",
        ok.message
    );

    let mut outsider_client = BuzzTestClient::connect(&url, &outsider)
        .await
        .expect("outsider");
    let foreign = job_event(&outsider, 43001, &[&channel]);
    let ok = outsider_client
        .send_event(foreign)
        .await
        .expect("send foreign");
    assert!(!ok.accepted, "a non-member job event must be rejected");

    let seen = read_job_events(&mut agent_client, &channel).await;
    for id in &accepted_ids {
        assert!(
            seen.iter().any(|event| event.id == *id),
            "member reads back every accepted job event"
        );
    }
    assert_eq!(
        seen.len(),
        accepted_ids.len(),
        "no rejected event was stored"
    );

    let leaked = read_job_events(&mut outsider_client, &channel).await;
    assert!(leaked.is_empty(), "a non-member must not read job events");

    for client in [owner_client, agent_client, outsider_client] {
        client.disconnect().await.expect("disconnect");
    }
}
