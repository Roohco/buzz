//! Regression tests for the R5 security review findings.

use super::*;

use std::sync::Arc;

use axum::{body::Bytes, response::IntoResponse as _, routing::post, Router};
use buzz_sdk::broker::{BrokerRequest, BrokerResult, JobAdmitArgs};
use nostr::Timestamp;

use crate::publisher::RelayPublisher;

/// 1. The peer tables are additive: the schema version the previous broker
///    release accepts is unchanged, and reopening keeps the tables.
#[tokio::test]
async fn peer_tables_keep_the_previous_release_schema_version() {
    let pilot = Pilot::new().await;
    let path = pilot._directory.path().join("authority.db");
    let pool = sqlx::SqlitePool::connect(&format!("sqlite://{}", path.display()))
        .await
        .unwrap();
    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(version, 6, "N-1 broker refuses any other schema version");
    let tables: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name LIKE 'broker_peer_%'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(tables, 3);
    pool.close().await;
    drop(pilot.store);
    AuthorityStore::open(&path).await.expect("state reopens");
}

/// 2. A stored but unpublished peer event is a pending effect: fence refuses
///    until drain-side reconciliation marks it published.
#[tokio::test]
async fn unpublished_peer_events_block_fence_until_reconciled() {
    let pilot = Pilot::new().await;
    let PeerStep::Publish { key, .. } = pilot.request(handoff("pending", SHA_A)).await else {
        panic!("request is journaled");
    };
    assert!(matches!(
        pilot.store.fence(&pilot.figma.identity).await,
        Err(StoreError::EffectsPending)
    ));
    let status = pilot.store.peer_status().await.unwrap();
    assert!(status
        .iter()
        .any(|line| line["type"] == "request" && line["published"] == false));
    pilot.store.quiesce(&pilot.figma.identity).await.unwrap();
    let (_, pending) = pilot
        .store
        .pending_peer_events(&pilot.figma.identity)
        .await
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].key, key);
    pilot.store.mark_peer_published(&key).await.unwrap();
    pilot.store.fence(&pilot.figma.identity).await.unwrap();

    // The recipient side counts too: an unpublished decision blocks its fence.
    let other = Pilot::new().await;
    let request = other.published_request(handoff("decision", SHA_A)).await;
    other.decide(&request).await;
    assert!(matches!(
        other.store.fence(&other.qa.identity).await,
        Err(StoreError::EffectsPending)
    ));
}

/// 4. A cancelled request reusing an idempotency key that already ran is
///    denied as a duplicate instead of failing the decision (poisoned queue).
#[tokio::test]
async fn cancelled_duplicate_key_is_denied_not_poisoned() {
    let pilot = Pilot::new().await;
    let mut first = handoff("poison", SHA_A);
    first.pipeline_id = "pipeline-poison".into();
    let original = pilot.published_request(first.clone()).await;
    assert_eq!(pilot.decide(&original).await.0, JobDecision::Accepted);

    pilot
        .store
        .revoke_peer_grant(GENERATION_GRANT)
        .await
        .unwrap();
    pilot.store.fence(&pilot.figma.identity).await.unwrap();
    let successor = agent_identity(
        &pilot.figma.keys,
        "11111111-1111-4111-8111-111111111111",
        OTHER_GENERATION,
    );
    let successor_credential = issue_agent(&pilot.store, &successor).await;
    let mut regrant = pilot.grant("55555555-5555-4555-8555-555555555555");
    regrant.sender = successor;
    pilot.store.issue_peer_grant(&regrant).await.unwrap();
    let PeerStep::Publish { event, key, .. } = pilot
        .store
        .prepare_job_request(
            &successor_credential,
            &pilot.figma.signer,
            &JobRequestArgs {
                channel_id: CHANNEL_ID.into(),
                recipient_pubkey: PubkeyHex::parse(pilot.qa.keys.public_key().to_hex()).unwrap(),
                handoff: JobHandoff {
                    attempt: 2,
                    ..first
                },
            },
        )
        .await
        .unwrap()
    else {
        panic!("successor request is journaled");
    };
    pilot.store.mark_peer_published(&key).await.unwrap();
    let PeerStep::Publish { key, .. } = pilot
        .store
        .prepare_job_cancel(
            &successor_credential,
            &pilot.figma.signer,
            &JobCancelArgs {
                request_event_id: event.id.to_hex(),
                reason: "withdrawn".into(),
            },
        )
        .await
        .unwrap()
    else {
        panic!("successor cancels its request");
    };
    pilot.store.mark_peer_published(&key).await.unwrap();
    assert_eq!(
        pilot.decide(&event).await,
        (
            JobDecision::Denied,
            Some("duplicate_idempotency_key".to_owned())
        )
    );
}

