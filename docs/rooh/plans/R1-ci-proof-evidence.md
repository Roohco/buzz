# R1 CI proof against a real relay

Status: **R1 proven against the exact published artifacts, rollback rehearsed.**
The Slice 0 broker path passed against digest-pinned, provenance-verified
images on 2026-11-16 ([pinned run](#pinned-published-artifacts)). The only open
item is the RustSec baseline, which is a separate public-pilot blocker. This
record complements the manual [local evidence](R1-local-proof-evidence.md) and
follows the [proof plan](R1-real-relay-proof.md).

## What runs

CI job `broker-relay-e2e` ("Broker Real-Relay E2E") in
`.github/workflows/ci.yml` builds `buzz-relay` from the same commit, starts it
with Postgres, Redis and Silo through `scripts/start-relay-for-tests.sh`, and
runs two ignored tests from
`crates/buzz-broker-host/tests/real_relay_e2e/`:

1. `r1_broker_authority_against_real_relay` drives the real
   `buzz-broker-host` binary (`issue`, `serve`, `quiesce`, `drain`, `fence`).
   The client holds only a broker bearer: no private key, auth tag or relay URL.
2. `acp_liveness::r1_keyless_acp_stays_live_against_real_relay` runs the actual
   `buzz-acp` and `buzz` CLI in broker mode with the deterministic
   `fake-acp-post.mjs` peer (no model).

The job runs when the broker host, the relay surface or `ci.yml` changes. It
uploads artifact `r1-broker-real-relay-evidence` containing
`r1-broker-real-relay.json` and `r1-acp-liveness.json`.

The broker's relay traffic goes through an in-test HTTP proxy (`HTTP_PROXY` for
the broker process only). The proxy can drop relay responses after the request
reached the relay (lost ACK) or refuse connections (outage). The shared relay
is never stopped.

## Coverage

| Slice 0 acceptance | Proven by |
| --- | --- |
| Private channel, separate agent; non-member cannot read or post | Owner creates a private channel and adds the agent. Outsider `/query` returns no events; outsider post fails `restricted: not a channel member`. A bearer the host never issued gets `unauthenticated`. |
| Owner task, broker-backed reply to that exact event, same ID on read | Owner kind 9 task with agent mention and preallocated `run_id`; authenticated read returns the same ID. Keyless `channel.read` (mentions only) returns it. `message.reply` returns an event ID; relay read returns kind 9, the agent pubkey, the NIP-10 `reply` tag to the task and one valid owner attestation. |
| Exact retry and changed bytes | Exact retry returns the same event ID with `replayed=true`, and the relay holds one copy. Changed content under the same request ID returns `request_id_conflict`. |
| Lost relay ACK | `outcome_unknown`; the relay holds the event once. Exact retry reconciles to that event ID without a second copy. |
| Relay loss during a pending effect; no automatic successor | Under outage, `message.post` returns `outcome_unknown` and nothing reaches the relay. A second effect fails `action_failed`. After quiesce, fence fails `EffectsPending` and successor issue fails `ScopeAlreadyActive`. With the relay restored, operator `drain` publishes one copy and exact retry returns the same ID. |
| Quiesce, drain, fence; fenced generation cannot publish | After drain, the quiescing bearer gets `unauthorized`; fence succeeds. The fenced bearer gets `unauthorized` and nothing reaches the relay. Reissue of the old generation fails `GenerationAlreadyIssued`. The successor generation issues only after the prior effect is terminal and publishes once. |
| Long-running managed ACP | Real `buzz-acp` replies through the broker, stays alive for 65 seconds of idle polling, answers a second task, and exits cleanly after quiesce. The generation is then fenced. |

Every event read back from the relay is checked with `nostr::Event::verify`
(event ID and Schnorr signature) in the test process, independently of the
broker's validation. The evidence JSON records the steps, broker request IDs,
generations, action outcome codes, event IDs, relay copy counts and verified
event IDs. Before writing it, the test asserts that the JSON contains no bearer,
private key or auth tag.

## Pinned published artifacts

Workflow `r1-pinned-artifact-proof.yml` (dispatch-only, from `main`) pulls each
image by digest. It requires an OCI revision that is an ancestor of the
dispatched commit and runs `gh attestation verify` on each image. It also checks
that the relay revision's `schema/` matches the commit. It then copies the
broker and Sprig binaries out of the images and runs the relay as its published
container. The same three tests then run with
`BUZZ_E2E_BROKER_HOST_BIN`, `BUZZ_E2E_ACP_BIN`, `BUZZ_E2E_CLI_BIN` and
`BUZZ_E2E_ROLLBACK_BROKER_HOST_BIN` pointing at those exact binaries.

- Proof run: [43943214173](https://github.com/Roohco/buzz/actions/runs/43943214173), commit `98e36c66d6150ac27cdc15af6572dda7c32f22b7`,
  conclusion **success**.
- Evidence artifact: [`r1-pinned-artifact-evidence`](https://github.com/Roohco/buzz/actions/runs/43943214173/artifacts/14263981564) (expires
  2026-12-16): `r1-pinned-artifacts.json`, `r1-broker-real-relay.json`,
  `r1-acp-liveness.json`, `r1-broker-rollback.json`. Each timeline embeds the
  digests under `pinned_artifacts`. A search of all four files for bearer (`bzh_`) or
  `nsec` material found none, and the tests also assert the absence of their
  own keys, bearers and auth tags.
- Current broker canary: published by
  [managed-agent-smoke run 43940921809](https://github.com/Roohco/buzz/actions/runs/43940921809)
  on the same commit.

| Role | Image | OCI revision | Provenance |
| --- | --- | --- | --- |
| Broker (current, N) | `ghcr.io/roohco/buzz-broker-host@sha256:26725cde8c0f55368d745f57a456f94a2ecb3e30c006794daba4b6e3550f6819` | `98e36c66d` | verified |
| Broker (rollback, N-1) | `ghcr.io/roohco/buzz-broker-host@sha256:60e66bd7d0f36be23568294f7da4b2a6b944477a3a5780ad1259869ef65856c8` | `b39e44db1` | verified |
| Relay | `ghcr.io/roohco/buzz@sha256:30549038f469f28dd5f5a8c14ccf83b99f563865a15bd92998f1a84e7a0a7abb` | `d4b7e8110` | verified |
| Sprig (`buzz-acp`, `buzz`) | `ghcr.io/roohco/buzz-sprig@sha256:b7e9895b8a530f92b92e5c0f6a27337b5981a5e60b4b1186e6ab21a33218614f` | `d4b7e8110` | verified |

Extracted binary SHA-256: current broker `637c1d95…a331`, rollback broker
`10a3d2f4…fc6c`, Sprig `e6339bdd…a341` (full values in
`r1-pinned-artifacts.json`). The relay and Sprig images are the latest ones
published on `main`. Their workflows skip broker-only changes, so `d4b7e8110`
remains the current relay and Sprig source.

### Slice 0 checklist (pinned run)

Step names refer to entries in the evidence files above.

- [x] Private channel, separate agent; non-member denied:
  `owner_private_channel_and_agent_member`, `outsider_denied`,
  `unissued_bearer_denied` (`r1-broker-real-relay.json`).
- [x] Owner task acknowledged and read back by the same ID; keyless broker read:
  `owner_task_ack_and_read`, `keyless_channel_read`.
- [x] Broker-backed reply to that exact event, agent-signed, owner-attested, one
  copy: `keyless_message_reply`.
- [x] Exact retry returns the same ID; changed bytes rejected:
  `exact_retry_same_event`, `changed_bytes_conflict`.
- [x] Lost relay ACK reconciles without a duplicate:
  `lost_ack_outcome_unknown`, `lost_ack_exact_retry_reconciled`.
- [x] Relay loss during a pending effect; no automatic successor; operator drain
  publishes once: `outage_outcome_unknown`,
  `second_effect_blocked_while_pending`,
  `quiesce_fence_successor_while_pending`, `drain_and_exact_retry_reconciled`.
- [x] Quiesce, drain, fence; fenced generation cannot publish or be reissued;
  successor publishes once: `quiesced_generation_refuses_new_effect`,
  `fence_after_drain`, `fenced_bearer_denied`, `old_generation_reissue_denied`,
  `fresh_generation_publishes`, `teardown_successor_fenced`.
- [x] Long-running managed ACP (real `buzz-acp` and `buzz` from Sprig): replies,
  is live after 65 s idle, exits cleanly on quiesce and is fenced:
  `acp_first_reply`, `acp_reply_after_idle`, `acp_quiesce_exit_and_fence`
  (`r1-acp-liveness.json`).
- [x] Rollback N to N-1 on the same authority state (`r1-broker-rollback.json`):
  N publishes, then quiesce, drain (`no pending effect`) and fence; N is
  killed; N-1 starts; the fenced bearer gets `unauthorized` with zero relay
  copies; the fenced generation reissue gets `GenerationAlreadyIssued`; a fresh
  generation publishes exactly once and is fenced:
  `current_artifact_publishes` through `rollback_teardown_fenced`.
- [x] Every relay-returned event verified independently (ID and Schnorr): 5 in
  the broker proof, 2 in ACP liveness, 2 in rollback.
- [x] Image provenance: all four images' attestations verified, revisions in
  `main` history.

To repeat the proof for a new canary, dispatch `managed-agent-smoke.yml` on `main`. Then
dispatch `r1-pinned-artifact-proof.yml` with the new broker digest, the
previous broker digest as `rollback_broker_digest`, and the latest relay and
Sprig digests from their `main` workflow summaries.

## Finding: loopback community host

`AuthorityIdentity.communityRelayUrl` is canonicalized by
`buzz_core::relay::normalize_relay_url`, which maps `localhost` to
`127.0.0.1`. The broker uses that identity URL to connect and to sign NIP-98,
while the relay binds tenants by `Host`. A community mapped only as
`localhost:3000` therefore returns `404` to the broker. The CI job maps
`127.0.0.1:3000` to a separate proof community, and the test's owner and
outsider use the same canonical URL.

Production `wss://` hosts are only lowercased, so this affects loopback
development relays only. Changing the broker's connection URL is a separate
broker/identity change and is not part of this proof.

## Remaining for R1

None. The RustSec baseline closed with PR #7: the `Security` job on main
`539f864a` (run https://github.com/Roohco/buzz/actions/runs/43940573381) passed
for the first time in the fork, with reasoned ignores and bans for
RUSTSEC-2026-0459, RUSTSEC-2026-0462 and RUSTSEC-2024-0449 in `deny.toml`. The
deployed-image run and the rollback rehearsal are closed by the
[pinned run](#pinned-published-artifacts). R1 is done; R2–R4 adapters connect
to the broker next.

## Rollback

The change adds a test target and one CI job. Revert the commit, or delete the
`broker-relay-e2e` job, to remove it. Production code is unchanged.
