//! A relay that definitively rejects an event (and no longer holds it)
//! resolves the effect as failed instead of leaving it pending forever.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::{body::Bytes, http::StatusCode, routing::post, Router};
use buzz_sdk::broker::{MessagePostArgs, PubkeyHex};
use nostr::{Event, Keys};

use super::*;
use crate::publisher::PublishError;

const MEMBERSHIP: &str = r#"{"error":"restricted: not a channel member"}"#;

#[derive(Clone, Copy)]
enum Query {
    Absent,
    Present,
    Fail,
}

struct FakeRelay {
    url: String,
    posts: Arc<AtomicUsize>,
    reply: Arc<Mutex<(u16, String)>>,
    query: Arc<Mutex<Query>>,
    task: tokio::task::JoinHandle<()>,
}

impl FakeRelay {
    /// `/events` answers `status` with `body` (`{id}` becomes the posted
    /// event id); `/query` answers per `query`.
    async fn start(status: u16, body: &str, query: Query) -> Self {
        let posts = Arc::new(AtomicUsize::new(0));
        let reply = Arc::new(Mutex::new((status, body.to_owned())));
        let query = Arc::new(Mutex::new(query));
        let saved = Arc::new(Mutex::new(None::<Event>));
        let app = Router::new()
            .route(
                "/events",
                post({
                    let (posts, reply, saved) = (posts.clone(), reply.clone(), saved.clone());
                    move |body: Bytes| {
                        let (posts, reply, saved) = (posts.clone(), reply.clone(), saved.clone());
                        async move {
                            posts.fetch_add(1, Ordering::SeqCst);
                            let event: Event = serde_json::from_slice(&body).expect("signed event");
                            let (status, text) = reply.lock().unwrap().clone();
                            let text = text.replace("{id}", &event.id.to_hex());
                            *saved.lock().unwrap() = Some(event);
                            (
                                StatusCode::from_u16(status).unwrap(),
                                [("content-type", "application/json")],
                                text,
                            )
                        }
                    }
                }),
            )
            .route(
                "/query",
                post({
                    let (query, saved) = (query.clone(), saved.clone());
                    move || {
                        let (query, saved) = (query.clone(), saved.clone());
                        async move {
                            let found = saved.lock().unwrap().clone();
                            match *query.lock().unwrap() {
                                Query::Absent => (StatusCode::OK, axum::Json(Vec::<Event>::new())),
                                Query::Present => (
                                    StatusCode::OK,
                                    axum::Json(found.into_iter().collect::<Vec<Event>>()),
                                ),
                                Query::Fail => (
                                    StatusCode::INTERNAL_SERVER_ERROR,
                                    axum::Json(Vec::<Event>::new()),
                                ),
                            }
                        }
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("relay listener");
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            url,
            posts,
            reply,
            query,
            task,
        }
    }

    fn answer(&self, status: u16, body: &str, query: Query) {
        *self.reply.lock().unwrap() = (status, body.to_owned());
        *self.query.lock().unwrap() = query;
    }
}

/// Bind a fresh executor key and owner attestation to `fixture` on `relay_url`.
fn bind_signer(fixture: &mut Fixture, relay_url: &str) -> HostSigner {
    use std::os::unix::fs::PermissionsExt as _;

    let agent = Keys::generate();
    let owner = Keys::generate();
    fixture.identity.executor_agent_pubkey =
        PubkeyHex::parse(agent.public_key().to_hex()).expect("executor pubkey");
    fixture.identity.community_relay_url = relay_url.to_owned();
    write_identity(&fixture.authority_file, &fixture.identity);
    let key_path = fixture.directory.path().join("agent.key");
    let tag_path = fixture.directory.path().join("owner.auth-tag");
    std::fs::write(&key_path, agent.secret_key().to_secret_hex()).expect("key fixture");
    let tag = buzz_sdk::nip_oa::compute_auth_tag(&owner, &agent.public_key(), "kind=9")
        .expect("owner attestation");
    std::fs::write(&tag_path, tag).expect("tag fixture");
    for path in [&key_path, &tag_path] {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .expect("private fixture file");
    }
    HostSigner::load(&key_path, &tag_path).expect("host signer")
}

fn post_request(request_id: &str, content: &str) -> buzz_sdk::broker::PreparedRequest {
    BrokerRequest::new(
        request_id,
        ActionArgs::MessagePost(MessagePostArgs {
            channel_id: CHANNEL_ID.into(),
            content: content.into(),
            mentions: Vec::new(),
        }),
    )
    .and_then(BrokerRequest::prepare)
    .expect("post request")
}

async fn publish_with(status: u16, body: &str, query: Query) -> Result<(), PublishError> {
    let relay = FakeRelay::start(status, body, query).await;
    let mut fixture = Fixture::new();
    let signer = bind_signer(&mut fixture, &relay.url);
    let event = signer
        .sign_message(
            &fixture.identity,
            &MessagePostArgs {
                channel_id: CHANNEL_ID.into(),
                content: "outside the channel".into(),
                mentions: Vec::new(),
            },
        )
        .expect("message signed");
    let result = RelayPublisher::new()
        .expect("publisher")
        .publish(&signer, &relay.url, &event)
        .await;
    relay.task.abort();
    result
}

#[tokio::test]
async fn relay_rejection_with_event_absent_is_terminal() {
    assert!(matches!(
        publish_with(400, MEMBERSHIP, Query::Absent).await,
        Err(PublishError::Rejected(reason)) if reason == "restricted: not a channel member"
    ));
    let ack = r#"{"event_id":"{id}","accepted":false,"message":"blocked: policy"}"#;
    assert!(matches!(
        publish_with(200, ack, Query::Absent).await,
        Err(PublishError::Rejected(reason)) if reason == "blocked: policy"
    ));
    // An unbounded or non-printable relay reason is never echoed.
    let long = format!(r#"{{"error":"restricted: {}"}}"#, "x".repeat(400));
    assert!(matches!(
        publish_with(400, &long, Query::Absent).await,
        Err(PublishError::Rejected(reason)) if reason.is_empty()
    ));
    let control = r#"{"error":"restricted:\nforged line"}"#;
    assert!(matches!(
        publish_with(400, control, Query::Absent).await,
        Err(PublishError::Rejected(reason)) if reason.is_empty()
    ));
}

#[tokio::test]
async fn rejection_of_an_event_the_relay_holds_is_success() {
    let ack = r#"{"event_id":"{id}","accepted":false,"message":"duplicate: have it"}"#;
    assert!(publish_with(200, ack, Query::Present).await.is_ok());
    assert!(publish_with(400, MEMBERSHIP, Query::Present).await.is_ok());
}

#[tokio::test]
async fn unproven_rejections_stay_indeterminate() {
    let cases = [
        (503, MEMBERSHIP, Query::Absent),
        (500, r#"{"error":"internal server error"}"#, Query::Absent),
        (403, r#"{"error":"auth failed"}"#, Query::Absent),
        (400, "<html>bad request</html>", Query::Absent),
        (
            200,
            r#"{"event_id":"0000","accepted":false,"message":"other event"}"#,
            Query::Absent,
        ),
        (200, "not json", Query::Absent),
        (400, MEMBERSHIP, Query::Fail),
    ];
    for (status, body, query) in cases {
        assert!(
            matches!(
                publish_with(status, body, query).await,
                Err(PublishError::Indeterminate)
            ),
            "status {status} body {body} must stay indeterminate"
        );
    }

    // Transport failure: nothing listens at the relay address.
    let unavailable = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", unavailable.local_addr().unwrap());
    drop(unavailable);
    let mut fixture = Fixture::new();
    let signer = bind_signer(&mut fixture, &url);
    let event = signer
        .sign_message(
            &fixture.identity,
            &MessagePostArgs {
                channel_id: CHANNEL_ID.into(),
                content: "relay down".into(),
                mentions: Vec::new(),
            },
        )
        .unwrap();
    assert!(matches!(
        RelayPublisher::new()
            .unwrap()
            .publish(&signer, &url, &event)
            .await,
        Err(PublishError::Indeterminate)
    ));
}

#[tokio::test]
async fn rejected_post_fails_replays_and_no_longer_blocks_the_credential() {
    let relay = FakeRelay::start(400, MEMBERSHIP, Query::Absent).await;
    let mut fixture = Fixture::new();
    let signer = bind_signer(&mut fixture, &relay.url);
    let store = AuthorityStore::open(&fixture.state)
        .await
        .expect("state opens");
    let bearer = fixture.issue(&store).await;
    let host_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("host listener");
    let host_url = format!("http://{}", host_listener.local_addr().unwrap());
    let host_task = tokio::spawn({
        let store = store.clone();
        async move {
            axum::serve(
                host_listener,
                crate::http::app_with_effects(store, signer, RelayPublisher::new().unwrap()),
            )
            .await
            .unwrap();
        }
    });

    let rejected = post_request("not-a-member", "outside the channel");
    let (_, body) = send(&host_url, &bearer, rejected.body()).await;
    let first: BrokerResponse = serde_json::from_slice(&body).expect("broker response");
    assert!(matches!(
        first.result,
        BrokerResult::Failed { ref error }
            if error.code == BrokerErrorCode::ActionFailed
                && error.message == "relay rejected the event: restricted: not a channel member"
    ));
    assert!(!first.replayed);

    // An exact retry replays the stored failure without publishing again,
    // even once the relay would accept the event.
    relay.answer(
        200,
        r#"{"event_id":"{id}","accepted":true,"message":""}"#,
        Query::Absent,
    );
    let (_, body) = send(&host_url, &bearer, rejected.body()).await;
    let replay: BrokerResponse = serde_json::from_slice(&body).expect("replay response");
    assert!(replay.replayed);
    assert_eq!(
        serde_json::to_value(&first.result).unwrap(),
        serde_json::to_value(&replay.result).unwrap()
    );
    assert_eq!(relay.posts.load(Ordering::SeqCst), 1);

    // Changed bytes under the failed requestId still conflict.
    let changed = post_request("not-a-member", "different bytes");
    let (_, body) = send(&host_url, &bearer, changed.body()).await;
    let conflict: BrokerResponse = serde_json::from_slice(&body).unwrap();
    assert!(matches!(
        conflict.result,
        BrokerResult::Failed { ref error } if error.code == BrokerErrorCode::RequestIdConflict
    ));

    // A new effect for the same credential is admitted and published.
    let next = post_request("after-rejection", "inside the channel");
    let (_, body) = send(&host_url, &bearer, next.body()).await;
    let next: BrokerResponse = serde_json::from_slice(&body).unwrap();
    assert!(matches!(next.result, BrokerResult::Succeeded { .. }));
    assert_eq!(relay.posts.load(Ordering::SeqCst), 2);
    store
        .fence(&fixture.identity)
        .await
        .expect("no unresolved effect blocks fence");

    // The previous state shape is kept: the journal reopens cleanly.
    drop(store);
    AuthorityStore::open(&fixture.state)
        .await
        .expect("state with a failed effect reopens");
    host_task.abort();
    relay.task.abort();
}

/// A retry cannot prove absence: an earlier attempt may have stored the
/// bytes in a channel the signer can no longer read, and `/query` hides it.
#[tokio::test]
async fn retries_of_a_journaled_effect_keep_a_rejection_unresolved() {
    let relay = FakeRelay::start(400, MEMBERSHIP, Query::Absent).await;
    let mut fixture = Fixture::new();
    let signer = bind_signer(&mut fixture, &relay.url);
    let store = AuthorityStore::open(&fixture.state)
        .await
        .expect("store opens");
    let bearer = fixture.issue(&store).await;
    let credential =
        crate::credential::CredentialDigest::from_bearer(&bearer).expect("issued credential");
    let request = post_request("journaled-then-rejected", "retry me");
    let validated = serde_json::from_slice::<BrokerRequest>(request.body())
        .unwrap()
        .validated()
        .unwrap();
    let Ok(EffectAdmission::Pending(event, _, false)) = store
        .prepare_message_post(&credential, request.body(), &validated, &signer)
        .await
    else {
        panic!("effect journaled without publish");
    };
    let publisher = RelayPublisher::new().unwrap();
    assert!(matches!(
        publisher.publish_pending(&signer, &relay.url, &event).await,
        Err(PublishError::Indeterminate)
    ));

    let host_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("host listener");
    let host_url = format!("http://{}", host_listener.local_addr().unwrap());
    let host_signer = HostSigner::load(
        &fixture.directory.path().join("agent.key"),
        &fixture.directory.path().join("owner.auth-tag"),
    )
    .expect("host signer reloads");
    let host_task = tokio::spawn({
        let store = store.clone();
        async move {
            axum::serve(
                host_listener,
                crate::http::app_with_effects(store, host_signer, RelayPublisher::new().unwrap()),
            )
            .await
            .unwrap();
        }
    });
    let (_, body) = send(&host_url, &bearer, request.body()).await;
    let retry: BrokerResponse = serde_json::from_slice(&body).unwrap();
    assert!(matches!(retry.result, BrokerResult::Indeterminate { .. }));

    store.quiesce(&fixture.identity).await.expect("quiesce");
    assert!(
        crate::drain::drain_pending(&store, &fixture.identity, &signer, &publisher)
            .await
            .is_err()
    );
    assert!(matches!(
        store.fence(&fixture.identity).await,
        Err(StoreError::EffectsPending)
    ));
    host_task.abort();
    relay.task.abort();
}
