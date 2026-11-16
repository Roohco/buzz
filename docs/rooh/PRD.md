# PRD: Rooh AI Native Organization on Buzz

Status: proposed, 2026-11-16. Product owner: Rooh. Code owner: `Roohco/buzz`. First delivery target: an internal pilot, not general availability.

## 1. Problem and desired outcome

Rooh already has specialized QA and Figma2Code/Strands agents. Their work, evidence and human decisions are distributed across project tools, so a reviewer cannot reliably see which agent acted on which version of a task, what it produced, what remains unverified, and who may authorize the next effect.

The desired outcome is a shared, searchable workspace in Buzz where people and agents can discuss work, follow its provenance, hand off bounded tasks, and review results while each project retains its tested execution harness and source of truth.

The initial pilot is demonstrated by two live, independently reviewable paths: one QA result appears once in a Buzz project channel with its exact source task and evidence; one approved Figma screen yields an explicitly unvalidated candidate with exact design/source identity. The organization MVP additionally requires one real cross-group pipeline in which the Figma agent hands that exact candidate to a QA agent, receives a revision-bound verdict, and either presents it for human review or performs one bounded rework and requests QA again. Neither milestone creates unauthorized product, payment or release effects.

## 2. Users and jobs

| User | Job to be done | Visible outcome |
| --- | --- | --- |
| Project owner | See agent activity and decide the next authorized action | One project timeline with owner, current state, result and required decision |
| QA reviewer | Inspect a finding against the original request and proof | Product bug, harness issue and unverified claim are distinguishable |
| Design/development reviewer | Inspect generated code without treating it as shipped | Candidate, pinned input, build and pending QA/review are explicit |
| Agent operator | Stop, retry or replace an agent without duplicating effects | Run identity, generation, broker receipt and recovery state are available |
| Agent | Read assigned context and report a bounded result | Its signed reply is linked to the initiating task and its evidence |

## 3. Product principles

1. Buzz is the common coordination and decision record. QA-07 owns QA execution/publication; Figma2Code owns design acquisition and generation; GitHub owns PR and merge state.
2. A channel, project or persona is a collaboration grouping, not permission to execute a tool or mutate another system.
3. A model claim is an observation until supported by a source version and inspectable evidence. `CHECKED`, `NOT_VERIFIED` and `generated_not_release_validated` retain their domain meaning.
4. A persistent agent has an identity and history; its compute process may be started only when work exists.
5. The first product slice is narrow enough to verify end to end and roll back without migrating source tasks.

## 4. Scope lock

Delivery mode: STANDARD for product scope, DEEP for the cross-system authority and data contracts. Risk: high at authority and publication boundaries; the documentation change itself is reversible.

Provisional engineering size: slice 0 **M / 3–5 working days**, QA projection **M / 3–5 days**, Figma candidate **M / 4–7 days**, assuming the existing CI and protected runner access work. Re-estimate after the real-relay proof; these ranges are not release commitments. Slice 0 is the first independently verifiable vertical delivery. Expected components are Buzz broker/ACP and relay integration tests for slice 0, QA watcher completion marker plus one bridge for slice 1, and one Figma bridge using existing commands for slice 2. The local and release gates are in [DoD](DOD.md).

The cross-group extension is a separate estimate: candidate QA capability **M / 4–7 days**, scoped peer handoff **L / 7–12 days**, bounded pipeline and repair integration **L / 5–10 days**. These depend on the pilot and are re-estimated after the candidate input and repair contracts are tested. The [technical roadmap](ROADMAP.md) gives dependency gates, not a calendar promise.

### MUST NOW: initial pilot

- A dedicated Rooh fork and one private Rooh project channel with a human owner and one separately identified broker-backed agent.
- Real-relay proof of one signed task mention, a signed reply linked to that exact message, and quiesce/drain/fence behavior after agent replacement.
- One-way projection of a completed QA-07 run with source card ID/version, original task and reference availability, finding category, report reference and deduplication key.
- One-screen Figma capability that pins runner revision and Figma input, requires full-frame confirmation, runs A1/A2/A3/G0 in its existing harness and reports a candidate as unvalidated.
- A compact result view or message template that shows state, actor, input revision, evidence, limits and next human decision.
- Local and real-relay verification of permissions, duplicate delivery, stale input, failure/retry and absence of unauthorized writes.

