use std::time::Duration;

use base64::Engine as _;
use buzz_sdk::broker::{
    ActionOutcome, BrokerMessage, ChannelReadArgs, MessagePage, MessageReplyArgs,
};
use buzz_sdk::ThreadRef;
use nostr::Event;
use serde::Deserialize;
use sha2::{Digest as _, Sha256};

use crate::signing::HostSigner;

/// Position below which the next older job-request page starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct JobPageCursor {
    pub(crate) created_at: u64,
    pub(crate) event_id: String,
}

impl JobPageCursor {
    /// Whether `event` comes after this cursor in the relay's page order
    /// (`created_at DESC, id ASC`).
    pub(crate) fn precedes(&self, event: &Event) -> bool {
        let created_at = event.created_at.as_secs();
        created_at < self.created_at
            || (created_at == self.created_at && event.id.to_hex() > self.event_id)
    }
}

pub(crate) struct RelayPublisher {
    client: reqwest::Client,
}

const MAX_READ_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const MAX_READ_PAGE_EVENTS: u32 = 16;
const MAX_EXACT_EVENT_BYTES: usize = 1024 * 1024;

#[derive(Deserialize)]
struct RelayAck {
    accepted: bool,
    event_id: String,
}

impl RelayPublisher {
    pub(crate) fn new() -> Result<Self, PublishError> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(20))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| PublishError::Unavailable)?;
        Ok(Self { client })
    }

    pub(crate) async fn resolve_reply_target(
        &self,
        signer: &HostSigner,
        community_relay_url: &str,
        args: &MessageReplyArgs,
    ) -> Result<ThreadRef, PublishError> {
        let parent = self
            .query_exact_channel_event(
                signer,
                community_relay_url,
                &args.channel_id,
                &args.reply_to_event_id,
            )
            .await?;
        let root_event_id = reply_root(&parent)?;
        if root_event_id != parent.id {
            self.query_exact_channel_event(
                signer,
                community_relay_url,
                &args.channel_id,
                &root_event_id.to_hex(),
            )
            .await?;
        }
        Ok(ThreadRef {
            root_event_id,
            parent_event_id: parent.id,
        })
    }

    async fn query_exact_channel_event(
        &self,
        signer: &HostSigner,
        community_relay_url: &str,
        channel_id: &str,
        event_id: &str,
    ) -> Result<Event, PublishError> {
        let url = relay_endpoint(community_relay_url, "/query")?;
        let body = serde_json::to_vec(&serde_json::json!([{
            "ids": [event_id],
            "kinds": [9],
            "#h": [channel_id],
            "limit": 1
        }]))
        .map_err(|_| PublishError::InvalidEvent)?;
        let authorization = signer
            .nip98_authorization(url.as_str(), &body)
            .map_err(|_| PublishError::Unavailable)?;
        let mut response = self
            .client
            .post(url)
            .header("authorization", authorization)
            .header("x-auth-tag", signer.auth_tag_json())
            .header("content-type", "application/json")
            .body(body)
            .send()
            .await
            .map_err(|_| PublishError::Unavailable)?;
        if !response.status().is_success() {
            return Err(PublishError::Unavailable);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| PublishError::Unavailable)?
        {
            if chunk.len() > MAX_EXACT_EVENT_BYTES - bytes.len() {
                return Err(PublishError::ResponseTooLarge);
            }
            bytes.extend_from_slice(&chunk);
        }
        let mut events: Vec<Event> =
            serde_json::from_slice(&bytes).map_err(|_| PublishError::InvalidEvent)?;
        if events.len() != 1 {
            return Err(PublishError::InvalidEvent);
        }
        let event = events.pop().ok_or(PublishError::InvalidEvent)?;
        let channels: Vec<_> = event
            .tags
            .iter()
            .filter(|tag| tag.kind().to_string() == "h")
            .collect();
        if event.id.to_hex() != event_id
            || event.kind.as_u16() != 9
            || event.verify().is_err()
            || channels.len() != 1
            || channels[0].content() != Some(channel_id)
        {
            return Err(PublishError::InvalidEvent);
        }
        Ok(event)
    }

    /// Read one page of signed job requests (43001) in one channel, oldest
    /// first, plus the cursor for the next older page when this one was full.
    ///
    /// With `request_event_id`, only that event. Otherwise requests that
    /// p-tag this signer, newest page first; `before` continues below the
    /// oldest event of the previous page so older requests are reachable.
    /// Every returned event is verified and carries exactly one `h` tag for
    /// the channel; admission still re-checks everything it relies on.
    pub(crate) async fn query_job_requests(
        &self,
        signer: &HostSigner,
        community_relay_url: &str,
        channel_id: &str,
        request_event_id: Option<&str>,
        before: Option<&JobPageCursor>,
    ) -> Result<(Vec<Event>, Option<JobPageCursor>), PublishError> {
        let mut filter = serde_json::json!({
            "kinds": [buzz_core::kind::KIND_JOB_REQUEST],
            "#h": [channel_id],
            "limit": MAX_READ_PAGE_EVENTS,
        });
        match request_event_id {
            Some(id) => filter["ids"] = serde_json::json!([id]),
            None => filter["#p"] = serde_json::json!([signer.pubkey_hex()]),
        }
        let (mut events, next) = self
            .job_page(
                signer,
                community_relay_url,
                filter,
                channel_id,
                &[buzz_core::kind::KIND_JOB_REQUEST],
                before,
            )
            .await?;
        if let Some(id) = request_event_id {
            events.retain(|event| event.id.to_hex() == id);
            return Ok((events, None));
        }
        Ok((events, next))
    }

    /// Read one page of signed decision, result, cancel and error events
    /// (43002/43004/43005/43006) by `authors` in one channel, created at or
    /// after `since`, oldest first, plus the cursor for the next older page.
    ///
    /// Job events link their request through the multi-letter `request` tag,
    /// which relay filters cannot match, so the page is narrowed by kind,
    /// channel and author; the caller checks the link and every other field.
    pub(crate) async fn query_job_outcomes(
        &self,
        signer: &HostSigner,
        community_relay_url: &str,
        channel_id: &str,
        authors: &[&str],
        since: u64,
        before: Option<&JobPageCursor>,
    ) -> Result<(Vec<Event>, Option<JobPageCursor>), PublishError> {
        const KINDS: [u32; 4] = [
            buzz_core::kind::KIND_JOB_ACCEPTED,
            buzz_core::kind::KIND_JOB_RESULT,
            buzz_core::kind::KIND_JOB_CANCEL,
            buzz_core::kind::KIND_JOB_ERROR,
        ];
        let filter = serde_json::json!({
            "kinds": KINDS,
            "authors": authors,
            "#h": [channel_id],
            "since": since,
            "limit": MAX_READ_PAGE_EVENTS,
        });
        let (mut events, next) = self
            .job_page(
                signer,
                community_relay_url,
                filter,
                channel_id,
                &KINDS,
                before,
            )
            .await?;
        events.retain(|event| {
            let author = event.pubkey.to_hex();
            event.created_at.as_secs() >= since && authors.contains(&author.as_str())
        });
        Ok((events, next))
    }

    /// One page of a job-kind query in the relay's own order.
    ///
    /// Returns the verified events of `kinds` carrying exactly one `h` tag for
    /// `channel_id`, oldest first, and the cursor for the next older page when
    /// the relay returned a full one.
    async fn job_page(
        &self,
        signer: &HostSigner,
        community_relay_url: &str,
        mut filter: serde_json::Value,
        channel_id: &str,
        kinds: &[u32],
        before: Option<&JobPageCursor>,
    ) -> Result<(Vec<Event>, Option<JobPageCursor>), PublishError> {
        let url = relay_endpoint(community_relay_url, "/query")?;
        if let Some(cursor) = before {
            filter["until"] = serde_json::json!(cursor.created_at);
            filter["before_id"] = serde_json::json!(cursor.event_id);
        }
        let body = serde_json::to_vec(&serde_json::json!([filter]))
            .map_err(|_| PublishError::InvalidEvent)?;
        let authorization = signer
            .nip98_authorization(url.as_str(), &body)
            .map_err(|_| PublishError::Unavailable)?;
        let mut response = self
            .client
            .post(url)
            .header("authorization", authorization)
            .header("x-auth-tag", signer.auth_tag_json())
            .header("content-type", "application/json")
            .body(body)
            .send()
            .await
            .map_err(|_| PublishError::Unavailable)?;
        if !response.status().is_success() {
            return Err(PublishError::Unavailable);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| PublishError::Unavailable)?
        {
            if chunk.len() > MAX_READ_RESPONSE_BYTES - bytes.len() {
                return Err(PublishError::ResponseTooLarge);
            }
            bytes.extend_from_slice(&chunk);
        }
        let mut events: Vec<Event> =
            serde_json::from_slice(&bytes).map_err(|_| PublishError::InvalidEvent)?;
        if events.len() > MAX_READ_PAGE_EVENTS as usize {
            return Err(PublishError::InvalidEvent);
        }
        let full = events.len() == MAX_READ_PAGE_EVENTS as usize;
        // The relay orders `created_at DESC, id ASC` and continues a page with
        // `created_at < until OR (created_at = until AND id > before_id)`, so
        // the cursor is the last event in that order: the oldest second, and
        // the highest id within it.
        let next = if full {
            events
                .iter()
                .min_by(|left, right| {
                    left.created_at
                        .cmp(&right.created_at)
                        .then_with(|| right.id.cmp(&left.id))
                })
                .map(|oldest| JobPageCursor {
                    created_at: oldest.created_at.as_secs(),
                    event_id: oldest.id.to_hex(),
                })
        } else {
            None
        };
        events.retain(|event| {
            let channels: Vec<_> = event
                .tags
                .iter()
                .filter(|tag| tag.kind().to_string() == "h")
                .collect();
            event.verify().is_ok()
                && kinds.contains(&u32::from(event.kind.as_u16()))
                && channels.len() == 1
                && channels[0].content() == Some(channel_id)
                && before.is_none_or(|cursor| cursor.precedes(event))
        });
        events.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok((events, next))
    }

    pub(crate) async fn query_channel(
        &self,
        signer: &HostSigner,
        community_relay_url: &str,
        args: &ChannelReadArgs,
    ) -> Result<MessagePage, PublishError> {
        if let Some(targets) = &args.reactions_to {
            return self
                .query_reactions(signer, community_relay_url, args, targets)
                .await;
        }
        let url = relay_endpoint(community_relay_url, "/query")?;
        let scope = cursor_scope(args, community_relay_url, signer)?;
        let relay_limit = args.effective_limit().min(MAX_READ_PAGE_EVENTS);
        let mut filter = serde_json::json!({
            "kinds": [9, 40002, 40008, 45001, 45003],
            "#h": [args.channel_id],
            "limit": relay_limit,
        });
        if args.mentions_only {
            filter["#p"] = serde_json::json!([signer.pubkey_hex()]);
        }
        if let Some(root) = &args.root_event_id {
            filter["#e"] = serde_json::json!([root]);
        }
        if let Some(cursor) = &args.cursor {
            let (until, before_id) = decode_cursor(cursor, &scope, signer)?;
            filter["until"] = serde_json::json!(until);
            filter["before_id"] = serde_json::json!(before_id);
        }
        let body = serde_json::to_vec(&serde_json::json!([filter]))
            .map_err(|_| PublishError::InvalidEvent)?;
        let authorization = signer
            .nip98_authorization(url.as_str(), &body)
            .map_err(|_| PublishError::Unavailable)?;
        let mut response = self
            .client
            .post(url)
            .header("authorization", authorization)
            .header("x-auth-tag", signer.auth_tag_json())
            .header("content-type", "application/json")
            .body(body)
            .send()
            .await
            .map_err(|_| PublishError::Unavailable)?;
        if !response.status().is_success() {
            return Err(PublishError::Unavailable);
        }
        let mut response_body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| PublishError::Unavailable)?
        {
            if chunk.len() > MAX_READ_RESPONSE_BYTES - response_body.len() {
                return Err(PublishError::ResponseTooLarge);
            }
            response_body.extend_from_slice(&chunk);
        }
        let messages: Vec<BrokerMessage> =
            serde_json::from_slice(&response_body).map_err(|_| PublishError::InvalidEvent)?;
        let signer_pubkey = signer.pubkey_hex();
        if messages.len() > relay_limit as usize
            || messages.iter().any(|message| {
                let channel_tags: Vec<_> = message
                    .0
                    .tags
                    .iter()
                    .filter(|tag| tag.kind().to_string() == "h")
                    .collect();
                let kind = message.0.kind.as_u16();
                message.verify().is_err()
                    || ![9, 40002, 40008, 45001, 45003].contains(&kind)
                    || channel_tags.len() != 1
                    || channel_tags[0].content() != Some(args.channel_id.as_str())
                    || (args.mentions_only
                        && !message.0.tags.iter().any(|tag| {
                            tag.kind().to_string() == "p"
                                && tag.content() == Some(signer_pubkey.as_str())
                        }))
                    || args.root_event_id.as_ref().is_some_and(|root| {
                        !message.0.tags.iter().any(|tag| {
                            tag.kind().to_string() == "e" && tag.content() == Some(root.as_str())
                        })
                    })
            })
        {
            return Err(PublishError::InvalidEvent);
        }
        ActionOutcome::ChannelRead(MessagePage {
            messages: messages.clone(),
            next_cursor: None,
        })
        .validate()
        .map_err(|_| PublishError::InvalidEvent)?;
        let next_cursor = if messages.len() == relay_limit as usize {
            messages
                .last()
                .map(|message| encode_cursor(message, &scope, signer))
        } else {
            None
        };
        Ok(MessagePage {
            messages,
            next_cursor,
        })
    }

    /// One page of NIP-25 reactions (kind 7) to `targets`, newest first.
    ///
    /// Every target is first proven to be a signed kind-9 message carrying
    /// exactly one `h` tag for the requested channel; otherwise the read is
    /// refused, because reactions in this relay carry no `h` tag and the
    /// target is the only thing tying them to the granted channel. A returned
    /// event must be a verified kind 7 with an `e` tag naming a target; one
    /// whose last `e` tag (NIP-25's reacted-to event) is not a target reacts
    /// to another event and is dropped. The relay soft-deletes a reaction on
    /// a NIP-09 deletion and `/query` omits it, so no kind 5 is returned.
    async fn query_reactions(
        &self,
        signer: &HostSigner,
        community_relay_url: &str,
        args: &ChannelReadArgs,
        targets: &[String],
    ) -> Result<MessagePage, PublishError> {
        let body = self
            .query_bytes(
                signer,
                community_relay_url,
                &serde_json::json!({
                    "ids": targets,
                    "kinds": [9],
                    "#h": [args.channel_id],
                    "limit": targets.len(),
                }),
            )
            .await?;
        let found: Vec<Event> =
            serde_json::from_slice(&body).map_err(|_| PublishError::InvalidEvent)?;
        let in_channel = |id: &String| {
            found.iter().any(|event| {
                let mut channels = event
                    .tags
                    .iter()
                    .filter(|tag| tag.kind().to_string() == "h");
                event.id.to_hex() == *id
                    && event.kind.as_u16() == 9
                    && event.verify().is_ok()
                    && channels.next().and_then(|tag| tag.content())
                        == Some(args.channel_id.as_str())
                    && channels.next().is_none()
            })
        };
        if found.len() > targets.len() || !targets.iter().all(in_channel) {
            return Err(PublishError::TargetOutsideChannel);
        }

        let relay_limit = args.effective_limit().min(MAX_READ_PAGE_EVENTS);
        let body = self
            .query_bytes(
                signer,
                community_relay_url,
                &serde_json::json!({
                    "kinds": [7],
                    "#e": targets,
                    "limit": relay_limit,
                }),
            )
            .await?;
        let reactions: Vec<BrokerMessage> =
            serde_json::from_slice(&body).map_err(|_| PublishError::InvalidEvent)?;
        if reactions.len() > relay_limit as usize {
            return Err(PublishError::InvalidEvent);
        }
        let mut messages = Vec::with_capacity(reactions.len());
        for reaction in reactions {
            let referenced: Vec<&str> = reaction
                .0
                .tags
                .iter()
                .filter(|tag| tag.kind().to_string() == "e")
                .filter_map(|tag| tag.content())
                .collect();
            if reaction.verify().is_err()
                || reaction.0.kind.as_u16() != 7
                || !referenced.iter().any(|id| targets.iter().any(|t| t == id))
            {
                return Err(PublishError::InvalidEvent);
            }
            if referenced
                .last()
                .is_some_and(|id| targets.iter().any(|t| t == id))
            {
                messages.push(reaction);
            }
        }
        let page = MessagePage {
            messages,
            next_cursor: None,
        };
        ActionOutcome::ChannelRead(page.clone())
            .validate()
            .map_err(|_| PublishError::InvalidEvent)?;
        Ok(page)
    }

    /// POST one filter to the relay's `/query` under host NIP-98 auth and
    /// return the body, capped at the read response limit.
    async fn query_bytes(
        &self,
        signer: &HostSigner,
        community_relay_url: &str,
        filter: &serde_json::Value,
    ) -> Result<Vec<u8>, PublishError> {
        let url = relay_endpoint(community_relay_url, "/query")?;
        let body = serde_json::to_vec(&serde_json::json!([filter]))
            .map_err(|_| PublishError::InvalidEvent)?;
        let authorization = signer
            .nip98_authorization(url.as_str(), &body)
            .map_err(|_| PublishError::Unavailable)?;
        let mut response = self
            .client
            .post(url)
            .header("authorization", authorization)
            .header("x-auth-tag", signer.auth_tag_json())
            .header("content-type", "application/json")
            .body(body)
            .send()
            .await
            .map_err(|_| PublishError::Unavailable)?;
        if !response.status().is_success() {
            return Err(PublishError::Unavailable);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| PublishError::Unavailable)?
        {
            if chunk.len() > MAX_READ_RESPONSE_BYTES - bytes.len() {
                return Err(PublishError::ResponseTooLarge);
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }

    pub(crate) async fn publish(
        &self,
        signer: &HostSigner,
        community_relay_url: &str,
        event: &Event,
    ) -> Result<(), PublishError> {
        let url = relay_endpoint(community_relay_url, "/events")?;
        let body = serde_json::to_vec(event).map_err(|_| PublishError::InvalidEvent)?;
        let authorization = signer
            .nip98_authorization(url.as_str(), &body)
            .map_err(|_| PublishError::Unavailable)?;
        let response = self
            .client
            .post(url)
            .header("authorization", authorization)
            .header("x-auth-tag", signer.auth_tag_json())
            .header("content-type", "application/json")
            .body(body)
            .send()
            .await;
        if let Ok(response) = response {
            if response.status().is_success() {
                if let Ok(ack) = response.json::<RelayAck>().await {
                    if ack.accepted && ack.event_id == event.id.to_hex() {
                        return Ok(());
                    }
                }
            }
        }
        if self.find(signer, community_relay_url, event).await? {
            Ok(())
        } else {
            Err(PublishError::Indeterminate)
        }
    }

    pub(crate) async fn publish_pending(
        &self,
        signer: &HostSigner,
        community_relay_url: &str,
        event: &Event,
    ) -> Result<(), PublishError> {
        if self.find(signer, community_relay_url, event).await? {
            Ok(())
        } else {
            self.publish(signer, community_relay_url, event).await
        }
    }

    async fn find(
        &self,
        signer: &HostSigner,
        community_relay_url: &str,
        event: &Event,
    ) -> Result<bool, PublishError> {
        let url = relay_endpoint(community_relay_url, "/query")?;
        let body = serde_json::to_vec(&serde_json::json!([{
            "ids": [event.id.to_hex()],
            "limit": 1
        }]))
        .map_err(|_| PublishError::Indeterminate)?;
        let authorization = signer
            .nip98_authorization(url.as_str(), &body)
            .map_err(|_| PublishError::Indeterminate)?;
        let response = self
            .client
            .post(url)
            .header("authorization", authorization)
            .header("x-auth-tag", signer.auth_tag_json())
            .header("content-type", "application/json")
            .body(body)
            .send()
            .await
            .map_err(|_| PublishError::Indeterminate)?;
        if !response.status().is_success() {
            return Err(PublishError::Indeterminate);
        }
        let found: Vec<Event> = response
            .json()
            .await
            .map_err(|_| PublishError::Indeterminate)?;
        Ok(found.iter().any(|candidate| {
            candidate.id == event.id
                && candidate.pubkey == event.pubkey
                && candidate.verify().is_ok()
        }))
    }
}

fn cursor_scope(
    args: &ChannelReadArgs,
    community_relay_url: &str,
    signer: &HostSigner,
) -> Result<[u8; 16], PublishError> {
    let scope = serde_json::to_vec(&(
        &args.channel_id,
        &args.root_event_id,
        args.mentions_only,
        args.effective_limit(),
        community_relay_url,
        signer.pubkey_hex(),
        signer.owner_hex(),
    ))
    .map_err(|_| PublishError::InvalidCursor)?;
    let digest = Sha256::digest(scope);
    let mut truncated = [0_u8; 16];
    truncated.copy_from_slice(&digest[..16]);
    Ok(truncated)
}

fn encode_cursor(message: &BrokerMessage, scope: &[u8; 16], signer: &HostSigner) -> String {
    let mut payload = Vec::with_capacity(73);
    payload.push(1);
    payload.extend_from_slice(&message.0.created_at.as_secs().to_be_bytes());
    payload.extend_from_slice(&message.0.id.to_bytes());
    payload.extend_from_slice(scope);
    payload.extend_from_slice(&signer.cursor_mac(&payload));
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload)
}

