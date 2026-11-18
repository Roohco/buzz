use super::*;

use axum::{body::Bytes, http::HeaderMap, response::IntoResponse as _, routing::post, Router};
use base64::Engine as _;
use buzz_sdk::broker::{MessagePostArgs, PubkeyHex};
use nostr::{Event, EventBuilder, Keys, Kind, Tag};
use std::os::unix::fs::PermissionsExt as _;

#[tokio::test]
async fn channel_query_uses_host_nip98_and_returns_verified_channel_event() {
    let mut fixture = Fixture::new();
    let agent = Keys::generate();
    let owner = Keys::generate();
    let key_path = fixture.directory.path().join("agent.key");
    let tag_path = fixture.directory.path().join("owner.auth-tag");
    std::fs::write(&key_path, agent.secret_key().to_secret_hex()).expect("key fixture");
    let auth_tag = buzz_sdk::nip_oa::compute_auth_tag(&owner, &agent.public_key(), "kind=9")
        .expect("owner attestation");
    std::fs::write(&tag_path, &auth_tag).expect("tag fixture");
    for path in [&key_path, &tag_path] {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .expect("private signer file");
    }
    let signer = HostSigner::load(&key_path, &tag_path).expect("verified signer");
    let mut identity = fixture.identity.clone();
    identity.executor_agent_pubkey = PubkeyHex::parse(agent.public_key().to_hex()).unwrap();
    let signed_message = signer
        .sign_message(
            &identity,
            &MessagePostArgs {
                channel_id: CHANNEL_ID.into(),
                content: "relay message".into(),
                mentions: Vec::new(),
            },
        )
        .expect("signed channel event");
    let candidate = EventBuilder::new(Kind::Custom(9), "same-timestamp page")
        .tags([
            Tag::parse(["h", CHANNEL_ID]).unwrap(),
            buzz_sdk::nip_oa::parse_auth_tag(&auth_tag).unwrap(),
        ])
        .custom_created_at(signed_message.created_at)
        .sign_with_keys(&agent)
        .unwrap();
    assert_ne!(signed_message.id, candidate.id);
    // Cursor order is by event ID within a timestamp; sort the fixtures
    // instead of hoping one of a bounded set hashes above a random ID.
    let (message, second_message) = if signed_message.id.to_hex() < candidate.id.to_hex() {
        (signed_message, candidate)
    } else {
        (candidate, signed_message)
    };
    let expected_id = message.id;
    let second_id = second_message.id;
    let expected_tag = auth_tag.clone();
    let app = Router::new().route(
        "/query",
        post(move |headers: HeaderMap, body: Bytes| {
            let message = message.clone();
            let second_message = second_message.clone();
            let expected_tag = expected_tag.clone();
            async move {
                assert_eq!(headers.get("x-auth-tag").unwrap(), expected_tag.as_str());
                let auth = headers
                    .get("authorization")
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .strip_prefix("Nostr ")
                    .expect("NIP-98 authorization");
                let auth_bytes = base64::engine::general_purpose::STANDARD
                    .decode(auth)
                    .expect("base64 auth");
                let auth_event: Event = serde_json::from_slice(&auth_bytes).unwrap();
                assert_eq!(auth_event.pubkey, message.pubkey);
                assert_eq!(auth_event.kind.as_u16(), 27235);
                assert!(auth_event.verify().is_ok());
                let filters: serde_json::Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(filters[0]["#h"], serde_json::json!([CHANNEL_ID]));
                assert!(filters[0]["limit"] == 1 || filters[0]["limit"] == 16);
                if filters[0].get("#p").is_some() {
                    assert_eq!(
                        filters[0]["#p"],
                        serde_json::json!([message.pubkey.to_hex()])
                    );
                }
                if filters[0].get("#e").is_some() {
                    return vec![b'x'; 16 * 1024 * 1024 + 1].into_response();
                }
                if filters[0].get("before_id").is_some() {
                    assert_eq!(filters[0]["until"], message.created_at.as_secs());
                    if filters[0]["before_id"] == message.id.to_hex() {
                        axum::Json(serde_json::json!([second_message])).into_response()
                    } else {
                        assert_eq!(filters[0]["before_id"], second_message.id.to_hex());
                        axum::Json(serde_json::json!([])).into_response()
                    }
                } else {
                    axum::Json(serde_json::json!([message])).into_response()
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let relay = format!("ws://{}/community/?q=1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let publisher = RelayPublisher::new().unwrap();
    let args = ChannelReadArgs {
        channel_id: CHANNEL_ID.into(),
        limit: Some(1),
        ..ChannelReadArgs::default()
    };
    let page = publisher
        .query_channel(&signer, &relay, &args)
        .await
        .expect("signed relay query succeeds");
    let acp_page = publisher
        .query_channel(
            &signer,
            &relay,
            &ChannelReadArgs {
                limit: Some(100),
                ..args.clone()
            },
        )
        .await
        .expect("ACP poller limit is bounded by the host");
    assert_eq!(acp_page.messages.len(), 1);
    assert_eq!(page.messages.len(), 1);
    assert_eq!(page.messages[0].0.id, expected_id);
    assert!(page.next_cursor.is_some(), "full page has a continuation");
    let next_args = ChannelReadArgs {
        cursor: page.next_cursor.clone(),
        ..args.clone()
    };
    let second_page = publisher
        .query_channel(&signer, &relay, &next_args)
        .await
        .expect("opaque cursor reaches same-timestamp successor");
    assert_eq!(second_page.messages.len(), 1);
    assert_eq!(second_page.messages[0].0.id, second_id);
    let terminal = publisher
        .query_channel(
            &signer,
            &relay,
            &ChannelReadArgs {
                cursor: second_page.next_cursor,
                ..args.clone()
            },
        )
        .await
        .expect("opaque cursor reaches terminal page");
    assert!(terminal.messages.is_empty());
    assert!(terminal.next_cursor.is_none());
    let mut wrong_scope = next_args.clone();
    wrong_scope.mentions_only = true;
    assert!(publisher
        .query_channel(&signer, &relay, &wrong_scope)
        .await
        .is_err());
    let mention_args = ChannelReadArgs {
        mentions_only: true,
        ..args.clone()
    };
    assert!(
        publisher
            .query_channel(&signer, &relay, &mention_args)
            .await
            .is_err(),
        "relay cannot inject an event without the requested mention"
    );
    let oversized_args = ChannelReadArgs {
        root_event_id: Some(expected_id.to_hex()),
        ..args.clone()
    };
    assert!(matches!(
        publisher
            .query_channel(&signer, &relay, &oversized_args)
            .await,
        Err(crate::publisher::PublishError::ResponseTooLarge)
    ));
    fixture.identity.executor_agent_pubkey = PubkeyHex::parse(agent.public_key().to_hex()).unwrap();
    fixture.identity.community_relay_url = relay;
    write_identity(&fixture.authority_file, &fixture.identity);
    let store = AuthorityStore::open(&fixture.state).await.unwrap();
    let bearer = fixture.issue(&store).await;
    let status_store = store.clone();
    let wrong_store = store.clone();
    let rotated_store = store.clone();
    let operator_store = store.clone();
    let host_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let host_url = format!("http://{}", host_listener.local_addr().unwrap());
    let host = tokio::spawn(async move {
        axum::serve(
            host_listener,
            crate::http::app_with_effects(store, signer, RelayPublisher::new().unwrap()),
        )
        .await
        .unwrap();
    });
    let request = BrokerRequest::new(
        "channel-read-1",
        ActionArgs::ChannelRead(ChannelReadArgs {
            channel_id: CHANNEL_ID.into(),
            limit: Some(1),
            ..ChannelReadArgs::default()
        }),
    )
    .and_then(BrokerRequest::prepare)
    .unwrap();
    let (_, body) = send(&host_url, &bearer, request.body()).await;
    let response: BrokerResponse = serde_json::from_slice(&body).unwrap();
    assert!(matches!(
        response.result,
        BrokerResult::Succeeded {
            outcome: buzz_sdk::broker::ActionOutcome::ChannelRead(ref page)
        } if page.messages.len() == 1 && page.messages[0].0.id == expected_id
    ));
    let bad_cursor_request = BrokerRequest::new(
        "channel-read-bad-cursor",
        ActionArgs::ChannelRead(ChannelReadArgs {
            mentions_only: true,
            cursor: page.next_cursor.clone(),
            ..args.clone()
        }),
    )
    .and_then(BrokerRequest::prepare)
    .unwrap();
    let (_, bad_cursor_body) = send(&host_url, &bearer, bad_cursor_request.body()).await;
    let bad_cursor: BrokerResponse = serde_json::from_slice(&bad_cursor_body).unwrap();
    assert!(
        matches!(bad_cursor.result, BrokerResult::Failed { ref error }
        if error.code == BrokerErrorCode::InvalidRequest)
    );
    let (status_url, status_host) = spawn_host(status_store).await;
    let denied_request = BrokerRequest::new(
        "channel-read-denied-first",
        ActionArgs::ChannelRead(args.clone()),
    )
    .and_then(BrokerRequest::prepare)
    .unwrap();
    let (_, denied_body) = send(&status_url, &bearer, denied_request.body()).await;
    let denied: BrokerResponse = serde_json::from_slice(&denied_body).unwrap();
    assert!(matches!(denied.result, BrokerResult::Failed { .. }));
    let (_, denied_retry_body) = send(&host_url, &bearer, denied_request.body()).await;
    let denied_retry: BrokerResponse = serde_json::from_slice(&denied_retry_body).unwrap();
    assert!(matches!(denied_retry.result, BrokerResult::Failed { .. }));
    assert!(
        denied_retry.replayed,
        "denied exact retry replays after signer appears"
    );
    let wrong_agent = Keys::generate();
    let wrong_key = fixture.directory.path().join("wrong-agent.key");
    let wrong_tag_path = fixture.directory.path().join("wrong-agent.auth-tag");
    std::fs::write(&wrong_key, wrong_agent.secret_key().to_secret_hex()).unwrap();
    std::fs::write(
        &wrong_tag_path,
        buzz_sdk::nip_oa::compute_auth_tag(&owner, &wrong_agent.public_key(), "kind=9").unwrap(),
    )
    .unwrap();
    for path in [&wrong_key, &wrong_tag_path] {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let wrong_signer = HostSigner::load(&wrong_key, &wrong_tag_path).unwrap();
    let wrong_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let wrong_url = format!("http://{}", wrong_listener.local_addr().unwrap());
    let wrong_host = tokio::spawn(async move {
        axum::serve(
            wrong_listener,
            crate::http::app_with_effects(
                wrong_store,
                wrong_signer,
                RelayPublisher::new().unwrap(),
            ),
        )
        .await
        .unwrap();
    });
    let wrong_request = BrokerRequest::new(
        "channel-read-wrong-first",
        ActionArgs::ChannelRead(args.clone()),
    )
    .and_then(BrokerRequest::prepare)
    .unwrap();
    let (_, wrong_body) = send(&wrong_url, &bearer, wrong_request.body()).await;
    let wrong: BrokerResponse = serde_json::from_slice(&wrong_body).unwrap();
    assert!(matches!(wrong.result, BrokerResult::Failed { .. }));
    let (_, wrong_retry_body) = send(&host_url, &bearer, wrong_request.body()).await;
    let wrong_retry: BrokerResponse = serde_json::from_slice(&wrong_retry_body).unwrap();
    assert!(matches!(wrong_retry.result, BrokerResult::Failed { .. }));
    assert!(
        wrong_retry.replayed,
        "wrong-signer denial retains its receipt"
    );
    let rotated_owner = Keys::generate();
    let rotated_tag_path = fixture.directory.path().join("rotated-owner.auth-tag");
    std::fs::write(
        &rotated_tag_path,
        buzz_sdk::nip_oa::compute_auth_tag(&rotated_owner, &agent.public_key(), "kind=9").unwrap(),
    )
    .unwrap();
    std::fs::set_permissions(&rotated_tag_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let rotated_signer = HostSigner::load(&key_path, &rotated_tag_path).unwrap();
    let rotated_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let rotated_url = format!("http://{}", rotated_listener.local_addr().unwrap());
    let rotated_host = tokio::spawn(async move {
        axum::serve(
            rotated_listener,
            crate::http::app_with_effects(
                rotated_store,
                rotated_signer,
                RelayPublisher::new().unwrap(),
            ),
        )
        .await
        .unwrap();
    });
    let (_, rotated_body) = send(&rotated_url, &bearer, request.body()).await;
    let rotated: BrokerResponse = serde_json::from_slice(&rotated_body).unwrap();
    assert!(matches!(rotated.result, BrokerResult::Failed { ref error }
        if error.code == BrokerErrorCode::Unauthorized));
    assert!(
        !rotated.replayed,
        "read page cannot replay to another owner"
    );
    let rotated_fresh = BrokerRequest::new(
        "channel-read-rotated-fresh",
        ActionArgs::ChannelRead(args.clone()),
    )
    .and_then(BrokerRequest::prepare)
    .unwrap();
    let (_, rotated_fresh_body) = send(&rotated_url, &bearer, rotated_fresh.body()).await;
    let rotated_fresh_response: BrokerResponse =
        serde_json::from_slice(&rotated_fresh_body).unwrap();
    assert!(
        matches!(rotated_fresh_response.result, BrokerResult::Failed { ref error }
        if error.code == BrokerErrorCode::Unauthorized)
    );
    server.abort();
    let (_, retry_body) = send(&host_url, &bearer, request.body()).await;
    let retry: BrokerResponse = serde_json::from_slice(&retry_body).unwrap();
    assert!(matches!(retry.result, BrokerResult::Succeeded { .. }));
    assert!(
        retry.replayed,
        "exact retry uses the stored page during relay outage"
    );
    let changed = BrokerRequest::new(
        "channel-read-1",
        ActionArgs::ChannelRead(ChannelReadArgs {
            mentions_only: true,
            ..args
        }),
    )
    .and_then(BrokerRequest::prepare)
    .unwrap();
    let (_, changed_body) = send(&host_url, &bearer, changed.body()).await;
    let changed_response: BrokerResponse = serde_json::from_slice(&changed_body).unwrap();
    assert!(
        matches!(changed_response.result, BrokerResult::Failed { ref error }
        if error.code == BrokerErrorCode::RequestIdConflict)
    );
    operator_store.quiesce(&fixture.identity).await.unwrap();
    let (_, quiesced_body) = send(&host_url, &bearer, request.body()).await;
    let quiesced: BrokerResponse = serde_json::from_slice(&quiesced_body).unwrap();
    assert!(matches!(quiesced.result, BrokerResult::Failed { ref error }
        if error.code == BrokerErrorCode::Unauthorized));
    host.abort();
    status_host.abort();
    wrong_host.abort();
    rotated_host.abort();
}

const OTHER_CHANNEL_ID: &str = "0b7f1c52-3a8e-4d55-9f0e-6f2d8a1c4b90";

fn reaction_at(keys: &Keys, e_tags: &[&str], emoji: &str, created_at: u64) -> Event {
    EventBuilder::new(Kind::Custom(7), emoji)
        .tags(e_tags.iter().map(|id| Tag::parse(["e", id]).unwrap()))
        .custom_created_at(nostr::Timestamp::from(created_at))
        .sign_with_keys(keys)
        .unwrap()
}

/// The relay's page order and cursor: `created_at DESC, id ASC`, continued
/// below `(until, before_id)`. Rows are served whatever their `#e`, so the
/// host's own checks are what is under test.
fn relay_page(rows: &[serde_json::Value], filter: &serde_json::Value) -> Vec<serde_json::Value> {
    let key = |row: &serde_json::Value| {
        (
            row["created_at"].as_u64().unwrap(),
            row["id"].as_str().unwrap().to_owned(),
        )
    };
    let mut rows: Vec<_> = rows
        .iter()
        .filter(
            |row| match (filter["until"].as_u64(), filter["before_id"].as_str()) {
                (Some(until), Some(before_id)) => {
                    let (created_at, id) = key(row);
                    created_at < until || (created_at == until && id.as_str() > before_id)
                }
                _ => true,
            },
        )
        .cloned()
        .collect();
    rows.sort_by(|left, right| {
        let (left_at, left_id) = key(left);
        let (right_at, right_id) = key(right);
        right_at.cmp(&left_at).then_with(|| left_id.cmp(&right_id))
    });
    rows.truncate(filter["limit"].as_u64().unwrap() as usize);
    rows
}

#[tokio::test]
async fn reactions_read_requires_channel_targets_and_verified_last_e_tag() {
    let fixture = Fixture::new();
    let agent = Keys::generate();
    let owner = Keys::generate();
    let reviewer = Keys::generate();
    let key_path = fixture.directory.path().join("agent.key");
    let tag_path = fixture.directory.path().join("owner.auth-tag");
    std::fs::write(&key_path, agent.secret_key().to_secret_hex()).unwrap();
    let auth_tag =
        buzz_sdk::nip_oa::compute_auth_tag(&owner, &agent.public_key(), "kind=9").unwrap();
    std::fs::write(&tag_path, &auth_tag).unwrap();
    for path in [&key_path, &tag_path] {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let signer = HostSigner::load(&key_path, &tag_path).unwrap();

    let message = |channel: &str, content: &str| {
        EventBuilder::new(Kind::Custom(9), content)
            .tags([Tag::parse(["h", channel]).unwrap()])
            .sign_with_keys(&agent)
            .unwrap()
    };
    let result = message(CHANNEL_ID, "QA result");
    let second = message(CHANNEL_ID, "second QA result");
    let foreign = message(OTHER_CHANNEL_ID, "foreign result");
    let elsewhere = message(CHANNEL_ID, "a message nobody asked about");
    let (result_hex, second_hex, elsewhere_hex) = (
        result.id.to_hex(),
        second.id.to_hex(),
        elsewhere.id.to_hex(),
    );
    let t0 = 1_700_000_000;

    let verdict = reaction_at(&reviewer, &[&result_hex], "✅", t0 + 2);
    // NIP-25: the last `e` tag is the reacted-to event, earlier ones are context.
    let with_context = reaction_at(&reviewer, &[&elsewhere_hex, &second_hex], "🔁", t0 + 1);
    let to_other_event = reaction_at(&reviewer, &[&result_hex, &elsewhere_hex], "❌", t0 + 3);
    // The relay's rule skips an `e` tag that is not 64 hex, so this reacts to
    // the result.
    let junk_tail = reaction_at(&reviewer, &[&result_hex, "junk"], "✅", t0);
    let mut forged =
        serde_json::to_value(reaction_at(&reviewer, &[&result_hex], "✅", t0)).unwrap();
    forged["content"] = serde_json::json!("❌");

    let channel_events = vec![result.clone(), second.clone(), foreign.clone()];
    let rows = std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
    let set_rows = |events: Vec<serde_json::Value>| *rows.lock().unwrap() = events;
    let json = |event: &Event| serde_json::to_value(event).unwrap();
    let served = rows.clone();
    let reaction_queries = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = reaction_queries.clone();
    let app = Router::new().route(
        "/query",
        post(move |body: Bytes| {
            let channel_events = channel_events.clone();
            let served = served.clone();
            let counted = counted.clone();
            async move {
                let filters: serde_json::Value = serde_json::from_slice(&body).unwrap();
                let filter = &filters[0];
                if filter["kinds"] == serde_json::json!([9]) {
                    assert_eq!(filter["#h"], serde_json::json!([CHANNEL_ID]));
                    // Ignore `#h` on purpose: the host must not trust the relay
                    // to have scoped the targets to the channel.
                    let ids = filter["ids"].as_array().unwrap();
                    let found: Vec<_> = channel_events
                        .iter()
                        .filter(|event| ids.contains(&serde_json::json!(event.id.to_hex())))
                        .collect();
                    return axum::Json(serde_json::json!(found)).into_response();
                }
                assert_eq!(filter["kinds"], serde_json::json!([7]));
                assert!(filter.get("#h").is_none(), "reactions carry no h tag");
                assert!(filter["#e"].is_array());
                assert_eq!(filter["limit"], 16);
                counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                axum::Json(relay_page(&served.lock().unwrap(), filter)).into_response()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let relay = format!("ws://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let publisher = RelayPublisher::new().unwrap();
    let read = |targets: Vec<nostr::EventId>| ChannelReadArgs {
        channel_id: CHANNEL_ID.into(),
        reactions_to: Some(targets.iter().map(|id| id.to_hex()).collect()),
        ..ChannelReadArgs::default()
    };
    let ids = |page: &buzz_sdk::broker::MessagePage| -> Vec<nostr::EventId> {
        page.messages.iter().map(|m| m.0.id).collect()
    };

    // Kept reactions come back oldest first; the reaction to another event
    // is dropped.
    set_rows(vec![
        json(&verdict),
        json(&with_context),
        json(&to_other_event),
        json(&junk_tail),
    ]);
    let page = publisher
        .query_channel(&signer, &relay, &read(vec![result.id, second.id]))
        .await
        .expect("reactions to channel messages");
    assert_eq!(ids(&page), vec![junk_tail.id, with_context.id, verdict.id]);
    assert!(page.next_cursor.is_none());

    for (what, targets) in [
        ("unknown target", vec![result.id, elsewhere.id]),
        ("foreign-channel target", vec![result.id, foreign.id]),
    ] {
        assert!(
            matches!(
                publisher
                    .query_channel(&signer, &relay, &read(targets))
                    .await,
                Err(crate::publisher::PublishError::TargetOutsideChannel)
            ),
            "{what} refuses the whole read"
        );
    }

    // A full relay page of reactions to another event does not push the
    // verdict out: the host pages on to the older relay page.
    let decoys: Vec<_> = (0..16)
        .map(|n| {
            json(&reaction_at(
                &reviewer,
                &[&result_hex, &elsewhere_hex],
                "❌",
                t0 + 10 + n,
            ))
        })
        .collect();
    let mut with_verdict = decoys.clone();
    with_verdict.push(json(&verdict));
    set_rows(with_verdict);
    reaction_queries.store(0, std::sync::atomic::Ordering::SeqCst);
    let page = publisher
        .query_channel(&signer, &relay, &read(vec![result.id]))
        .await
        .expect("paged reactions");
    assert_eq!(ids(&page), vec![verdict.id]);
    assert_eq!(
        reaction_queries.load(std::sync::atomic::Ordering::SeqCst),
        3,
        "two non-empty relay pages, then an empty one proves the end"
    );

    // The page is held to the caller's limit, keeping the newest.
    let kept: Vec<_> = (0..3)
        .map(|n| reaction_at(&reviewer, &[&result_hex], "✅", t0 + 40 + n))
        .collect();
    set_rows(kept.iter().map(json).collect());
    let page = publisher
        .query_channel(
            &signer,
            &relay,
            &ChannelReadArgs {
                limit: Some(2),
                ..read(vec![result.id])
            },
        )
        .await
        .unwrap();
    assert_eq!(ids(&page), vec![kept[1].id, kept[2].id]);

    // Past the relay page cap with pages still coming, the read fails rather
    // than return a page that may be missing reactions.
    let flood: Vec<_> = (0..64)
        .map(|n| {
            json(&reaction_at(
                &reviewer,
                &[&result_hex, &elsewhere_hex],
                "❌",
                t0 + 100 + n,
            ))
        })
        .collect();
    let mut flooded = flood.clone();
    flooded.push(json(&verdict));
    set_rows(flooded);
    assert!(matches!(
        publisher
            .query_channel(&signer, &relay, &read(vec![result.id]))
            .await,
        Err(crate::publisher::PublishError::TooManyReactions)
    ));

    set_rows(vec![json(&verdict), forged]);
    assert!(matches!(
        publisher
            .query_channel(&signer, &relay, &read(vec![result.id]))
            .await,
        Err(crate::publisher::PublishError::InvalidEvent)
    ));
    set_rows(vec![json(&reaction_at(
        &reviewer,
        &[&elsewhere_hex],
        "✅",
        t0,
    ))]);
    assert!(
        matches!(
            publisher
                .query_channel(&signer, &relay, &read(vec![result.id]))
                .await,
            Err(crate::publisher::PublishError::InvalidEvent)
        ),
        "a reaction naming no target is a relay fault"
    );
    set_rows(vec![json(&elsewhere)]);
    assert!(
        matches!(
            publisher
                .query_channel(&signer, &relay, &read(vec![result.id]))
                .await,
            Err(crate::publisher::PublishError::InvalidEvent)
        ),
        "only kind 7 is returned"
    );
    server.abort();
}
