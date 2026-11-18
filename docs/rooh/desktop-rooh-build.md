# Buzz Rooh desktop build

A separate desktop build for Rooh, installed next to the stock Buzz app without
sharing state with it.

Why: one stock Buzz Desktop install shares a single owner key, one local agent
library and one agent nest (`~/.buzz`) across every community it joins. Joining
a Rooh community from a stock install therefore mirrors every persona, team and
agent config of the other communities into the Rooh relay, and agents without a
relay binding connect to whichever community is open.

The flavored build changes, at build time only:

| | Stock | Buzz Rooh |
| --- | --- | --- |
| Bundle identifier / app data dir | `xyz.block.buzz.app` | `co.rooh.buzz.desktop` |
| App name | Buzz | Buzz Rooh |
| OS keyring service (owner key, agent keys) | `buzz-desktop` | `buzz-desktop.rooh` |
| Agent nest | `~/.buzz` | `~/.buzz-rooh` |
| Deep-link scheme | `buzz://` | `buzz-rooh://` |
| Updater | (release config) | disabled (no endpoints) |
| `~/.local/bin/buzz` CLI link | set | not touched |
| Speech models | `~/.buzz/models` | `~/.buzz-rooh/models` |

Rooh agents run server-side behind the broker, so this build ships with stub
sidecars and does not run local agents.

## Build (macOS, Apple Silicon)

Do not set `BUZZ_UPDATER_PUBLIC_KEY` / `BUZZ_UPDATER_ENDPOINT` for this build:
they compile the updater in and would point it at stock releases.

```bash
. ./bin/activate-hermit
TARGET=aarch64-apple-darwin
mkdir -p desktop/src-tauri/binaries
for b in buzz-acp buzz-agent buzz-backend-kubernetes buzz-broker-host buzz-dev-mcp git-credential-nostr buzz; do
  touch "desktop/src-tauri/binaries/$b-$TARGET"
done
pnpm install
cd desktop
BUZZ_DESKTOP_FLAVOR=rooh pnpm tauri build \
  --config src-tauri/tauri.rooh.conf.json --target "$TARGET" --bundles app
```

The app lands in
`desktop/src-tauri/target/$TARGET/release/bundle/macos/Buzz Rooh.app`. It is
unsigned: copy it to `~/Applications` and open it once with right-click → Open.
On first launch it creates a new owner key in its own keyring service; add that
key's npub to the Rooh relay as a member.
