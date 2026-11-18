# R2 live evidence — first real QA-07 card projected to Buzz

Date: 2026-11-17. Gate: ROADMAP §5 (R2), DoD QA-07 projection items.

## Revisions

| Component | Revision |
| --- | --- |
| QA-07 watcher + bridge (qa-agent `main`) | `bf051c6bde4b6d558b460dde4a3b1d3faa07a4cd` (deployed; triage self-check 11/11) |
| Relay | `ghcr.io/roohco/buzz@sha256:df84f2635ccf2c45925931609671320c6c52a4b87ac33bd6e8986a67617eda7d` (rev `d50079403`) |
| Broker (QA-07 signer) | `ghcr.io/roohco/buzz-broker-host@sha256:6988cb86f4c0e26a7f6300964299d21c5f36227ebc639fedc5d118c1f7468ce8` (rev `fe284dd33`) |
| Pinned-artifact proof of these digests | R1 Pinned Artifact Proof run 44029789496 (attestations verified, real-relay proof, rollback) |

Deployment shape: [PILOT-deploy.md](../PILOT-deploy.md).

## The run

- Source: Trello card `6af576ab831530105e8d69e6`, version `2026-11-17T09:16:16.555Z` (eligible card picked up by the production watcher; not replayed or synthesized).
- Run `ef85933b-8616-4a0d-a8f5-707b8ec364bf` on harness `bf051c6`.
- Watcher completion marker (`qa07.watcher_completion.v1`): `watcher_state=reported`, `notion_outcome=published`, `slack_outcome=acknowledged`, task text present (sha256 `f39e0431…c5d353`, not truncated), attachments none, reference not required; one landing; findings: product_bug 3, known_issue 0, harness_failure 0, observation 4; report file sha256 `6b7e0068…e89bde`; evidence sha256 `d80412ab8ada6133098b5b17ddae6ee21c28d0675792b7ce3d74755bf310c487`; completed `2026-11-17T09:26:40Z`.
- Bridge pass (cron, `2026-11-17T09:30:03Z`): `published: 1`, guard refusals 0, halted null.

## The Buzz event

- Event `ab8887e4a4297dbdd6cab5e1a525cb7b6ea473302ed1965b13580edbb9b89ff1`, kind 9, author = QA-07 agent `785b9ccc6cd88a454503fdafa3a20bf62889914008bbb550704c9ce8fcbbacfb`, accepted by the relay (signature and NIP-OA `auth` tag verified on ingest; tag condition `created_at<1802649600`).
- Tags: `h` = the pilot QA results channel; `e … reply` = the operator's R2 task event.
- Content: human summary + fenced `rooh.buzz.envelope.v1` with `outcome=FAILED`, `input_status=complete`, findings counts, `report=notion_published`, `landings=1`, evidence ref + sha256, idempotency key `trello|<card>|<version>|<run>|qa07.result`.
- Bridge ledger record: `state=published`, `attempts=1`, broker request id `qa07-69d201aa…9938a5`, generation `3dda9543-bde1-49f9-9992-e85789f7e233`.

## Projection matches source

Outcome, card id/version, run id, harness revision, input status, finding counts, Notion/Slack outcomes and evidence hash in the event equal the completion marker field by field. Free task text, URLs and finding titles are not projected (classification `internal`, pre-sign guard passed).

## No duplicates

A repeat bridge pass (same revision, same marker) reports `already: 1, published: 0`; the relay holds exactly one QA-07 result event. Replay, restart, relay-outage and missing-reference behaviour are covered by qa-agent `npm test` (bridge/completion suites) at the deployed revision.

## Still open for R2 Done

- Owner review of a result in Buzz. Since 2026-11-18 the owner is a member and reviewer, results are top-level human-readable messages, and verdicts (thread replies or ✅/❌/🔁 reactions) are recorded every 5 minutes; no verdict yet.
- Notion-failure fallback: tested separately in the suite, not observed live.
- QA-07 Slack-acknowledgement duplicate risk: tracked separately (unchanged by the bridge).
