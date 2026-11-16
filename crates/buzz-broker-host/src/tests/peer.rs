//! R5 scoped peer handoff: grant store and recipient admission matrix.

use super::*;

use buzz_sdk::broker::actions::job::{single_tag, tag};
use buzz_sdk::broker::{
    JobCancelArgs, JobDecision, JobHandoff, JobRequestArgs, JobResultArgs, JobVerdict, PubkeyHex,
    CAPABILITY_QA_REVIEW_CANDIDATE,
};
use nostr::{EventBuilder, Keys, Kind, Tag};
use std::os::unix::fs::PermissionsExt as _;

use crate::credential::{self, CredentialDigest};
use crate::store::peer::{PeerColumn, PeerGrantFile, PeerStep};

const NOW: u64 = 1_800_000_000;
const OTHER_CHANNEL: &str = "6a1d1b36-6b54-4d55-9c1e-4a6f0f1b2c3d";
const SHA_A: &str = "0123456789abcdef0123456789abcdef01234567";
const SHA_B: &str = "89abcdef0123456789abcdef0123456789abcdef";

struct Agent {
    keys: Keys,
    signer: HostSigner,
    identity: AuthorityIdentity,
    credential: CredentialDigest,
}

struct Pilot {
    _directory: TempDir,
    store: AuthorityStore,
    figma: Agent,
    qa: Agent,
}

fn agent_identity(keys: &Keys, task_id: &str, generation: &str) -> AuthorityIdentity {
    serde_json::from_value(serde_json::json!({
        "communityRelayUrl": "wss://relay.example",
        "logicalAgentPubkey": keys.public_key().to_hex(),
        "executorAgentPubkey": keys.public_key().to_hex(),
        "taskId": task_id,
        "generation": generation,
        "location": {"kind": "local", "id": "r5-unit"},
        "runtimeSupport": "portable"
    }))
    .expect("agent identity")
}

fn signer_for(directory: &Path, name: &str, keys: &Keys, owner: &Keys) -> HostSigner {
    let key_path = directory.join(format!("{name}.key"));
    let tag_path = directory.join(format!("{name}.auth-tag"));
    std::fs::write(&key_path, keys.secret_key().to_secret_hex()).expect("key file");
    let tag = buzz_sdk::nip_oa::compute_auth_tag(owner, &keys.public_key(), "").expect("tag");
    std::fs::write(&tag_path, tag).expect("tag file");
    for path in [&key_path, &tag_path] {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).expect("mode");
    }
    HostSigner::load(&key_path, &tag_path).expect("signer")
}

async fn issue_agent(store: &AuthorityStore, identity: &AuthorityIdentity) -> CredentialDigest {
    let bearer = credential::generate();
    let digest = CredentialDigest::from_bearer(&bearer).expect("digest");
    store.issue(&digest, identity).await.expect("issued");
    digest
}

impl Pilot {
    async fn new() -> Self {
        let directory = tempfile::tempdir().expect("tempdir");
        make_private_directory(directory.path());
        let path = directory.path().to_path_buf();
        let store = AuthorityStore::open(&path.join("authority.db"))
            .await
            .expect("store");
        let owner = Keys::generate();
        let mut agents = Vec::new();
        for (name, task) in [
            ("figma", "11111111-1111-4111-8111-111111111111"),
            ("qa", "33333333-3333-4333-8333-333333333333"),
        ] {
            let keys = Keys::generate();
            let identity = agent_identity(&keys, task, GENERATION);
            let credential = issue_agent(&store, &identity).await;
            agents.push(Agent {
                signer: signer_for(&path, name, &keys, &owner),
                keys,
                identity,
                credential,
            });
        }
        let qa = agents.pop().expect("qa");
        let figma = agents.pop().expect("figma");
        let pilot = Self {
            _directory: directory,
            store,
            figma,
            qa,
        };
        pilot
            .store
            .issue_peer_grant(&pilot.grant(GENERATION_GRANT))
            .await
            .expect("grant issued");
        pilot
    }

    fn grant(&self, grant_id: &str) -> PeerGrantFile {
        serde_json::from_value(serde_json::json!({
            "grantId": grant_id,
            "sender": self.figma.identity,
            "recipientPubkey": self.qa.keys.public_key().to_hex(),
            "channelId": CHANNEL_ID,
            "capability": CAPABILITY_QA_REVIEW_CANDIDATE,
        }))
        .expect("grant file")
    }

