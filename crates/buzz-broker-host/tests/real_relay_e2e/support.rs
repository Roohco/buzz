//! Relay proxy, relay/broker drivers and evidence timeline for the R1 proof.

use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::process::{Output, Stdio};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use buzz_broker_client::HttpBrokerClient;
use buzz_sdk::broker::{
    ActionArgs, ActionOutcome, AuthorityIdentity, BrokerClientExt as _, BrokerErrorCode,
    BrokerRequest, BrokerResult, MessagePostArgs, PreparedRequest,
};
use nostr::{Event, EventBuilder, JsonUtil as _, Keys, Kind, Tag};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

pub(crate) const PASS: u8 = 0;
pub(crate) const DROP_RESPONSES: u8 = 1;
pub(crate) const REFUSE: u8 = 2;

// ── Relay proxy ─────────────────────────────────────────────────────────────

/// Plain TCP relay in front of the real relay, used as the broker's
/// `HTTP_PROXY`. Absolute-form HTTP requests keep the original `Host`, so the
/// relay's tenant binding and NIP-98 URL checks see the real community URL.
pub(crate) struct RelayProxy {
    pub(crate) address: SocketAddr,
    pub(crate) mode: Arc<AtomicU8>,
    pub(crate) task: tokio::task::JoinHandle<()>,
}

impl RelayProxy {
    pub(crate) async fn start(upstream: String) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("proxy listener");
        let address = listener.local_addr().expect("proxy address");
        let mode = Arc::new(AtomicU8::new(PASS));
        let accept_mode = Arc::clone(&mode);
        let task = tokio::spawn(async move {
            loop {
                let Ok((client, _)) = listener.accept().await else {
                    return;
                };
                if accept_mode.load(Ordering::SeqCst) == REFUSE {
                    drop(client);
                    continue;
                }
                let mode = Arc::clone(&accept_mode);
                let upstream = upstream.clone();
                tokio::spawn(async move {
                    let server = match tokio::net::TcpStream::connect(&upstream).await {
                        Ok(server) => server,
                        Err(error) => {
                            eprintln!("[r1-proxy] upstream connect failed: {error}");
                            return;
                        }
                    };
                    let (mut client_read, mut client_write) = client.into_split();
                    let (mut server_read, mut server_write) = server.into_split();
                    let up_mode = Arc::clone(&mode);
                    let up = async move {
                        let mut buffer = vec![0_u8; 16 * 1024];
                        loop {
                            let read = client_read.read(&mut buffer).await.unwrap_or(0);
                            if read == 0 || up_mode.load(Ordering::SeqCst) == REFUSE {
                                return;
                            }
                            if server_write.write_all(&buffer[..read]).await.is_err() {
                                return;
                            }
                        }
                    };
                    let down = async move {
                        let mut buffer = vec![0_u8; 16 * 1024];
                        loop {
                            let read = server_read.read(&mut buffer).await.unwrap_or(0);
                            // Any non-pass mode closes the client before it sees
                            // the relay's answer: the request already reached the
                            // relay, the ACK is lost.
                            if read == 0 || mode.load(Ordering::SeqCst) != PASS {
                                return;
                            }
                            if client_write.write_all(&buffer[..read]).await.is_err() {
                                return;
                            }
                        }
                    };
                    tokio::select! {
                        () = up => {}
                        () = down => {}
                    }
                });
            }
        });
        Self {
            address,
            mode,
            task,
        }
    }

    pub(crate) fn set(&self, mode: u8) {
        self.mode.store(mode, Ordering::SeqCst);
    }

    pub(crate) fn url(&self) -> String {
        format!("http://{}", self.address)
    }
}

impl Drop for RelayProxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

// ── Direct relay access for owner and outsider ─────────────────────────────

pub(crate) struct Relay {
    pub(crate) ws_url: String,
    pub(crate) http_base: String,
    pub(crate) http: reqwest::Client,
}

