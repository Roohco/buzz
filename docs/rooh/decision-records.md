# Decision records and job telemetry

Status: implemented in the bridges, 2026-11-17 ([Roohco/qa-agent#64](https://github.com/Roohco/qa-agent/pull/64), [Roohco/figma2code-agent#342](https://github.com/Roohco/figma2code-agent/pull/342)). Contracts: `rooh.buzz.decision_record.v1`, `rooh.buzz.decision_ledger.v1`, `rooh.buzz.telemetry.v1`.

A reviewer's answer to an agent result is labeled data. This document fixes how that answer is written in Buzz, how a bridge turns it into a machine-readable record, and which per-job numbers the ledgers keep so the [§8](ARCHITECTURE.md#8-verification-and-observability) metrics (run duration, model spend, reviewer time) can be measured without a new service. Both stay in the bridge's protected ledger. Buzz carries only the signed reply ([ADR 0002](adr/0002-coordination-not-replacement.md), [ADR 0004](adr/0004-task-evidence-contract.md)). A decision here never approves a release; release gates stay in GitHub.

## 1. Writing a decision in Buzz

Reply directly to the result message, for example a QA-07 result, a `qa.review_candidate` result, a Figma candidate or the QA-pass owner notice:

```text
decision: rejected
label: false_positive
revision: 1f4074a
Optional free text: why. It is never stored.
```

- `decision` is one of `approved`, `rejected` or `needs_rework`.
- `label` is one value from the taxonomy below.
- `revision` is optional. When it is present, it must equal the result's revision or be a prefix of it with at least 7 characters. Use it to guard against answering a stale candidate.
- Keys are case-insensitive. `:` and `=` both work as separators. In values, spaces and hyphens become `_`, so `Needs rework` and `visual-mismatch` are accepted. Other lines are ignored.
- A later decision by any allowed reviewer on the same result replaces the earlier one in reports. Records are never rewritten; the latest `ts` is in force.

## 2. Label taxonomy

The list is fixed. Adding a label is a contract change in both bridges and in this document.

| Label | Use when |
| --- | --- |
| `real_defect` | The agent found a genuine product defect |
| `visual_mismatch` | The candidate visibly differs from the approved design |
| `false_positive` | The agent reported a defect that is not one |
| `missing_input` | The task lacked essential input, such as a reference, attachment or text |
| `harness_issue` | The harness, environment or tooling failed, not the product |
| `flaky` | The result is not reproducible; rerunning gives a different outcome |
| `other` | None of the above. The reviewer explains in the free text, which is not stored |

The labels extend the PRD finding classes (product bug, harness issue, unverified, false positive) with the two cases R6 review needs: visual mismatch and flaky.

## 3. Acceptance rules

A reply counts only if every check passes, in this order. Otherwise it is ignored with the reason code shown, logged with only that code and the event id prefix, and not stored. An ignored reply is never treated as approval.

| Check | Reason when it fails |
| --- | --- |
| Signed Nostr event shape | `not_an_event` |
| Stream message kind 9 or 40002 | `unsupported_kind` |
| NIP-01 id matches the content | `bad_id` |
| Author is in the bridge's `BUZZ_REVIEWER_PUBKEYS` allowlist | `not_reviewer` |
| BIP-340 signature by the author | `bad_signature` |
| Exactly one `["e", <id>, <relay>, "reply"]` tag | `not_a_reply` |
| The answered event is a result this bridge published, according to its ledger | `unknown_result` |
| Text contains a decision; ordinary conversation is not logged | `not_a_decision` |
| At most 4000 characters | `malformed:too_long` |
| No key with two different values | `malformed:duplicate_field` |
| A `decision` line is present when `label` or `revision` is given | `malformed:decision_missing` |
| Decision value is one of the allowed values | `malformed:decision_value` |
| Label is present and in the taxonomy | `malformed:label_missing`, `malformed:label_value` |
| Stated revision matches | `malformed:revision_mismatch` |

The reviewer allowlist is configuration, not channel membership ([ADR 0003](adr/0003-authority-and-agent-identity.md)). If the list is empty, nothing is recorded. Both bridges run the same signed vectors: `src/schemas/rooh.buzz.decision_reply_vectors.v1.json` is a byte-identical copy in each repository, and each test suite pins its sha256.

## 4. Decision record

Records are stored in a protected sidecar next to the run ledger, under `{"schema": "rooh.buzz.decision_ledger.v1", "decisions": {<decision_event_id>: record}}`. Ingest is idempotent by `decision_event_id`, uses atomic writes and a lock file, and never writes the run ledger itself.

| Field | Value |
| --- | --- |
| `schema` | `rooh.buzz.decision_record.v1` |
| `decision_event_id` | Id of the reviewer's signed reply |
| `result_event_id` | The event answered |
| `decision`, `label` | Normalized values from §1–§2 |
| `revision`, `digest` | From the ledger for that result: QA-07 uses source version and evidence sha256; candidate and QA job results use candidate SHA and preview bundle digest |
| `reviewer_pubkey` | Signer |
| `ts` | Reply `created_at`, as ISO 8601 |
| `subject_key`, `publication_kind` | Ledger key and kind of the result |
| `result_ts`, `result_ts_source` | `event`: signed `created_at` of the result when it is among the events read. `ledger`: publish time recorded by the bridge. Otherwise null |
| `reviewer_latency_ms` | `ts − result_ts`, or null when `result_ts` is unknown |

| Bridge | Command | Sidecar |
| --- | --- | --- |
| QA-07 results | `node scripts/buzz-decisions.mjs --ledger qa07` | `runs/.qa07-buzz-ledger.decisions.json` |
| `qa.review_candidate` | `node scripts/buzz-decisions.mjs --ledger job` | `$QA_JOB_STATE_DIR/ledger.decisions.json` |
| Figma pipeline | `npm run figma2code:decisions -- ingest --ledger-dir <dir> --pipeline-id <id>` | `<dir>/<pipeline_id>.decisions.json` |

Each command reads the channel through the broker's `channel.read`, up to 20 pages. With `--events <file>`, it reads a JSON array, `{messages}` or JSONL file of signed events instead.

## 5. Job telemetry

Each model-backed job records flat attributes. Their names follow the [OpenTelemetry GenAI semantic conventions](https://github.com/open-telemetry/semantic-conventions-genai), so a later exporter can copy them onto a span unchanged. There is no OTel dependency, collector or backend. The attributes stay in the ledger and never appear in a Buzz message.

| Attribute | QA `qa.review_candidate` source | Figma candidate source |
| --- | --- | --- |
| `gen_ai.operation.name` | `chat`: one review call | `invoke_agent`: the Claude Code or codex worker |
| `gen_ai.provider.name` | `anthropic` | `anthropic` or `openai`, depending on the transcript |
| `gen_ai.request.model` | Route model, `src/routing.js` | Bridge `--model` |
| `gen_ai.response.model` | `modelUsage` key in `claude -p --output-format json`, when there is exactly one | `modelUsage` in the stream-json `result` event |
| `gen_ai.usage.input_tokens` | Fresh + cache read + cache write | Same, summed over model attempts |
| `gen_ai.usage.output_tokens` | `usage.output_tokens` | Same |
| `gen_ai.usage.cache_read.input_tokens`, `gen_ai.usage.cache_creation.input_tokens` | `usage.cache_read_input_tokens`, `usage.cache_creation_input_tokens` | Same |
| `rooh.model.calls` | Review result `cost.model_calls` | Number of model-attempt transcripts |
| `rooh.model.duration_ms` | CLI `duration_ms` | Sum of `result.duration_ms` |
| `rooh.job.duration_ms` | `started_at` to result ready | Report uses the bridge's `elapsedMs` for the whole A1–G0 run |
| `rooh.cost.usd_estimate` | CLI `total_cost_usd` | Sum of `result.total_cost_usd` |
| `rooh.cost.source` | `provider_reported`, or null | Same |

Cost is the provider's own estimate. On a subscription it is the API-price equivalent, not an invoice. If the provider reports no cost, the value stays null; no price table is invented. A killed model process emits no `result` event, so its tokens and cost are null rather than zero.

Where the values are stored:

- QA: `telemetry` and `result_published_at` on each job record in `$QA_JOB_STATE_DIR/ledger.json`.
- Figma: `candidate.telemetry` on the `candidate_ready` entry in `<run>/bridge-ledger.jsonl`. The source is `<G0 workspace>/evidence/model-attempt-<n>.jsonl`.

## 6. Reports for R6

The report commands are read-only. Each prints one row per job and then a summary.

- Row fields: model, input and output tokens, cost, duration, decision in force, label and reviewer latency.
- Summary: p50, p90 and max duration; total and p50 cost; p50 and p90 reviewer latency; label counts.
- Commands:
  - QA: `node scripts/buzz-telemetry-report.mjs [--json]`
  - Figma: `npm run figma2code:decisions -- report --ledger-dir <dir> [--json]`

## 7. Not now

- Export of labeled decisions to a golden set. This is research item 5, after about 20 reviews.
- An OTel exporter or collector.
- Persisting `channel.read` cursors. Re-reading is idempotent, so it is not needed now.
- Storing the reviewer's free text.
- Usage accounting inside the Figma provider proxy.