    async fn request(&self, handoff: JobHandoff) -> PeerStep {
        self.store
            .prepare_job_request(
                &self.figma.credential,
                &self.figma.signer,
                &JobRequestArgs {
                    channel_id: CHANNEL_ID.into(),
                    recipient_pubkey: PubkeyHex::parse(self.qa.keys.public_key().to_hex()).unwrap(),
                    handoff,
                },
            )
            .await
            .expect("request step")
    }

    async fn published_request(&self, handoff: JobHandoff) -> nostr::Event {
        let PeerStep::Publish { event, .. } = self.request(handoff).await else {
            panic!("granted request must be signed for publication");
        };
        self.store
            .mark_peer_published(PeerColumn::Request, &event.id.to_hex())
            .await
            .expect("mark published");
        event
    }

    async fn decide(&self, event: &nostr::Event) -> (JobDecision, Option<String>) {
        self.decide_in(CHANNEL_ID, event).await
    }

    async fn decide_in(
        &self,
        channel: &str,
        event: &nostr::Event,
    ) -> (JobDecision, Option<String>) {
        let step = self
            .store
            .decide_job(&self.qa.credential, &self.qa.signer, channel, event, NOW)
            .await
            .expect("decision")
            .expect("recipient may admit");
        step.admission.validate().expect("admission shape");
        (step.admission.decision, step.admission.reason)
    }
}

const GENERATION_GRANT: &str = "44444444-4444-4444-8444-444444444444";

fn handoff(key: &str, sha: &str) -> JobHandoff {
    JobHandoff {
        capability: CAPABILITY_QA_REVIEW_CANDIDATE.into(),
        pipeline_id: "pipeline-1".into(),
        stage_id: "qa".into(),
        parent_event_id: "a".repeat(64),
        candidate_sha: sha.into(),
        preview_bundle_digest: format!("sha256:{}", "b".repeat(64)),
        budget_seconds: 600,
        deadline: NOW + 3600,
        attempt: 1,
        idempotency_key: key.into(),
    }
}

/// A request event signed directly with `keys`, bypassing any broker.
fn crafted(
    keys: &Keys,
    channel: &str,
    recipient: &Keys,
    tags: Vec<(&str, String)>,
) -> nostr::Event {
    let mut all = vec![
        Tag::parse(["h", channel]).unwrap(),
        Tag::parse(["p", &recipient.public_key().to_hex()]).unwrap(),
    ];
    for (name, value) in tags {
        all.push(Tag::parse([name, value.as_str()]).unwrap());
    }
    EventBuilder::new(Kind::Custom(43001), "crafted")
        .tags(all)
        .sign_with_keys(keys)
        .unwrap()
}

fn with(handoff: &JobHandoff, name: &str, value: Option<&str>) -> Vec<(&'static str, String)> {
    handoff
        .tag_pairs()
        .into_iter()
        .filter_map(|(tag_name, current)| {
            if tag_name == name {
                value.map(|value| (tag_name, value.to_owned()))
            } else {
                Some((tag_name, current))
            }
        })
        .collect()
}