impl Relay {
    /// The broker addresses the relay by the authority's canonical community
    /// URL, which maps every loopback spelling to `127.0.0.1`. The relay binds
    /// tenants by `Host`, so owner and outsider use the same canonical URL and
    /// the proof community must be mapped for that host.
    pub(crate) fn from_env() -> Self {
        let raw = std::env::var("RELAY_URL").expect("RELAY_URL of a real buzz-relay");
        let ws_url = buzz_core::relay::normalize_relay_url(&raw).expect("RELAY_URL is a relay URL");
        let http_base = if let Some(rest) = ws_url.strip_prefix("ws://") {
            format!("http://{rest}")
        } else if let Some(rest) = ws_url.strip_prefix("wss://") {
            format!("https://{rest}")
        } else {
            panic!("RELAY_URL must be ws:// or wss://");
        };
        let http = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(20))
            .build()
            .expect("relay HTTP client");
        Self {
            ws_url,
            http_base,
            http,
        }
    }

    pub(crate) fn authority(&self) -> String {
        let url = reqwest::Url::parse(&self.http_base).expect("relay HTTP URL");
        let host = url.host_str().expect("relay host");
        match url.port_or_known_default() {
            Some(port) => format!("{host}:{port}"),
            None => host.to_owned(),
        }
    }

    pub(crate) fn nip98(keys: &Keys, url: &str, body: &[u8]) -> String {
        let payload = hex::encode(Sha256::digest(body));
        let auth = EventBuilder::new(Kind::Custom(27235), "")
            .tags([
                Tag::parse(["u", url]).unwrap(),
                Tag::parse(["method", "POST"]).unwrap(),
                Tag::parse(["nonce", &uuid::Uuid::new_v4().to_string()]).unwrap(),
                Tag::parse(["payload", &payload]).unwrap(),
            ])
            .sign_with_keys(keys)
            .expect("sign NIP-98");
        format!(
            "Nostr {}",
            base64::engine::general_purpose::STANDARD.encode(auth.as_json().as_bytes())
        )
    }

    pub(crate) async fn post(&self, keys: &Keys, path: &str, body: Vec<u8>) -> (u16, Vec<u8>) {
        let url = format!("{}{path}", self.http_base);
        let response = self
            .http
            .post(&url)
            .header("authorization", Self::nip98(keys, &url, &body))
            .header("content-type", "application/json")
            .body(body)
            .send()
            .await
            .unwrap_or_else(|error| panic!("relay {path} unreachable: {error}"));
        let status = response.status().as_u16();
        let bytes = response.bytes().await.unwrap_or_default().to_vec();
        (status, bytes)
    }

    /// Submit an event; returns (accepted, relay message).
    pub(crate) async fn submit(&self, keys: &Keys, event: &Event) -> (bool, String) {
        let (status, body) = self
            .post(keys, "/events", serde_json::to_vec(event).unwrap())
            .await;
        let value: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        let accepted = (200..300).contains(&status)
            && value["accepted"].as_bool() == Some(true)
            && value["event_id"].as_str() == Some(event.id.to_hex().as_str());
        let message = value["message"]
            .as_str()
            .or_else(|| value["error"].as_str())
            .unwrap_or_default()
            .to_owned();
        (accepted, message)
    }

    /// Authenticated `/query`; non-success statuses return no events.
    pub(crate) async fn query(&self, keys: &Keys, filter: Value) -> (u16, Vec<Event>) {
        let (status, body) = self
            .post(
                keys,
                "/query",
                serde_json::to_vec(&json!([filter])).unwrap(),
            )
            .await;
        if !(200..300).contains(&status) {
            return (status, Vec::new());
        }
        let events: Vec<Event> =
            serde_json::from_slice(&body).expect("relay /query returns events");
        (status, events)
    }
}

// ── Broker host binary ──────────────────────────────────────────────────────

pub(crate) struct BrokerHost {
    pub(crate) directory: tempfile::TempDir,
    pub(crate) state: PathBuf,
    pub(crate) key_file: PathBuf,
    pub(crate) tag_file: PathBuf,
    pub(crate) proxy_url: String,
}