/// 5. A request addressed to someone else is refused with no write or signature.
#[tokio::test]
async fn request_for_another_recipient_writes_nothing() {
    let pilot = Pilot::new().await;
    let stranger = Keys::generate();
    let event = crafted(
        &pilot.figma.keys,
        CHANNEL_ID,
        &stranger,
        handoff("not-mine", SHA_A).tag_pairs(),
    );
    let refused = pilot
        .store
        .decide_job(
            &pilot.qa.credential,
            &pilot.qa.signer,
            CHANNEL_ID,
            &event,
            NOW,
        )
        .await
        .unwrap();
    assert!(refused.is_err());
    assert!(!pilot
        .store
        .peer_status()
        .await
        .unwrap()
        .iter()
        .any(|line| line["type"] == "job"));
}

/// 6. A completed job replays as `completed` with no ticket; re-checks at
///    result time turn a revoked grant into a signed job error, and a
///    quiescing recipient may still publish the result of an accepted job.
#[tokio::test]
async fn completed_replay_has_no_ticket_and_results_recheck_authority() {
    let pilot = Pilot::new().await;
    let me = pilot.qa.signer.pubkey_hex();
    let request = pilot.published_request(handoff("replay", SHA_A)).await;
    assert_eq!(pilot.decide(&request).await.0, JobDecision::Accepted);
    pilot.store.quiesce(&pilot.qa.identity).await.unwrap();
    let args = JobResultArgs {
        request_event_id: request.id.to_hex(),
        verdict: JobVerdict::Pass,
        summary: "fixture passed".into(),
        evidence_sha256: None,
    };
    let PeerStep::Publish { event, .. } = pilot
        .store
        .prepare_job_result(&pilot.qa.credential, &pilot.qa.signer, &args, NOW)
        .await
        .unwrap()
    else {
        panic!("quiescing recipient still publishes an accepted job's result");
    };
    assert_eq!(event.kind.as_u16(), 43004);
    let replay = pilot
        .store
        .recorded_decision(&me, CHANNEL_ID, Some(&request.id.to_hex()))
        .await
        .unwrap()
        .expect("recorded");
    replay.admission.validate().unwrap();
    assert_eq!(replay.admission.decision, JobDecision::Completed);
    assert!(
        replay.admission.ticket.is_none(),
        "completed never restarts work"
    );
    assert_eq!(replay.admission.decision_event_id, Some(event.id.to_hex()));

    let other = Pilot::new().await;
    let request = other.published_request(handoff("revoked", SHA_A)).await;
    assert_eq!(other.decide(&request).await.0, JobDecision::Accepted);
    other
        .store
        .revoke_peer_grant(GENERATION_GRANT)
        .await
        .unwrap();
    let PeerStep::Publish { event, .. } = other
        .store
        .prepare_job_result(
            &other.qa.credential,
            &other.qa.signer,
            &JobResultArgs {
                request_event_id: request.id.to_hex(),
                ..args
            },
            NOW,
        )
        .await
        .unwrap()
    else {
        panic!("job ends with a signed error");
    };
    assert_eq!(event.kind.as_u16(), 43006);
    assert_eq!(
        single_tag(&event.tags, tag::REASON).unwrap(),
        "grant_revoked"
    );
}

/// A fake relay with the real relay's job-request paging: `created_at DESC,
/// id ASC`, continued by `created_at < until OR (created_at = until AND id >
/// before_id)` (buzz-db `query_events`). Publication is acknowledged.
type Served = Arc<std::sync::Mutex<Vec<nostr::Event>>>;

async fn fake_relay() -> (String, tokio::task::JoinHandle<()>, Served) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let relay = format!("ws://{}", listener.local_addr().unwrap());
    let stored: Served = Arc::default();
    let served = Arc::clone(&stored);
    let app = Router::new()
        .route(
            "/query",
            post(move |body: Bytes| {
                let stored = Arc::clone(&stored);
                async move {
                    let filters: serde_json::Value = serde_json::from_slice(&body).unwrap();
                    let filter = &filters[0];
                    let limit = filter["limit"].as_u64().unwrap() as usize;
                    let mut page: Vec<_> = stored
                        .lock()
                        .unwrap()
                        .iter()
                        .filter(|event| {
                            filter.get("until").is_none_or(|until| {
                                let until = until.as_u64().unwrap();
                                let created = event.created_at.as_secs();
                                match filter["before_id"].as_str() {
                                    Some(before) => {
                                        created < until
                                            || (created == until
                                                && event.id.to_hex().as_str() > before)
                                    }
                                    None => created <= until,
                                }
                            })
                        })
                        .cloned()
                        .collect();
                    page.sort_by(|left, right| {
                        right
                            .created_at
                            .cmp(&left.created_at)
                            .then_with(|| left.id.to_hex().cmp(&right.id.to_hex()))
                    });
                    page.truncate(limit);
                    axum::Json(serde_json::json!(page)).into_response()
                }
            }),
        )
        .route(
            "/events",
            post(|body: Bytes| async move {
                let event: nostr::Event = serde_json::from_slice(&body).unwrap();
                axum::Json(serde_json::json!({
                    "accepted": true,
                    "event_id": event.id.to_hex(),
                    "message": ""
                }))
                .into_response()
            }),
        );
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (relay, server, served)
}

