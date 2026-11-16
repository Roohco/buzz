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

- [ ] A real eligible QA-07 card completes the existing card → receipt → Notion attempt → Slack acknowledgement path on the intended QA revision; the final watcher state and actual Notion/Slack outcomes are recorded.
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
- [ ] Mismatched identity, missing confirmation, `BLOCKED`, timeout and missing artifact each stop further execution with an inspectable reason.
- [ ] Product repository contents, PRs and deployment are unchanged by this slice.

### Release evidence

Attach protected A1/A2/A3/G0 receipts and hashes, confirmation tied to the exact manifest, candidate git-tree/source SHA, budget result, negative-test results and reviewer decision. A candidate alone is not a production feature.

## 5. Later organization capabilities

The following are separate delivery slices, not implied by the first three:

| Capability | Additional DoD before activation |
| --- | --- |
| Typed job board | Request publisher, authorized consumer, accepted/progress/result/cancel handling, replay/cancel tests and user-visible status agree on one job ID |
| Agent-to-agent delegation | Explicit peer capability and source metadata, bounded depth/breadth, cycle/timeout limits and negative test for ungranted peer |
| Durable approvals | Approval token and exact revision persist across restart; executor resumes once; rejection and stale-revision tests pass |
| Product writes / PR creation | Repository-specific assignment, isolated target checkout, existing project gates, human review and reversible rollback |
| Merge / deploy | Existing GitHub/release authority confirms exact artifact; Buzz never treats a chat message or model claim as deployment proof |
| Voice action | Transcript/request is bound to a named human, exact capability, target revision and explicit confirmation; same permission tests as text path |
| External A2A | Named external consumer, authentication and data-boundary review, interoperability and cancellation proof |

## 6. Production Done for the Rooh pilot

The pilot is production Done only when slices 0–2 have their release evidence, the exact Rooh-built revision is observed on the target server, broker and bridge health are monitored, costs and error rates are visible, rollback is exercised, and an owner reviews one real QA result and one real Figma candidate from Buzz. Until then use `pilot in progress` with the exact proven boundary.