impl BrokerHost {
    pub(crate) fn new(agent: &Keys, auth_tag: &str, proxy_url: String) -> Self {
        let directory = tempfile::tempdir().expect("broker directory");
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))
            .expect("private broker directory");
        let key_file = directory.path().join("agent.key");
        let tag_file = directory.path().join("owner.auth-tag");
        std::fs::write(&key_file, agent.secret_key().to_secret_hex()).unwrap();
        std::fs::write(&tag_file, auth_tag).unwrap();
        for path in [&key_file, &tag_file] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        Self {
            state: directory.path().join("authority.db"),
            directory,
            key_file,
            tag_file,
            proxy_url,
        }
    }

    pub(crate) fn command(&self) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_buzz-broker-host"));
        command
            .arg("--state")
            .arg(&self.state)
            .env("HTTP_PROXY", &self.proxy_url)
            .env("http_proxy", &self.proxy_url)
            .env_remove("HTTPS_PROXY")
            .env_remove("https_proxy")
            .env_remove("ALL_PROXY")
            .env_remove("all_proxy")
            .env_remove("NO_PROXY")
            .env_remove("no_proxy")
            .env_remove("BUZZ_PRIVATE_KEY")
            .stdin(Stdio::null())
            .kill_on_drop(true);
        command
    }

    pub(crate) fn authority_file(&self, identity: &AuthorityIdentity) -> PathBuf {
        let path = self
            .directory
            .path()
            .join(format!("authority-{}.json", identity.generation));
        std::fs::write(&path, serde_json::to_vec(identity).unwrap()).unwrap();
        path
    }

    pub(crate) async fn run(&self, args: &[&std::ffi::OsStr]) -> Output {
        self.command()
            .args(args)
            .output()
            .await
            .expect("broker CLI runs")
    }

    /// Issue one generation; returns the bearer or the CLI error text.
    pub(crate) async fn issue(&self, identity: &AuthorityIdentity) -> Result<String, String> {
        let authority = self.authority_file(identity);
        let credential = self.directory.path().join(format!(
            "credential-{}-{}",
            identity.generation,
            uuid::Uuid::new_v4()
        ));
        let output = self
            .run(&[
                "issue".as_ref(),
                "--authority-file".as_ref(),
                authority.as_os_str(),
                "--credential-output".as_ref(),
                credential.as_os_str(),
            ])
            .await;
        if output.status.success() {
            Ok(std::fs::read_to_string(credential).expect("issued credential"))
        } else {
            Err(String::from_utf8_lossy(&output.stderr).into_owned())
        }
    }

    pub(crate) async fn lifecycle(
        &self,
        verb: &str,
        identity: &AuthorityIdentity,
    ) -> Result<String, String> {
        let authority = self.authority_file(identity);
        let output = self
            .run(&[
                verb.as_ref(),
                "--authority-file".as_ref(),
                authority.as_os_str(),
            ])
            .await;
        cli_result(&output)
    }

    pub(crate) async fn drain(&self, identity: &AuthorityIdentity) -> Result<String, String> {
        let authority = self.authority_file(identity);
        let output = self
            .run(&[
                "drain".as_ref(),
                "--authority-file".as_ref(),
                authority.as_os_str(),
                "--signing-key-file".as_ref(),
                self.key_file.as_os_str(),
                "--auth-tag-file".as_ref(),
                self.tag_file.as_os_str(),
            ])
            .await;
        cli_result(&output)
    }

    pub(crate) async fn serve(&self) -> (tokio::process::Child, String) {
        let port = {
            let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("free port");
            probe.local_addr().unwrap().port()
        };
        let log = std::fs::File::create(self.directory.path().join("serve.log")).unwrap();
        let child = self
            .command()
            .args([
                "serve".as_ref(),
                "--bind".as_ref(),
                std::ffi::OsStr::new(&format!("127.0.0.1:{port}")),
                "--signing-key-file".as_ref(),
                self.key_file.as_os_str(),
                "--auth-tag-file".as_ref(),
                self.tag_file.as_os_str(),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::from(log))
            .spawn()
            .expect("broker serve starts");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        loop {
            if tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .is_ok()
            {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "broker host did not listen: {}",
                std::fs::read_to_string(self.directory.path().join("serve.log"))
                    .unwrap_or_default()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        (child, format!("http://127.0.0.1:{port}"))
    }
}

pub(crate) fn cli_result(output: &Output) -> Result<String, String> {
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

// ── Evidence timeline ───────────────────────────────────────────────────────

#[derive(Serialize, Default)]
pub(crate) struct Step {
    pub(crate) step: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) generation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) action: Option<&'static str>,
    pub(crate) outcome: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) replayed: Option<bool>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) event_ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) relay_copies: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) signatures_verified: Option<bool>,
}

#[derive(Default)]
pub(crate) struct Timeline {
    pub(crate) steps: Vec<Step>,
    pub(crate) verified_event_ids: Vec<String>,
}

impl Timeline {
    pub(crate) fn push(&mut self, step: Step) {
        eprintln!(
            "[r1] {} {} {}",
            step.step,
            step.outcome,
            step.event_ids.join(",")
        );
        self.steps.push(step);
    }

    /// Verify id + signature independently of the broker and record it.
    pub(crate) fn verify(&mut self, event: &Event) -> bool {
        let ok = event.verify().is_ok();
        assert!(
            ok,
            "relay returned an event with an invalid id or signature"
        );
        let id = event.id.to_hex();
        if !self.verified_event_ids.contains(&id) {
            self.verified_event_ids.push(id);
        }
        ok
    }
}

// ── Broker action helpers ──────────────────────────────────────────────────

pub(crate) struct Verdict {
    pub(crate) result: BrokerResult,
    pub(crate) replayed: bool,
}

impl Verdict {
    pub(crate) fn label(&self) -> String {
        match &self.result {
            BrokerResult::Succeeded { .. } => "succeeded".to_owned(),
            BrokerResult::Failed { error } => format!("failed:{}", error.code.as_str()),
            BrokerResult::Indeterminate { error } => {
                format!("indeterminate:{}", error.code.as_str())
            }
        }
    }

