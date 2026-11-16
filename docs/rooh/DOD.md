# Definition of Done: Rooh AI Native Organization

Status: proposed acceptance and release gates, 2026-11-16. A checked document is not evidence that software has shipped. Record a link to the test run, signed event, artifact or deployed revision beside each completed criterion in the delivery PR or release record.

## 1. General gate for every slice

A slice is Done only when all applicable items below have observed evidence:

- [ ] The PR names the PRD requirement IDs it closes, exact source revisions, affected files/components, one rollback action and any remaining limitation.
- [ ] Local final gate declared for that slice passes before the first push. Code changes run the affected unit/integration/build checks and repository-prescribed checks; documentation-only changes pass link, Markdown structure and `git diff --check` checks.
- [ ] Human reviewer verifies that the delivered behavior matches the original source input, not a shortened URL/title or model summary. Missing essential input is marked `input_incomplete`.
- [ ] Negative permission tests cover at least an unauthorized channel member, ungranted capability, stale source revision and fenced generation when those boundaries are touched.
- [ ] A real affected workflow is exercised where practical; mocks and synthetic relay tests are identified as such.
- [ ] The exact commit, CI result, release artifact/image digest and deployed revision are captured before any production completion claim.
- [ ] No secrets, private evidence, model transcripts or customer data appear in public repository content, Buzz message bodies or test logs.
- [ ] The implementation has no new queue/store/service/abstraction without a named acceptance or correctness/security requirement. Reviewer can explain its removal cost.

If CI is not running or the full real-user path has not been exercised, mark the slice `implemented, not release-verified`. A green local test alone is not production Done.

| PRD requirement | Primary proof below |
| --- | --- |
| FR-01, FR-02, FR-08 | Slice 0: private identities, signed task/reply and fenced replacement |
| FR-03, FR-04 | Slice 1: one completed QA projection and input-completeness review |
| FR-05, FR-06 | Slice 2: pinned one-screen generation and honest candidate status |
| FR-07 | General permission gate and negative tests in every slice |
| FR-09, FR-10 | Slices 4–5: private cross-group channel and authorized task handoff |
| FR-11 | Slice 3: independent candidate QA with complete input |
| FR-12 | Slice 5: bounded rework and stale-verdict rejection |

## 2. Slice 0: Rooh fork and real-relay agent proof

### Functional acceptance

- [ ] `Roohco/buzz` is the owned fork; its main and release branch record their parent/upstream commits. The deployed image is built from the Rooh fork and resolves to an exact commit and digest.
- [ ] One private channel has one named human owner and one separate agent identity. A non-member cannot read or post.
- [ ] The owner signs one task mention; the broker-backed agent replies to that exact event. The relay accepts and returns the same signed event ID on read.
- [ ] The operator can quiesce, drain and fence the agent generation. A fenced generation cannot initiate a new message effect.
- [ ] Relay loss during a pending effect produces an explicit unresolved state and no automatic successor until reconciliation. Recovery is documented and witnessed once.

### Release evidence

Attach the channel/task/reply event IDs, broker request ID and non-secret authority status, negative-test results, exact canary image digest, local gate and rollback rehearsal. Existing synthetic-container proof does not satisfy this gate.

## 3. Slice 1: QA evidence projection

### Functional acceptance

- [ ] A real eligible QA-07 card completes the existing card → receipt → successful Notion publication → Slack acknowledgement path on the intended QA revision; the final watcher state and actual Notion/Slack outcomes are recorded. The Notion-failure fallback is tested separately and labeled as such.
- [ ] The bridge consumes a durable completed-run marker with exact card ID/version and run ID. It does not infer completion from worker output alone.
- [ ] The Buzz item links the exact card version, original task/reference availability, report and finding categories. A reviewer can distinguish product bug, harness failure, observation and `NOT_VERIFIED`.
- [ ] Replaying the bridge input or restarting the bridge creates one Buzz result and does not start a second browser run or cause an adapter-initiated Notion/Slack publication.
- [ ] Missing task text, attachment or required dated reference yields `input_incomplete` or `NOT_VERIFIED`, never an unsupported product `FAILED` verdict.
- [ ] Buzz outage leaves QA-07 operational and retains a retryable unpublished receipt; retry publishes the same run or reconciles an already accepted event.

### Release evidence

Attach source card/version, protected report reference and hash, watcher completion marker, Slack outcome, Buzz event ID, duplicate-replay result, permission negative test and actual reviewer verdict. Track any pre-existing QA watcher duplicate-publication risk separately; do not claim it is fixed by the bridge.

## 4. Slice 2: one-screen Figma candidate

### Functional acceptance

- [ ] The exact Figma screen/root/version and pinned executable revision are recorded. A1 stops at `SCREEN_REVIEW_REQUIRED` until a human confirms the complete frame.
- [ ] A2 and A3 receipts match the A1 source identity; G0 runs in its existing isolated workspace and respects the declared model/time budget.
- [ ] A valid generation-only G0 exit 2 appears as `generated_not_release_validated`. Build status and `browser/visual/release = not_run` are visible; no message calls it shipped.
- [ ] A protected immutable static preview bundle has a manifest binding the candidate git tree, G0 build receipt and SHA-256 of each served file; the bundle is retrievable for later QA without a mutable preview URL.
- [ ] Mismatched identity, missing confirmation, `BLOCKED`, timeout and missing artifact each stop further execution with an inspectable reason.
- [ ] Product repository contents, PRs and deployment are unchanged by this slice.