fn decode_cursor(
    cursor: &str,
    expected_scope: &[u8; 16],
    signer: &HostSigner,
) -> Result<(u64, String), PublishError> {
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(cursor)
        .map_err(|_| PublishError::InvalidCursor)?;
    if payload.len() != 73 || payload[0] != 1 || payload[41..57] != expected_scope[..] {
        return Err(PublishError::InvalidCursor);
    }
    let expected_mac = signer.cursor_mac(&payload[..57]);
    let mismatch = payload[57..]
        .iter()
        .zip(expected_mac)
        .fold(0_u8, |acc, (left, right)| acc | (left ^ right));
    if mismatch != 0 {
        return Err(PublishError::InvalidCursor);
    }
    let until = u64::from_be_bytes(
        payload[1..9]
            .try_into()
            .map_err(|_| PublishError::InvalidCursor)?,
    );
    Ok((until, hex::encode(&payload[9..41])))
}

fn reply_root(parent: &Event) -> Result<nostr::EventId, PublishError> {
    let markers = buzz_core::nip10::parse_thread_markers(&parent.tags);
    let Some((root, _)) = markers.resolve() else {
        return Ok(parent.id);
    };
    nostr::EventId::from_hex(&root).map_err(|_| PublishError::InvalidEvent)
}

fn relay_endpoint(community_relay_url: &str, path: &str) -> Result<reqwest::Url, PublishError> {
    let http_url = if let Some(rest) = community_relay_url.strip_prefix("wss://") {
        format!("https://{rest}")
    } else if let Some(rest) = community_relay_url.strip_prefix("ws://") {
        format!("http://{rest}")
    } else {
        return Err(PublishError::InvalidRelay);
    };
    let mut url = reqwest::Url::parse(&http_url).map_err(|_| PublishError::InvalidRelay)?;
    if url.username() != "" || url.password().is_some() || url.fragment().is_some() {
        return Err(PublishError::InvalidRelay);
    }
    url.set_path(path);
    url.set_query(None);
    Ok(url)
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum PublishError {
    #[error("relay URL is invalid")]
    InvalidRelay,
    #[error("signed event is invalid")]
    InvalidEvent,
    #[error("channel cursor is invalid for this scope")]
    InvalidCursor,
    #[error("reaction target is not a message in the requested channel")]
    TargetOutsideChannel,
    #[error("relay channel response exceeds the host page cap")]
    ResponseTooLarge,
    #[error("relay publisher is unavailable")]
    Unavailable,
    #[error("relay publication outcome is indeterminate")]
    Indeterminate,
}