    pub(crate) fn failure_code(&self) -> Option<BrokerErrorCode> {
        match &self.result {
            BrokerResult::Succeeded { .. } => None,
            BrokerResult::Failed { error } | BrokerResult::Indeterminate { error } => {
                Some(error.code)
            }
        }
    }

    pub(crate) fn published_event_id(&self) -> String {
        match &self.result {
            BrokerResult::Succeeded {
                outcome: ActionOutcome::MessagePost(published),
            }
            | BrokerResult::Succeeded {
                outcome: ActionOutcome::MessageReply(published),
            } => published.event_id.clone(),
            _ => panic!("expected a published message, got {}", self.label()),
        }
    }
}

pub(crate) fn prepare(request_id: &str, args: ActionArgs) -> PreparedRequest {
    BrokerRequest::new(request_id, args)
        .and_then(BrokerRequest::prepare)
        .expect("valid broker request")
}

pub(crate) async fn send(client: &HttpBrokerClient, request: &PreparedRequest) -> Verdict {
    let response = client
        .execute(request)
        .await
        .expect("broker host returns a correlated verdict");
    let replayed = response.replayed();
    Verdict {
        result: response.into_envelope().result,
        replayed,
    }
}

pub(crate) fn post_args(channel: &str, content: &str) -> ActionArgs {
    ActionArgs::MessagePost(MessagePostArgs {
        channel_id: channel.to_owned(),
        content: content.to_owned(),
        mentions: Vec::new(),
    })
}

pub(crate) fn identity(
    relay_url: &str,
    agent: &Keys,
    task_id: &str,
    generation: &str,
) -> AuthorityIdentity {
    serde_json::from_value::<AuthorityIdentity>(json!({
        "communityRelayUrl": relay_url,
        "logicalAgentPubkey": agent.public_key().to_hex(),
        "executorAgentPubkey": agent.public_key().to_hex(),
        "taskId": task_id,
        "generation": generation,
        "location": {"kind": "local", "id": "r1-relay-e2e"},
        "runtimeSupport": "portable"
    }))
    .and_then(|identity| identity.validated().map_err(serde::de::Error::custom))
    .expect("valid authority identity")
}

pub(crate) fn random_bearer() -> String {
    let bytes: [u8; 32] = rand::random();
    format!(
        "bzh_{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    )
}

pub(crate) fn has_reply_parent(event: &Event, parent: &str) -> bool {
    event.tags.iter().any(|tag| {
        let parts = tag.as_slice();
        parts.first().map(String::as_str) == Some("e")
            && parts.get(1).map(String::as_str) == Some(parent)
            && parts.get(3).map(String::as_str) == Some("reply")
    })
}

pub(crate) async fn copies_with_content(
    relay: &Relay,
    owner: &Keys,
    channel: &str,
    agent: &Keys,
    content: &str,
    timeline: &mut Timeline,
) -> Vec<Event> {
    let (_, events) = relay
        .query(
            owner,
            json!({
                "kinds": [9],
                "#h": [channel],
                "authors": [agent.public_key().to_hex()],
                "limit": 100
            }),
        )
        .await;
    let matching: Vec<Event> = events
        .into_iter()
        .filter(|event| event.content == content)
        .collect();
    for event in &matching {
        timeline.verify(event);
    }
    matching
}

/// Owner creates a private channel and adds the agent as a member.
pub(crate) async fn create_private_channel(
    relay: &Relay,
    owner: &Keys,
    agent: &Keys,
    channel: &str,
) -> (Event, Event) {
    let create = EventBuilder::new(Kind::Custom(9007), "")
        .tags([
            Tag::parse(["h", channel]).unwrap(),
            Tag::parse(["name", &format!("r1-proof-{channel}")]).unwrap(),
            Tag::parse(["channel_type", "stream"]).unwrap(),
            Tag::parse(["visibility", "private"]).unwrap(),
        ])
        .sign_with_keys(owner)
        .unwrap();
    let (accepted, message) = relay.submit(owner, &create).await;
    assert!(accepted, "owner creates private channel: {message}");
    let add_agent = EventBuilder::new(Kind::Custom(9000), "")
        .tags([
            Tag::parse(["h", channel]).unwrap(),
            Tag::parse(["p", &agent.public_key().to_hex()]).unwrap(),
        ])
        .sign_with_keys(owner)
        .unwrap();
    let (accepted, message) = relay.submit(owner, &add_agent).await;
    assert!(accepted, "owner adds agent: {message}");
    (create, add_agent)
}
