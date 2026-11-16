# R1 CI proof against a real relay

Status: **reproducible CI proof for the Slice 0 broker path; R1 remains open
only for the deployed canary image run.** This record complements the manual
[local evidence](R1-local-proof-evidence.md) and follows the
[proof plan](R1-real-relay-proof.md).

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

1. **Deployed canary image.** CI proves the source revision, with a relay built
   from the same commit. It does not run the published broker image
   (`ghcr.io/roohco/buzz-broker-host@sha256:...`) or a pinned relay image.
   Repeat the same tests against the provenance-verified canary digests, and
   record the digests and the evidence artifact.
2. **Rollback rehearsal and release evidence** in DoD §2: canary digest, local
   gate and rollback rehearsal on the deployed path.
3. **RustSec baseline.** Broker-tree advisories remain a separate public-pilot
   blocker.

## Rollback

The change adds a test target and one CI job. Revert the commit, or delete the
`broker-relay-e2e` job, to remove it. Production code is unchanged.
