# R1 isolated local relay evidence (2026-11-16)

Status: **partial proof; R1 gate open**. This is an operator record for the
disposable local experiment defined in [R1-real-relay-proof.md](R1-real-relay-proof.md).
It does not authorize a public pilot or production rollout.

## Provenance and limits

- Rooh source base: `b39e44db1cedefd55fd3cd259053f3846ac89eae`.
- Published R0 broker artifact: `ghcr.io/roohco/buzz-broker-host@sha256:60e66bd7d0f36be23568294f7da4b2a6b944477a3a5780ad1259869ef65856c8`.
  The main workflow verified its OCI revision and provenance, but the local
  operator token cannot pull this private package (`read:packages` denied).
- The local proof used a **different** broker build, image
  `buzz-broker-host:rooh-r0-local`, revision `53bc826e59824e2f5957aee74b8d1ac7f2482c39`;
  ACP and CLI came from
  `ghcr.io/malkovitc/buzz-sprig@sha256:9f350ff80feeaf0ebfbccc603b18e6b75871ff54482c26c6ba9351ff9981c5ab`,
  revision `e2a4622158816a29053d9ecc3ab3d193d90088be`.
- The real relay used the public
  `ghcr.io/block/buzz@sha256:35ddaf05d5e4c8f4dccc0e12dd7de4d30f30eb81f2d309b7b5a620a6939a5b69`,
  revision `1a5c6939c58d8840272ca43084a9620636854c57`. Its newer schema did not
  accept the Rooh fork's preexisting seeded schema. Only the disposable
  `buzz-harness` database was reset and migrated for this proof.
- Relay ports `3030` and `8088` were bound to `127.0.0.1`; Postgres, Redis and
  MinIO belonged to the separate `buzz-harness` Compose project. No Scaleway
  or shared project state was used.

## Observed event chain

| Step | Evidence |
| --- | --- |
| Owner private channel | `073f379a-c29b-4341-98a4-5dec42165923`; owner created it and added the agent as bot. |
| Owner task | `7609dcc84dbc4e5d0909d9d8fb4c98d0adb9072deba4d8be558674d588d6569f`; kind 9, explicit agent mention, run marker `cb72a86c-44c5-48c9-9282-3bb60a22713c`; owner authenticated read returned the same ID. |
| Broker read | Keyless agent CLI using a broker bearer, without `BUZZ_PRIVATE_KEY`, returned the exact task under `--mentions-only`. |
| Manual broker reply | `17cf6b52182ae770fb36193216655d1917833370b75179d102d1b6b792b4d271`; relay read returned kind 9, agent pubkey, and NIP-10 `e` tag pointing to the task with marker `reply`. |
| Deterministic ACP reply | `5bb263943c108f7bdfda5e11d095df4a7ed6f0b9bf67e1a183973d563dcf90b4`; actual `buzz-acp` invoked the fixture ACP peer, which used the broker-backed CLI. Authenticated relay read found exactly one `AE-ACK` for the run marker, with the agent pubkey and correct reply parent. |
| Outsider | A separate uninvited key read zero messages; its attempted post failed `restricted: not a channel member`. |
| Exact retry | Broker request `r1-exact-retry-001` returned the same event ID `e4e5b781511121704e673001f59fc4d4b03c08123e4e6ab770a4f0ef00403f3f` twice. Changed content under that ID failed `request_id_conflict`. Authenticated relay read found one effect. |
| Relay outage | With the disposable relay stopped, `message.post` request `r1-outage-effect-001` returned `outcome_unknown`. After quiesce, fence failed `EffectsPending`; a successor issue failed `ScopeAlreadyActive`. Restart and operator drain reconciled the effect; exact retry returned event `c2f4740a3a478952a48be27dc1cb6cbf87f0e79019ee6bff834b6732488937f8`, present once at the relay. Fence then succeeded, and successor issue became possible. |
| Terminal lifecycle | The first generation rejected new work after quiesce; drain found no pending effect; fence denied the old bearer and reissue (`GenerationAlreadyIssued`). The successor generated during the outage was also quiesced and fenced after the pending effect resolved. |

The broker's read path validates signed relay events before returning them. The
local record checks event IDs, author pubkey, kind and reply tags after an
authenticated read; it does not claim an independent, external signature
verification artifact.

## Open R1 gate

Update: the CI job `broker-relay-e2e` now reproduces this chain against a
relay built from the same commit. It covers the ACP long-running poll check
(item 2), independent signature verification and the redacted timeline (item
3). See [R1-ci-proof-evidence.md](R1-ci-proof-evidence.md). Item 1 (the run on
the deployed canary image) and item 4 remain open.

1. Repeat the end-to-end proof with the exact Rooh-pinned relay, broker and ACP
   revisions. The local broker and ACP images above are from earlier/personal
   builds, and the relay is newer upstream. Package access or an isolated CI
   job is needed for a pinned run.
2. Investigate an unexpected ACP exit about 30 seconds after its successful
   reply. The log reported `managed ACP authority was rejected by the host`;
   a later direct `authority.status` probe for that generation returned
   `active` and the expected identity. This occurred before the deliberate
   relay outage and quiesce. A deterministic regression test subsequently
   proved that one broker `channel.read` `action_failed` verdict also made the
   poller exit while authority stayed active. The local fix retries that one
   class of read failure and retains fail-closed behavior for `unauthorized`.
   This test does not prove which broker verdict caused the original live exit;
   repeat the live run with a redacted action-code timeline before treating
   long-running ACP health as proven.
3. Independently verify signed event signatures and preserve a redacted
   machine-readable proof on the exact pinned run. Record ACP and broker
   request IDs for the production-shaped evidence bundle.
4. The main Security job still fails on the existing RustSec baseline; broker
   tree advisories remain a separate public-pilot blocker.

All private keys, auth tags, bearers and the disposable relay key were kept
only in owner-readable temporary files and are excluded from this document.

Local follow-up verification: the new ACP regression failed before the code
change (`poller should remain open`) and passed after it. The full `buzz-acp`
unit suite passed 858/858, `buzz-broker-client` passed 13/13, and Clippy for
both affected crates passed with warnings denied. The repository-wide `just ci`
gate then exited 0 on branch `feat/rooh-r1-real-relay` at `4a977b2a` after its
Rust, desktop, Tauri, web and mobile checks; mobile reported 1,835 passing
tests. The exact-image live rerun, redacted action-code timeline and
independent signature verification remain outstanding, so R1 is still open.