#[tokio::test]
async fn granted_request_is_accepted_once_and_result_links_the_same_identity() {
    let pilot = Pilot::new().await;
    let request = pilot.published_request(handoff("key-1", SHA_A)).await;
    assert_eq!(request.kind.as_u16(), 43001);
    assert_eq!(request.pubkey, pilot.figma.keys.public_key());

    let step = pilot
        .store
        .decide_job(
            &pilot.qa.credential,
            &pilot.qa.signer,
            CHANNEL_ID,
            &request,
            NOW,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(step.admission.decision, JobDecision::Accepted);
    let (accepted, retry) = step.publish.expect("acceptance is published");
    assert!(!retry);
    assert_eq!(accepted.kind.as_u16(), 43002);
    assert_eq!(
        single_tag(&accepted.tags, tag::REQUEST).unwrap(),
        request.id.to_hex()
    );
    assert_eq!(
        single_tag(&accepted.tags, tag::CANDIDATE_SHA).unwrap(),
        SHA_A
    );
    let ticket = step.admission.ticket.expect("ticket");
    assert_eq!(
        ticket.sender_pubkey.as_str(),
        pilot.figma.keys.public_key().to_hex()
    );

    // Lost acknowledgement: the unpublished decision is replayed, not re-signed.
    let replay = pilot
        .store
        .recorded_decision(&pilot.qa.credential, CHANNEL_ID, None)
        .await
        .unwrap()
        .expect("pending decision is reconcilable");
    assert_eq!(
        replay
            .publish
            .as_ref()
            .map(|(event, retry)| (event.id, *retry)),
        Some((accepted.id, true))
    );
    pilot
        .store
        .mark_peer_published(PeerColumn::Decision, &request.id.to_hex())
        .await
        .unwrap();

    // Duplicate delivery of the same request replays the one decision.
    let duplicate = pilot
        .store
        .decide_job(
            &pilot.qa.credential,
            &pilot.qa.signer,
            CHANNEL_ID,
            &request,
            NOW,
        )
        .await
        .unwrap()
        .unwrap();
    assert!(duplicate.replayed && duplicate.publish.is_none());
    assert_eq!(
        duplicate.admission.decision_event_id,
        Some(accepted.id.to_hex())
    );

    // The sender retrying the same idempotency key gets the same event back.
    match pilot.request(handoff("key-1", SHA_A)).await {
        PeerStep::Published(event) => assert_eq!(event.id, request.id),
        _ => panic!("same idempotency key must replay the journaled request"),
    }

    let result_args = JobResultArgs {
        request_event_id: request.id.to_hex(),
        verdict: JobVerdict::Pass,
        summary: "fixture review passed".into(),
        evidence_sha256: Some("c".repeat(64)),
    };
    let PeerStep::Publish { event: result, .. } = pilot
        .store
        .prepare_job_result(&pilot.qa.credential, &pilot.qa.signer, &result_args)
        .await
        .unwrap()
    else {
        panic!("accepted job yields a result");
    };
    assert_eq!(result.kind.as_u16(), 43004);
    assert_eq!(
        single_tag(&result.tags, tag::REQUEST).unwrap(),
        request.id.to_hex()
    );
    assert_eq!(
        single_tag(&result.tags, tag::ACCEPTED).unwrap(),
        accepted.id.to_hex()
    );
    assert_eq!(single_tag(&result.tags, tag::CANDIDATE_SHA).unwrap(), SHA_A);
    assert_eq!(
        single_tag(&result.tags, tag::PREVIEW_BUNDLE_DIGEST).unwrap(),
        format!("sha256:{}", "b".repeat(64))
    );
    assert_eq!(
        single_tag(&result.tags, "p").unwrap(),
        pilot.figma.keys.public_key().to_hex()
    );
    pilot
        .store
        .mark_peer_published(PeerColumn::Result, &request.id.to_hex())
        .await
        .unwrap();
    match pilot
        .store
        .prepare_job_result(&pilot.qa.credential, &pilot.qa.signer, &result_args)
        .await
        .unwrap()
    {
        PeerStep::Published(event) => assert_eq!(event.id, result.id),
        _ => panic!("the same result replays"),
    }
    let mut different = result_args.clone();
    different.verdict = JobVerdict::Defect;
    assert!(matches!(
        pilot
            .store
            .prepare_job_result(&pilot.qa.credential, &pilot.qa.signer, &different)
            .await
            .unwrap(),
        PeerStep::Refuse(_)
    ));
    // Completed is terminal: the sender cannot cancel it any more.
    assert!(matches!(
        pilot
            .store
            .prepare_job_cancel(
                &pilot.figma.credential,
                &pilot.figma.signer,
                &JobCancelArgs {
                    request_event_id: request.id.to_hex(),
                    reason: "too late".into()
                }
            )
            .await
            .unwrap(),
        PeerStep::Refuse(_)
    ));
    let status = pilot.store.peer_status().await.unwrap();
    assert!(status
        .iter()
        .any(|line| line["type"] == "grant" && line["state"] == "active"));
    assert!(status
        .iter()
        .any(|line| line["type"] == "job" && line["state"] == "completed"));
}

#[tokio::test]
async fn admission_matrix_denies_before_any_ticket() {
    let pilot = Pilot::new().await;
    let base = handoff("matrix", SHA_A);
    let outsider = Keys::generate();
    let cases: Vec<(&str, nostr::Event, &str)> = vec![
        (
            "ungranted peer",
            crafted(&outsider, CHANNEL_ID, &pilot.qa.keys, base.tag_pairs()),
            "ungranted_peer",
        ),
        (
            "wrong project channel",
            crafted(
                &pilot.figma.keys,
                OTHER_CHANNEL,
                &pilot.qa.keys,
                base.tag_pairs(),
            ),
            "channel_not_granted",
        ),
        (
            "wrong capability",
            crafted(
                &pilot.figma.keys,
                CHANNEL_ID,
                &pilot.qa.keys,
                with(&base, tag::CAPABILITY, Some("qa.release")),
            ),
            "capability_not_granted",
        ),
        (
            "missing candidate sha",
            crafted(
                &pilot.figma.keys,
                CHANNEL_ID,
                &pilot.qa.keys,
                with(&base, tag::CANDIDATE_SHA, None),
            ),
            "invalid_candidate_identity",
        ),
        (
            "malformed bundle digest",
            crafted(
                &pilot.figma.keys,
                CHANNEL_ID,
                &pilot.qa.keys,
                with(&base, tag::PREVIEW_BUNDLE_DIGEST, Some("md5:abc")),
            ),
            "invalid_candidate_identity",
        ),
        (
            "granted key but not issued through the broker",
            crafted(
                &pilot.figma.keys,
                CHANNEL_ID,
                &pilot.qa.keys,
                base.tag_pairs(),
            ),
            "request_not_issued_under_grant",
        ),
    ];
    for (name, event, reason) in cases {
        let channel = single_tag(&event.tags, "h").unwrap();
        let (decision, denied) = pilot.decide_in(&channel, &event).await;
        assert_eq!(decision, JobDecision::Denied, "{name}");
        assert_eq!(denied.as_deref(), Some(reason), "{name}");
    }

    let mut expired = handoff("expired", SHA_A);
    expired.deadline = NOW - 1;
    let event = pilot.published_request(expired).await;
    assert_eq!(
        pilot.decide(&event).await.1.as_deref(),
        Some("deadline_expired")
    );

    let mut short = handoff("short", SHA_A);
    short.pipeline_id = "pipeline-short".into();
    short.deadline = NOW + 10;
    let event = pilot.published_request(short).await;
    assert_eq!(
        pilot.decide(&event).await.1.as_deref(),
        Some("budget_exceeds_deadline")
    );

    // A newer candidate in the same pipeline stage makes the older request stale.
    let mut old = handoff("stale-old", SHA_A);
    old.pipeline_id = "pipeline-stale".into();
    let stale = pilot.published_request(old).await;
    let mut new = handoff("stale-new", SHA_B);
    new.pipeline_id = "pipeline-stale".into();
    let fresh = pilot.published_request(new).await;
    assert_eq!(
        pilot.decide(&stale).await.1.as_deref(),
        Some("stale_candidate_identity")
    );
    assert_eq!(pilot.decide(&fresh).await.0, JobDecision::Accepted);

    // The same idempotency key under a successor generation runs at most once.
    let mut first = handoff("dup", SHA_A);
    first.pipeline_id = "pipeline-dup".into();
    let original = pilot.published_request(first.clone()).await;
    assert_eq!(pilot.decide(&original).await.0, JobDecision::Accepted);
    pilot.store.fence(&pilot.figma.identity).await.unwrap();
    let successor = agent_identity(
        &pilot.figma.keys,
        "11111111-1111-4111-8111-111111111111",
        OTHER_GENERATION,
    );
    let successor_credential = issue_agent(&pilot.store, &successor).await;
    let mut regrant = pilot.grant("55555555-5555-4555-8555-555555555555");
    regrant.sender = successor;
    pilot
        .store
        .revoke_peer_grant(GENERATION_GRANT)
        .await
        .unwrap();
    pilot.store.issue_peer_grant(&regrant).await.unwrap();
    let PeerStep::Publish { event: replay, .. } = pilot
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
        panic!("successor generation signs under its own grant");
    };
    assert_ne!(replay.id, original.id);
    assert_eq!(
        pilot.decide(&replay).await.1.as_deref(),
        Some("duplicate_idempotency_key")
    );
}