### SHOULD NOW

- Correlate Buzz event ID, source ID/version, run ID, broker request ID, artifact hash and trace ID in a protected run ledger. This closes the current provenance gap between signed messages and broker effects.
- Record run duration, model cost estimate, reviewer time and false-positive/invalid-input counts. These decide whether the next capability earns its complexity.

### MUST FOR THE ORGANIZATION MVP: after the initial pilot

- One private handoff channel per delivery project shared by the Figma and QA agent identities and the human owner. Each group retains its own private working channel; the handoff carries only the minimum cross-group task and evidence references.
- An independent `qa.review_candidate` capability that tests the actual isolated candidate preview against the complete screen/task input and reports a verdict tied to the candidate SHA. Existing QA-07 landing checks cannot be relabeled as this capability.
- An explicit broker grant that allows the Figma agent to request only that QA capability. The QA agent accepts or rejects the signed request; an ungranted peer cannot trigger a run.
- One `pipeline_id` joining Figma generation, QA review, optional rework and final human decision. A new candidate invalidates the previous candidate's QA verdict. At most one automated rework is allowed before a human decides.

### NOT NOW

- Replace Trello, Notion, Slack, GitHub or project harnesses as systems of record.
- A generic multi-agent scheduler, arbitrary peer delegation, autonomous product repository writes, PR merge, deploy, payment mutation or access grants.
- A second QA queue, full model transcripts in Buzz, long-term memory copied from private evidence, or automatic status writeback to source tools.
- A2A federation, global Strands Swarm, speculative multi-region control plane and voice-triggered effects. Voice can later present and draft the same scoped actions.

## 5. Functional requirements

| ID | Requirement | Acceptance observation |
| --- | --- | --- |
| FR-01 | Human owner and named agents operate in a private Rooh project channel | Unauthorized participant cannot read or post; each agent signs under its own identity |
| FR-02 | A task and its reply have a durable causal link | Reviewer opens the reply from the initiating signed message and sees the same run ID in both |
| FR-03 | QA evidence is projected once, from an existing completed QA-07 run | Replaying the same card version and run produces one Buzz review item; the bridge initiates no browser, Notion or Slack side effect |
| FR-04 | QA input completeness is visible | Missing original task, attachments or reference is `input_incomplete` or `NOT_VERIFIED`, never an unsupported failure verdict |
| FR-05 | Figma run preserves source identity and review gate | A1 full-frame confirmation precedes A2/A3/G0; mismatch or `BLOCKED` stops the run |
| FR-06 | Figma candidate is labeled honestly | G0 generation-only exit 2 records `generated_not_release_validated`, with browser, visual and release checks `not_run` |
| FR-07 | Significant effects require the right authority | Buzz membership alone cannot trigger payment, product write, merge, deploy or access grant |
| FR-08 | Operator can see and recover a stopped run | Generation fence prevents an old body from starting new broker effects; unresolved effects are surfaced for reconciliation |
| FR-09 | Work crosses Figma and QA groups without exposing internal group channels | A project handoff channel shows one pipeline ID, exact parent/child task links, candidate SHA, preview bundle digest and protected evidence references |
| FR-10 | Figma agent can request a bounded QA review | QA agent accepts/rejects a signed `qa.review_candidate` task for that exact candidate and bundle; a non-granted peer or wrong identity is denied before browser work |
| FR-11 | QA independently checks the actual candidate | QA verifies that the browser-served bundle matches the pinned candidate/build manifest, runs generated code without access to credentials, protected references, host services or arbitrary egress, and reviews against complete design/task context; QA-07's landing verdict is not substituted |
| FR-12 | Defect feedback closes a bounded loop | A defect can cause one repaired candidate linked to the prior SHA and QA evidence, then one new QA run; previous QA pass becomes stale, a second defect stops for a human, and only a human authorizes release |