### Release evidence

Attach protected A1/A2/A3/G0 receipts and hashes, confirmation tied to the exact design manifest, candidate git-tree/source SHA, preview bundle/file-hash manifest, budget result, negative-test results and reviewer decision. A candidate alone is not a production feature.

## 5. Later organization capabilities

### Slice 3: independent candidate QA

- [ ] A human can request `qa.review_candidate` for one immutable Figma candidate. The QA agent receives the exact candidate SHA, content-addressed static bundle and file-hash manifest, full screen/task reference, G0 receipts and budget.
- [ ] The generated page runs in a disposable preview/browser boundary without credential mounts, host-service access or arbitrary egress; only its pinned static origin and explicit local fixtures are reachable. Protected design/task references enter QA separately and are not exposed to candidate JavaScript.
- [ ] QA verifies the bundle's binding to the requested candidate tree/build receipt and hashes of HTML, scripts, styles and assets actually served to its browser. A real browser check then produces a revision-bound `PASS`, `DEFECT`, `BLOCKED` or `INPUT_INCOMPLETE` with protected evidence and verified bundle identity. A QA-07 prepayment landing verdict is not accepted as substitute evidence.
- [ ] A missing preview, incomplete frame/reference, changed candidate SHA, stale/mutated served byte or unexpected outbound request blocks `PASS`.
- [ ] The QA process cannot modify the candidate, publish a QA-07 report or trigger a product release.

Evidence: one real candidate review, its browser artifacts and served-byte hashes, an input-incomplete case, a stale-bundle case, an outbound-request/secret-reachability denial, exact QA runner revision and local gate.

### Slice 4: authorized cross-group handoff

- [ ] One project has separate private Figma and QA group channels plus one private handoff channel with explicit membership. An agent in one group cannot read the other group's internal channel.
- [ ] A Figma agent with a specific project/recipient/`qa.review_candidate` grant signs a request for one candidate SHA and preview bundle digest. The QA agent accepts or rejects it with a linked signed event; result refers to the same request, pipeline ID, SHA and verified digest.
- [ ] Wrong project, ungranted peer, stale candidate or bundle digest, expired budget and fenced generation are denied before QA browser/model execution.
- [ ] Duplicate request delivery creates at most one QA run; cancellation/retry and relay-acknowledgement loss have an inspectable terminal or unresolved state.

Evidence: signed event chain, broker grant and request IDs, recipient decision, negative tests, duplicate/cancel proof and real-relay retrieval. An owner manually forwarding the task proves the QA capability but does not complete this autonomous handoff slice.

### Slice 5: one unified Figma → QA pipeline

- [ ] One real confirmed Figma screen yields an exact candidate, an autonomous QA request in the handoff channel, an independent QA result and a human decision linked by one `pipeline_id`.
- [ ] A deterministic defect fixture causes one automatic Figma repair linked to the prior candidate SHA and exact QA defect evidence, then a new QA request tied to the new SHA and bundle digest. The previous QA verdict is rejected as stale for either changed identity.
- [ ] A second defect, blocked input, timeout, cancelled request or uncertain broker effect stops automatic work and requires the owner. The loop cannot exceed one automatic rework.
- [ ] The full event chain is visible from Buzz without exposing either group's private channel or protected artifacts to the other group by default.
- [ ] No product repository write, merge, deploy or payment action occurs from this pipeline.

Evidence: real end-to-end run, fixture defect/rework run, event/receipt/hash chain, reviewer decision, cost and latency, negative authorization tests, exact deployed Rooh revision and rollback rehearsal.

## 6. Future optional organization capabilities

The following are separate delivery slices, not implied by the organization MVP:

| Capability | Additional DoD before activation |
| --- | --- |
| Typed job board | Request publisher, authorized consumer, accepted/progress/result/cancel handling, replay/cancel tests and user-visible status agree on one job ID |
| General agent-to-agent delegation | Additional recipient capabilities beyond `qa.review_candidate`, bounded depth/breadth, cycle/timeout limits and negative test for ungranted peer |
| Durable approvals | Approval token and exact revision persist across restart; executor resumes once; rejection and stale-revision tests pass |
| Product writes / PR creation | Repository-specific assignment, isolated target checkout, existing project gates, human review and reversible rollback |
| Merge / deploy | Existing GitHub/release authority confirms exact artifact; Buzz never treats a chat message or model claim as deployment proof |
| Voice action | Transcript/request is bound to a named human, exact capability, target revision and explicit confirmation; same permission tests as text path |
| External A2A | Named external consumer, authentication and data-boundary review, interoperability and cancellation proof |

## 7. Production Done

The **initial pilot** is production Done only when slices 0–2 have their release evidence, the exact Rooh-built revision is observed on the target server, broker and bridge health are monitored, costs and error rates are visible, rollback is exercised, and an owner reviews one real QA result and one real Figma candidate from Buzz.

The **AI Native organization MVP** is production Done only when slices 3–5 additionally pass on the deployed Rooh revision, including one real autonomous cross-group Figma → QA handoff and a verified bounded rework path. Until then use `pilot in progress` or `organization MVP in progress` with the exact proven boundary.
