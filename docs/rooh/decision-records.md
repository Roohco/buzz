# Decision records and job telemetry

Status: implemented in bridge PRs under review, 2026-11-17 ([Roohco/qa-agent#64](https://github.com/Roohco/qa-agent/pull/64), [Roohco/figma2code-agent#342](https://github.com/Roohco/figma2code-agent/pull/342)). Contracts: `rooh.buzz.decision_record.v1`, `rooh.buzz.decision_ledger.v1`, `rooh.buzz.telemetry.v1`.

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
  - For a Figma candidate or a QA job result, the revision is the candidate SHA, so a 7-character prefix is specific.
  - For a QA-07 result, the revision is the Trello card's `dateLastActivity`, for example `2026-11-06t13:18:52.631z`. A 7-character prefix such as `2026-11` matches every version from that month and is a weak check. To bind a decision to one card version, quote the whole value.
- The `revision` value must be one token, with no spaces. If a `revision:` line contains prose, the decision still counts without a revision. The ingest log records the warning `revision_unparsable`.
- Keys are case-insensitive. `:` and `=` both work as separators.
- Markdown that a chat client adds is removed before matching: list markers, `*` or backtick emphasis, and underscores wrapping a word. For example, `- **Decision:** _approved_` is read as `decision: approved`. Underscores inside a value, as in `false_positive`, are kept.
- Blockquotes are not stripped. A quoted line such as `> decision: approved` is someone else's words and never counts.
- In values, spaces and hyphens become `_`, so `Needs rework` and `visual-mismatch` are accepted. Other lines are ignored.
- **To change a decision, post a new reply.** A later decision by any allowed reviewer on the same result replaces the earlier one in reports. Records are never rewritten; the latest `ts` is in force.
- Editing a Buzz message (kind 40003) or deleting it (kind 5) does not change the record. The broker's `channel.read` returns only kinds 9, 40002, 40008, 45001 and 45003, so the bridge never sees edits or deletions. A deleted decision stays recorded until a newer reply replaces it.
- Reactions (kind 7) carry no `h` tag, so a channel read never returns them. `channel.read` with `reactionsTo: [<event id>, …]` returns only the reactions to those events: 1 to 16 ids, and not together with `rootEventId`, `mentionsOnly` or `cursor`. The broker first checks that every id is a signed kind-9 message in `channelId` and refuses the whole read with `invalid_request` otherwise. It returns a verified kind 7 only when its reacted-to event names a requested id. That event is the last `e` tag holding 64 hex characters, the relay's own NIP-25 rule. A reaction to another event is dropped. The page holds the newest matching reactions, at most 16, sorted oldest first by `created_at` and then id, and has no cursor. A full page may leave older reactions out, so ask for fewer ids. To fill the page, the broker reads up to four relay pages. If those are still not enough, because reactions to other events crowd them, the read fails with `invalid_request` rather than return a page that may miss a verdict. A reaction removed by a NIP-09 deletion (kind 5) is soft-deleted by the relay and is no longer returned. Mapping an emoji to a verdict is the bridge's job.

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
| Exactly one `h` tag, equal to the channel the results were published in. This also applies to `--events` files | `wrong_channel` |
| NIP-01 id matches the content | `bad_id` |
| `created_at` is at most 15 minutes ahead of the ingesting host's clock. This applies to every reply, from the relay or from a file | `future_timestamp` |
| Author is in the bridge's `BUZZ_REVIEWER_PUBKEYS` allowlist | `not_reviewer` |
| BIP-340 signature by the author | `bad_signature` |
| Exactly one `["e", <id>, <relay>, "reply"]` tag | `not_a_reply` |
| The answered event is a result this bridge published, according to its ledger | `unknown_result` |
| Text contains a `decision`, `label` or `revision` line. Ordinary conversation, however long, is not logged | `not_a_decision` |
| A reply with decision fields is at most 4000 characters | `malformed:too_long` |
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
| `result_ts`, `result_ts_source` | `event`: signed `created_at` of the result, when the read returned that event. `ledger`: time kept in the bridge ledger. Null otherwise. See the table below |
| `reviewer_latency_ms` | `ts − result_ts`, or null when `result_ts` is unknown |

Where each result's time comes from:

| Result | Time source |
| --- | --- |
| QA-07 result | Ledger publish time (`updated_at` of the published record), or the signed event when the read reaches it |
| `qa.review_candidate` result (43004) | Ledger `result_published_at`. `channel.read` never returns job events |
| Figma candidate post | Signed event when the read reaches it. The attempt's `candidatePostedAt` (request time) bounds the read only |
| QA result in a Figma pipeline | Ledger `result.createdAt`: the signed `created_at` of the terminal event, recorded at poll time |
| QA-pass owner notice | Ledger `humanReview.publishedAt`, or the signed event when the read reaches it |

| Bridge | Command | Sidecar |
| --- | --- | --- |
| QA-07 results | `node scripts/buzz-decisions.mjs --ledger qa07` | `runs/.qa07-buzz-ledger.decisions.json` |
| `qa.review_candidate` | `node scripts/buzz-decisions.mjs --ledger job` | `$QA_JOB_STATE_DIR/ledger.decisions.json` |
| Figma pipeline | `npm run figma2code:decisions -- ingest --ledger-dir <dir> --pipeline-id <id>` | `<dir>/<pipeline_id>.decisions.json` |

### Read window

Each command reads the channel through the broker's `channel.read`.

- The host returns at most 16 events per page, newest first. Its opaque cursor pages backwards, and the bridges request exactly 16.
- Reading stops at the first of these:
  - the host has nothing older;
  - a page reaches below the read bound;
  - 200 pages (3,200 events) have been read.
- Only results published within the lookback count, whether decided or not. The lookback is 30 days by default; set `BUZZ_DECISION_LOOKBACK_DAYS` to change it. Most QA-07 results never get a reply, so an old undecided result must not keep the window open.
- The read bound is the earliest time known to precede each counted result's post, minus 15 minutes for relay clock drift. A reply cannot predate its result.
  - QA-07: `first_seen_at`.
  - `qa.review_candidate`: `started_at`.
  - Figma candidate: `candidatePostedAt`, the time just before the post was requested.
  - Figma QA result and owner notice: their ledger times.
- A result with no known time counts as the start of the lookback.
- A reply to a result older than the lookback, including a changed decision, is not picked up.
- Cursors are not persisted. Re-reading is idempotent.
- If the page cap is reached while older messages remain, the command still records what it read, prints a warning and exits with code 3. Older replies may then be missing.

With `--events <file>`, the command reads a JSON array, `{messages}` or JSONL file of signed events instead of the broker. The same checks apply, including the channel tag and the 15-minute bound.

The ingest lock is `<sidecar>.lock`, and the QA job adapter's `adapter.lock` works the same way.

- The lock holds the process id. It is created atomically: the id is written to a temporary file, which is then hard-linked to the lock path.
- A live holder makes the ingest fail without writing anything.
- A lock left by a dead process is taken over. The stale lock is renamed aside, then checked to be the same file; if another process got there first, it is restored and the ingest fails.
- An empty lock counts as live for 60 seconds.
- Release removes only the caller's own lock.

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
  - Telemetry is written wherever spend may have happened, including results that are `BLOCKED` after a budget or review error.
  - After a crash, telemetry is rebuilt from the adapter's hash-checked `review-result.json`. The end time is when that file was written, so a restart does not inflate the duration.
  - The sender-cancel path (Roohco/qa-agent#63) is to use the same `jobTelemetry` hook.
- Figma: `candidate.telemetry` on the `candidate_ready` entry in `<run>/bridge-ledger.jsonl`. The source is `<G0 workspace>/evidence/model-attempt-<n>.jsonl`.

## 6. Reports for R6

The report commands are read-only. Each prints one row per job and then a summary.

- Row fields: model, input and output tokens, cost, duration, decision in force, label and reviewer latency.
- The Figma report has one row per published event kind (`figma2code.candidate`, `qa.review_candidate.result`, `figma2code.qa_pass_notice`). Verdicts and decisions on different events are therefore never mixed. Generation telemetry and run duration are on the candidate row.
- Summary: p50, p90 and max duration; total and p50 cost; p50 and p90 reviewer latency; label counts.
- Commands:
  - QA: `node scripts/buzz-telemetry-report.mjs [--json]`
  - Figma: `npm run figma2code:decisions -- report --ledger-dir <dir> [--json]`

## 7. Not now

- Export of labeled decisions to a golden set. This is research item 5, after about 20 reviews.
- An OTel exporter or collector.
- Persisting `channel.read` cursors. Re-reading within the bounded window is idempotent.
- Honouring Buzz edits and deletions. The broker does not return them; post a new reply instead.
- Storing the reviewer's free text.
- Usage accounting inside the Figma provider proxy.