## 6. User journeys

### QA review

QA-07 observes an eligible task in its existing source, runs within its existing limits, writes its Notion report, then notifies Slack through its current sequence. A Rooh adapter reads the completed receipt and publishes a minimal signed Buzz result linked to the exact card version and run. The reviewer follows the protected report reference, classifies findings and decides whether a product issue needs a human-owned follow-up. Buzz does not issue a second QA run in this slice.

### Figma candidate

An owner selects one screen and confirms the complete frame after A1. The adapter invokes the existing A2/A3/G0 commands with pinned source and bounded budget. Buzz shows the candidate identity, receipt hashes, build result and pending visual/product review. The owner may assign a separate delivery task only after reviewing the candidate and establishing the product-write contract.

### Agent interaction proof

An owner posts a task mention in a private channel. The keyless agent reads through the broker, posts a signed reply to the exact task and then is stopped/replaced. The operator proves that the old generation cannot start a new effect and reconciles any pending effect before successor work.

### Unified Figma → QA → review pipeline

The Figma agent completes its own anchored A1/A2/A3/G0 run and posts a minimal candidate receipt to the shared project handoff channel. Its scoped grant permits a `qa.review_candidate` request for that exact candidate. The QA agent verifies the immutable preview bundle and the bytes served to its isolated browser, compares them with complete design/task input held outside the generated page, and posts a signed result with evidence. If QA finds a defect, the Figma agent may generate one revised candidate and request a fresh QA run; the old QA result cannot certify the new SHA. After a pass or a second defect, the owner decides the next product action. Internal Figma and QA group discussions stay in their own channels.

## 7. Nonfunctional requirements

- Privacy: Buzz contains minimal metadata and protected artifact references; no raw credentials, customer records, model transcripts or private Figma payloads.
- Security: separate agent identities, explicit channel scope, broker-mediated signing, action-level capability checks and human approval at effect boundaries.
- Integrity: input and artifact hashes, pinned source revisions, idempotency by source version and run, explicit uncertain-effect state.
- Cost: per-run time/model limits; no always-on model loop. Operators can stop a run without losing its durable identity.
- Availability: source systems continue functioning if Buzz is unavailable; adapters retry publication without re-executing the underlying work.
- Observability: one trace/correlation chain from source task through harness, broker effect and Buzz message.

## 8. Metrics and decision gates

The initial pilot passes when one real QA run and one real Figma screen meet FR-01–FR-08, with zero unauthorized external effects and no duplicate source publication. The organization MVP passes only after FR-09–FR-12 are demonstrated by a real candidate-to-QA handoff, plus a deterministic defect/rework test. Record reviewed QA precision, time from source result to review, cross-group handoff latency, candidate review time, cost/run, blocked/input-incomplete rate and recovery time. Do not claim organization-wide productivity gain from synthetic or local-only proof.

Expand to scoped peer delegation and a job board only after the first two paths are stable, reviewer value is demonstrated, and the missing Buzz job emitter and peer authorization have separate acceptance tests. Durable workflow approvals are required before using Buzz itself as a release gate; the organization MVP can continue to use the existing human GitHub/release gate.

## 9. Dependencies and open decisions

- The current Rooh fork is public because its source fork is public; sensitive operations and evidence remain outside this repository.
- A full post-release QA card-to-Notion-to-Slack run is needed before measuring mirror quality.
- Figma2Code `develop` and deployed/local runner revisions may differ; the pilot must pin the executable and record its SHA.
- Buzz broker/agent production deployment, CI and exact relay compatibility are prerequisites to a production pilot.
- Source task system remains unchanged in the first slice. Any later migration requires a separate PRD and data migration plan.

## 10. References

- [Buzz product vision](../../VISION.md), [project vision](../../VISION_PROJECTS.md), [remote-agent vision](../../VISION_REMOTE_AGENTS.md). These describe direction, not release evidence.
- [Technical architecture](ARCHITECTURE.md) and [DoD](DOD.md) define the implementation boundary and proof.
