# Rooh Buzz pilot deployment

Status, 2026-11-17: **pilot in progress.** Relay and two broker signers are running on the team host; channels, agent identities and the R5 grant exist; broker smoke passed for both agents. The live R2 gate (one real QA-07 card result in Buzz) waits for the next natural QA-07 completion after the bridge cron wrapper is merged and deployed (see [Pending](#pending)).

This is a public repository: the document names no host, credential or private payload. Pubkeys, image digests and loopback ports are public by nature. The exact host, channel UUIDs and grant IDs live in the operator README on the host (`~/rooh-buzz/README.md`, `channels.env`).

## What runs

Compose project `rooh-buzz` on one shared Linux host, its own network `rooh-buzz_buzz-net` and volumes `rooh-buzz_*`. Everything binds `127.0.0.1` only; there is no reverse proxy and no public port. Every client uses `127.0.0.1:3130`, never `localhost`: the relay selects the community by `Host`, and the broker canonicalizes `localhost` to `127.0.0.1`.

| Service | Image (pinned by digest) | Revision | Listens |
| --- | --- | --- | --- |
| relay | `ghcr.io/roohco/buzz@sha256:df84f2635ccf2c45925931609671320c6c52a4b87ac33bd6e8986a67617eda7d` | `d50079403` (R5 job kinds) | `127.0.0.1:3130` HTTP/WS |
| broker (QA-07 signer) | `ghcr.io/roohco/buzz-broker-host@sha256:6988cb86f4c0e26a7f6300964299d21c5f36227ebc639fedc5d118c1f7468ce8` | `fe284dd33` (R5 grants, `job.outcome`, `channel.read`) | `127.0.0.1:3140` `/v1/action` |
| broker-figma (Figma2Code signer) | same digest | `fe284dd33` | `127.0.0.1:3141` `/v1/action` |
| broker rollback (N-1, not running) | `ghcr.io/roohco/buzz-broker-host@sha256:26725cde8c0f55368d745f57a456f94a2ecb3e30c006794daba4b6e3550f6819` | `98e36c66d` | — |
| postgres / redis / silo | as pinned in `deploy/compose/compose.yml` | — | internal network only |

Relay settings: `BUZZ_REQUIRE_AUTH_TOKEN=true` (REST needs NIP-98), `BUZZ_REQUIRE_RELAY_MEMBERSHIP=true`, `BUZZ_ALLOW_NIP_OA_AUTH=true` (an agent with a valid owner attestation from a relay member is admitted without its own membership row).

### Why two broker processes

One `serve` holds exactly one signer (agent key + owner auth tag) and refuses bearers whose `executorAgentPubkey` differs. The pilot has two agents, so it runs two `serve` processes on one shared authority SQLite state, the same topology as the R5 real-relay proof (`crates/buzz-broker-host/tests/real_relay_e2e/r5_peer.rs`). The shared state holds both generations, the effect ledger and the peer grant. Both containers use `network_mode: host` (`serve` refuses non-loopback binds), run as UID 1000 with `cap_drop: ALL` and `no-new-privileges`, and each mounts only its own signer directory read-only.

| Agent | Role | Broker | Uses |
| --- | --- | --- | --- |
| QA-07 | R2 result publisher; R4/R6 QA recipient (`job.admit`, `job.result`) | `:3140` | `message.post`/`message.reply`, job recipient actions |
| Figma2Code | R3 candidate poster; R5/R6 requester (`job.request`, `job.outcome`) | `:3141` | `message.post`, job sender actions |

## Identities (public keys only)

| Identity | Pubkey | Notes |
| --- | --- | --- |
| Pilot operator (`RELAY_OWNER_PUBKEY`, NIP-OA owner for now) | `cc9450f6580aca6afa8466f5315a3e8dc9d20710b623afa187408b8768e8b7ac` | Generated on the host by `buzz-admin generate-key`. Stands in for the human owner until the owner's key arrives. |
| QA-07 agent | `785b9ccc6cd88a454503fdafa3a20bf62889914008bbb550704c9ce8fcbbacfb` | Generation `3dda9543-bde1-49f9-9992-e85789f7e233` |
| Figma2Code agent | `e2cee768b8e65a2485a898c99b688e15619cfd8d823181e78a0fcb8e36890a4d` | Generation `92b922df-1723-40c7-b459-5c90a4c830e6` |

Owner attestations (NIP-OA, `buzz_sdk::nip_oa` format) for both agents carry **no kind restriction** (job kinds are not kind 9) and the expiry `created_at<1802649600` (**2027-02-15T00:00:00Z**), as ARCHITECTURE.md requires for handoff agents. Reissue before that date (see [Rotate](#rotate-an-agent-key-or-attestation)). Each `serve` re-verifies its attestation with `buzz_sdk::nip_oa::verify_auth_tag` at start; both started healthy, and relay copies of broker-signed events carry an `auth` tag that verifies against the operator key.

Keys never leave the host: agent keys and auth tags are 0600 files under `broker/signing/<agent>/` (dir 0700); bearers are 0600 under `broker/credential/`. The only copy of a bearer is the QA-07 one, delivered by file to the QA-07 deployment on the same host (`~/rooh-qa07/secrets/buzz-broker.bearer`, 0600).

## Channels and grant

All channels are private streams created by the pilot operator (kind 9007, then kind 9000 per member). The operator is their owner/admin.

| Channel | Members besides the operator | Purpose |
| --- | --- | --- |
| `qa-results` | QA-07 | R2: one reply per completed QA-07 card run to the operator task event in this channel |
| `figma-candidates` | Figma2Code | R3: pinned Figma candidates for human review |
| `figma-qa` | Figma2Code, QA-07 | R5/R6: scoped Figma → QA handoff |

R5 grant (in broker state, `peer-status` shows it): sender = Figma2Code generation `92b922df-…`, recipient = QA-07 pubkey, channel = `figma-qa`, capability `qa.review_candidate`, state `active`. No other grant exists, so any other sender, recipient, channel or capability is denied by the broker.

## Evidence (2026-11-17)

- `./health.sh`: relay liveness/readiness 200, relay/postgres/redis/silo healthy, `broker` and `broker-figma` healthy (`/v1/action` answers 405 to GET).
- Broker smoke, `message.post` through each signer: QA-07 → `qa-results` event `62cccc58a740524db70f024d0ddb1f5061a173c0fa86849f804a8aba47dbaec7`; Figma2Code → `figma-candidates` event `7873b2e88095f828fb5e86ce7d5990c2e76942340fee1ea4d2ca9294f363bfa3`. Both read back from the relay with `/query`: kind 9, author = the agent pubkey, event signature valid, NIP-OA `auth` tag valid for owner `cc9450f6…` with the expiry above. Resending the identical QA-07 request returned the same event id with `replayed: true`.
- QA-07 bridge reachability: the deployed QA Agent client (`Roohco/qa-agent` main `27c4543`) in a host-network container returned `authority.status` = `active` for the QA-07 generation.
- R2 task event (operator, kind 9 in `qa-results`): `d60018e9239744ec9a06819679df2a9ee1caeaad792874e640d1c497497a4871`. Bridge results are `message.reply` to it.

## Operate

On the host, from `~/rooh-buzz` (wrapper `./bz` = `docker compose -p rooh-buzz` with the pinned env files):

```bash
./deploy.sh                                              # relay stack up --wait + health
./bz --profile broker up -d --wait broker broker-figma   # both signers
./health.sh
./bz --profile broker logs --tail 100 broker broker-figma
B='./bz --profile broker run --rm --no-deps broker --state /broker/state/authority.db'
$B peer-status
```

Never `./bz down -v` (deletes the pilot volumes). The host is shared with other production services: touch only `rooh-buzz*` containers, volumes and networks; stop processes by exact container or PID.

Operator helpers on the host (not in this repository): `pt` (NIP-OA tag, operator events and NIP-98 `/query` with signature and NIP-OA verification, run in a throwaway `node:22` container on the host network; never prints keys), `setup-agent.sh <name> <expiry>` (key via `buzz-admin generate-key`, auth tag, authority JSON, `issue`), `broker-call.sh <agent> <request.json>` (one broker action; bearer never in argv or output).

### Fence a generation

```bash
$B quiesce --authority-file /broker/authority/<agent>-g1.json
$B drain   --authority-file /broker/authority/<agent>-g1.json \
           --signing-key-file /broker/signing/agent.key --auth-tag-file /broker/signing/owner.auth-tag
$B fence   --authority-file /broker/authority/<agent>-g1.json
```

`drain` needs the agent's own signer, so run all three through the matching service: `broker` for QA-07, `broker-figma` for Figma2Code (replace `broker` with `broker-figma` in `$B`). Each service mounts its own `broker/signing/<agent>` at `/broker/signing`. A fenced generation stays dead. A successor needs a new authority JSON (new `taskId`/`generation`) and a new bearer file; issue never overwrites an existing credential path. Re-issuing the Figma2Code generation also needs a new peer grant (grants bind the exact sender generation).

### Rotate an agent key or attestation

1. Fence the agent's generation (above).
2. Move `broker/signing/<agent>` aside, run `setup-agent.sh <agent> <new-expiry>` (new key, new `owner.auth-tag`, new authority, new bearer; rename the `-g1` files first or extend the script to a new generation suffix).
3. Add the new pubkey to the agent's channels (kind 9000 by the operator) and remove the old one (kind 9001).
4. Restart the matching broker service; deliver the new QA-07 bearer to the QA-07 deployment by file (0600).

To re-attest only (same key, new expiry), replace `owner.auth-tag` and restart that broker service; bearers and generations stay valid.

### Revoke the R5 grant

```bash
$B grant-revoke --grant-id <grantId>    # new requests under it are denied
```

### Rollback

- Broker to N-1: quiesce, drain and fence the active generations, set `BROKER_IMAGE` to the rollback digest above in `images.env`, then `./bz --profile broker up -d --wait broker broker-figma`. N-1 (`98e36c66d`) predates R5 grants and job outcome: the R5/R6 path is unavailable on it; R2 posting works.
- Relay: set `BUZZ_IMAGE` back to the previous digest and `./bz up -d --wait relay`. Migrations are forward-only; back up first.
- Switch R2 off without touching Buzz: remove `~/rooh-qa07/buzz-bridge.env` (or the bearer) on the host; the bridge pass exits 0 without them.

### Backup

`pg_dump -Fc` of the relay database, a copy of `authority.db` **and its `-wal` file** taken with both brokers stopped (the broker does not checkpoint on exit, so the pair is the consistent state), and a tarball of `secrets/`, `broker/{signing,authority,grants,credential}` and the channel/task files. All stay on the host in `backups/` (0600). The first post-setup backup was taken on 2026-11-17 after the grant and smoke.

## Owner onboarding

1. Tunnel the relay: `ssh -N -L 3130:127.0.0.1:3130 <pilot-host>` (host name in the operator README).
2. Buzz Desktop relay URL: `ws://127.0.0.1:3130` (not `localhost`).
3. Send the operator your Buzz pubkey (hex or npub). The operator runs
   `./bz exec relay buzz-admin add-member --pubkey <hex> --role admin`
   and adds you to `qa-results`, `figma-candidates` and `figma-qa` (kind 9000 from the operator key).

## Pending

| Item | Who | Blocks |
| --- | --- | --- |
| Merge and deploy `Roohco/qa-agent` PR #65 (bridge cron wrapper; the cron line is installed on the host and inert until the script exists) | reviewer | R2 automation |
| One natural QA-07 card completion on the deployed revision after that | QA-07 traffic | R2 live gate |
| Owner Buzz pubkey: add as relay admin and channel member | owner | owner review in Buzz Desktop |
| Re-attest both agents under the owner's key instead of the pilot operator key (new `owner.auth-tag`, broker restart) | operator, after owner key | removes the stand-in owner |
| Reissue attestations before 2027-02-15 | operator | agent publishing after expiry |
| Monitoring/alerting for `health.sh` (not installed in cron) | operator | DoD "broker and bridge health are monitored" |