#[tokio::test]
async fn fenced_sender_generation_and_revoked_grant_deny_new_work() {
    let pilot = Pilot::new().await;
    let pending = pilot.published_request(handoff("fence", SHA_A)).await;
    pilot.store.fence(&pilot.figma.identity).await.unwrap();
    assert_eq!(
        pilot.decide(&pending).await.1.as_deref(),
        Some("sender_generation_fenced")
    );
    assert!(matches!(
        pilot.request(handoff("after-fence", SHA_A)).await,
        PeerStep::Refuse(_)
    ));

    let other = Pilot::new().await;
    let pending = other.published_request(handoff("revoke", SHA_A)).await;
    other
        .store
        .revoke_peer_grant(GENERATION_GRANT)
        .await
        .unwrap();
    assert_eq!(
        other.decide(&pending).await.1.as_deref(),
        Some("grant_revoked")
    );
    assert!(matches!(
        other.request(handoff("after-revoke", SHA_A)).await,
        PeerStep::Refuse(_)
    ));
}

#[tokio::test]
async fn cancel_is_terminal_before_and_after_acceptance() {
    let pilot = Pilot::new().await;
    let cancel = |request: &nostr::Event| JobCancelArgs {
        request_event_id: request.id.to_hex(),
        reason: "superseded by owner".into(),
    };

    let before = pilot
        .published_request(handoff("cancel-before", SHA_A))
        .await;
    let PeerStep::Publish { event, .. } = pilot
        .store
        .prepare_job_cancel(
            &pilot.figma.credential,
            &pilot.figma.signer,
            &cancel(&before),
        )
        .await
        .unwrap()
    else {
        panic!("sender cancels its own request");
    };
    assert_eq!(event.kind.as_u16(), 43005);
    assert_eq!(
        single_tag(&event.tags, tag::REQUEST).unwrap(),
        before.id.to_hex()
    );
    assert_eq!(pilot.decide(&before).await.0, JobDecision::Cancelled);

    let mut later = handoff("cancel-after", SHA_A);
    later.pipeline_id = "pipeline-cancel".into();
    let after = pilot.published_request(later).await;
    assert_eq!(pilot.decide(&after).await.0, JobDecision::Accepted);
    assert!(matches!(
        pilot
            .store
            .prepare_job_cancel(
                &pilot.figma.credential,
                &pilot.figma.signer,
                &cancel(&after)
            )
            .await
            .unwrap(),
        PeerStep::Publish { .. }
    ));
    assert!(matches!(
        pilot
            .store
            .prepare_job_result(
                &pilot.qa.credential,
                &pilot.qa.signer,
                &JobResultArgs {
                    request_event_id: after.id.to_hex(),
                    verdict: JobVerdict::Pass,
                    summary: "late".into(),
                    evidence_sha256: None,
                },
            )
            .await
            .unwrap(),
        PeerStep::Refuse(_)
    ));
    let status = pilot.store.peer_status().await.unwrap();
    assert_eq!(
        status
            .iter()
            .filter(|line| line["type"] == "job" && line["state"] == "cancelled")
            .count(),
        2
    );
    // Only the other generation's credential may cancel; a recipient cannot.
    assert!(matches!(
        pilot
            .store
            .prepare_job_cancel(&pilot.qa.credential, &pilot.qa.signer, &cancel(&after))
            .await
            .unwrap(),
        PeerStep::Refuse(_)
    ));
}