/// Ungranted requests from one outsider at the given creation times.
fn outsider_requests(pilot: &Pilot, outsider: &Keys, times: &[u64]) -> Vec<nostr::Event> {
    let base = handoff("starve", SHA_A);
    times
        .iter()
        .enumerate()
        .map(|(index, created_at)| {
            let mut tags = vec![
                Tag::parse(["h", CHANNEL_ID]).unwrap(),
                Tag::parse(["p", &pilot.qa.keys.public_key().to_hex()]).unwrap(),
            ];
            for (name, value) in base.tag_pairs() {
                tags.push(Tag::parse([name, value.as_str()]).unwrap());
            }
            EventBuilder::new(Kind::Custom(43001), format!("crafted {index}"))
                .tags(tags)
                .custom_created_at(Timestamp::from(*created_at))
                .sign_with_keys(outsider)
                .unwrap()
        })
        .collect()
}

/// Run one scanning `job.admit` and return the decided request id, if any.
async fn admit_scan(pilot: &Pilot) -> Option<String> {
    let publisher = RelayPublisher::new().unwrap();
    let admit = BrokerRequest::new(
        "admit-scan",
        ActionArgs::JobAdmit(JobAdmitArgs {
            channel_id: CHANNEL_ID.into(),
            request_event_id: None,
        }),
    )
    .unwrap()
    .validated()
    .unwrap();
    let response = crate::peer::handle(
        &pilot.store,
        &pilot.qa.signer,
        &publisher,
        &pilot.qa.credential,
        &admit,
    )
    .await
    .expect("job action");
    let BrokerResult::Succeeded {
        outcome: buzz_sdk::broker::ActionOutcome::JobAdmit(admission),
    } = response.result
    else {
        panic!("admit succeeds");
    };
    match admission.decision {
        JobDecision::None => None,
        JobDecision::Denied => admission.request_event_id,
        other => panic!("unexpected decision {other:?}"),
    }
}

/// Decide `events` directly and mark their denials published.
async fn decide_published(pilot: &Pilot, events: &[nostr::Event]) {
    let me = pilot.qa.signer.pubkey_hex();
    for event in events {
        pilot.decide(event).await;
        pilot
            .store
            .mark_peer_published(&PeerKey::Decision(event.id.to_hex(), me.clone()))
            .await
            .unwrap();
    }
}

/// Drain every remaining undecided request through scanning admits.
async fn admit_all(pilot: &Pilot, limit: usize) -> Vec<String> {
    let mut decided = Vec::new();
    while let Some(id) = admit_scan(pilot).await {
        decided.push(id);
        assert!(decided.len() <= limit, "admit repeats a decided request");
    }
    decided
}

/// 3. `job.admit` pages past a full page of already-decided newer requests,
///    so older undecided requests are never starved.
#[tokio::test]
async fn admit_pages_past_decided_requests_to_older_ones() {
    let times: Vec<u64> = (0..20).map(|offset| NOW - 100 + offset).collect();
    let (server, pilot, requests) = relay_with_requests(&times).await;
    // The newest 16 (one full page) are already decided and published.
    decide_published(&pilot, &requests[4..]).await;
    let mut decided = admit_all(&pilot, 4).await;
    decided.sort();
    let mut expected: Vec<String> = requests[..4].iter().map(|e| e.id.to_hex()).collect();
    expected.sort();
    assert_eq!(
        decided, expected,
        "every older, previously starved request is decided"
    );
    server.abort();
}

/// 3b. Requests sharing one second across a page boundary are all reached:
///     the cursor follows the relay's `created_at DESC, id ASC` order.
#[tokio::test]
async fn admit_reaches_same_second_requests_across_a_page_boundary() {
    // 12 newer requests, then 8 in one older second: the first page ends
    // inside that second.
    let mut times: Vec<u64> = vec![NOW - 10; 8];
    times.extend(std::iter::repeat_n(NOW - 5, 12));
    let (server, pilot, requests) = relay_with_requests(&times).await;
    let mut relay_order = requests.clone();
    relay_order.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then_with(|| left.id.to_hex().cmp(&right.id.to_hex()))
    });
    // Everything on the relay's first page is already decided.
    decide_published(&pilot, &relay_order[..16]).await;
    let mut decided = admit_all(&pilot, 4).await;
    decided.sort();
    let mut expected: Vec<String> = relay_order[16..].iter().map(|e| e.id.to_hex()).collect();
    expected.sort();
    assert_eq!(
        decided, expected,
        "same-second requests after the boundary are reached"
    );
    server.abort();
}

/// Start a fake relay, build a pilot on it and serve requests addressed to
/// its QA identity at `times`.
async fn relay_with_requests(
    times: &[u64],
) -> (tokio::task::JoinHandle<()>, Pilot, Vec<nostr::Event>) {
    let (relay, server, served) = fake_relay().await;
    let pilot = Pilot::with_relay(&relay).await;
    let outsider = Keys::generate();
    let requests = outsider_requests(&pilot, &outsider, times);
    served.lock().unwrap().extend(requests.iter().cloned());
    (server, pilot, requests)
}
