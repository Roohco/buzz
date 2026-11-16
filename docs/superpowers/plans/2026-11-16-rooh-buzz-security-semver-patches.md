# Rooh Buzz security lockfile patch slice

## Scope lock

- **Problem and observable outcome:** Rooh `main` at `61eb6c3` has 32 RustSec findings in its Security job. Seven resolved crate names (`crypto-bigint`, the three `hickory-*` crates, `negentropy`, `pulldown-cmark`, `webpki-root-certs`) have published compatible patch releases. Update their resolved versions and prove the corresponding findings disappear without changing application code.
- **MUST NOW:** Update only those compatible Cargo.lock resolutions; record exact before/after versions and advisory IDs; run a local security gate and the repository's complete `just ci` gate; publish one reviewable PR and inspect its exact remote checks.
- **NOT NOW:** The `rmcp` and `zip` major upgrades, `libsqlite3-sys`/SQLx constraint, `unsafe-libyaml` replacement, R1 relay logic, Compose/MinIO replacement, and any policy suppression. Those need separate compatibility or scope decisions.
- **Acceptance:** `Cargo.lock` resolves non-yanked `crypto-bigint 0.7.11`, `hickory-net/proto/resolver >=0.26.2`, `negentropy >=0.5.2`, `pulldown-cmark >=0.13.6`, and `webpki-root-certs >=1.0.10`; a fresh local advisory scan no longer reports the IDs attributed to these versions; `just ci` passes at the committed revision.
- **Production Done:** A reviewed Rooh PR is merged, its exact main SHA is verified, and the Security job on that SHA confirms the expected advisory reduction. Remaining advisories are reported explicitly; this slice does not claim a clean security baseline.
- **Smallest correct option:** Resolve compatible patches in the lockfile, with no new dependency or source code. Expected changed files: this plan and `Cargo.lock`. One vertical slice: a safer resolved dependency graph consumed by the Rooh build.
- **Estimate:** S, 2–4 hours including the full local gate and remote review. Checkpoint if a version cannot resolve compatibly, source edits become necessary, file scope doubles, or time exceeds six hours.
- **Local final gate:** Hermit activated, fresh `cargo deny check advisories` (or the repository's equivalent security command) and `just ci`. **Remote target:** PR checks and exact merged `Roohco/buzz` main revision.

## Evidence and implementation order

1. Pin the 32-ID baseline from the Security run on `61eb6c3`; identify each target crate's current resolved version and its patched range from RustSec. A human would need this same package/advisory map to judge the change; the CI log, lockfile and official advisories provide it without dropping structure.
2. Update the seven compatible resolved packages, with the Hickory trio coordinated at one release version. Inspect `Cargo.lock` for unexpected dependency churn; stop if the update needs an API or trust-boundary change.
   RustSec's first fixed `crypto-bigint` version, 0.7.6, is yanked in the Cargo index; the selected non-yanked compatible release is 0.7.11. The first `pulldown-cmark` fix, 0.13.5, leaves RUSTSEC-2026-0433 open, so this slice uses 0.13.6.
3. Run the cheapest distinguishing security scan first, comparing the target IDs against the baseline. Then run the full local gate before the first push.
4. Commit with DCO, request review, push once, inspect all terminal CI/review findings, then make at most one batched correction if needed. Verify the exact remote SHA after merge.

## Failure criteria

The slice fails if any target advisory remains, the lockfile introduces an incompatible major release, the full local gate fails due to this update, or remote evidence cannot be tied to the exact reviewed SHA. An infrastructure-only CI failure is reported separately and retried narrowly where available.