#[tokio::test]
async fn grant_issue_requires_exact_active_sender_and_one_capability() {
    let pilot = Pilot::new().await;
    // Re-issuing the identical grant is idempotent.
    pilot
        .store
        .issue_peer_grant(&pilot.grant(GENERATION_GRANT))
        .await
        .unwrap();
    let mut other_capability = pilot.grant("66666666-6666-4666-8666-666666666666");
    other_capability.capability = "qa.release".into();
    assert!(pilot
        .store
        .issue_peer_grant(&other_capability)
        .await
        .is_err());
    let mut self_grant = pilot.grant("77777777-7777-4777-8777-777777777777");
    self_grant.recipient_pubkey = PubkeyHex::parse(pilot.figma.keys.public_key().to_hex()).unwrap();
    assert!(pilot.store.issue_peer_grant(&self_grant).await.is_err());
    let mut unknown = pilot.grant("88888888-8888-4888-8888-888888888888");
    unknown.sender.generation = OTHER_GENERATION.into();
    assert!(pilot.store.issue_peer_grant(&unknown).await.is_err());
    // A second active grant for the same tuple is refused.
    assert!(pilot
        .store
        .issue_peer_grant(&pilot.grant("99999999-9999-4999-8999-999999999999"))
        .await
        .is_err());
    pilot.store.fence(&pilot.figma.identity).await.unwrap();
    assert!(pilot
        .store
        .issue_peer_grant(&pilot.grant("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"))
        .await
        .is_err());
    assert!(pilot
        .store
        .revoke_peer_grant("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb")
        .await
        .is_err());
}
