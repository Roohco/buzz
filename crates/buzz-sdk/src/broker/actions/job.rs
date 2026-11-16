//! Scoped peer handoff over the agent job kinds (43001/43002/43004/43005/43006).
//!
//! One agent (the sender) asks one other agent (the recipient) for one bounded
//! capability on one exact candidate. Every handoff event is signed by the
//! broker host holding that agent's key, so a keyless runtime only names the
//! business fields here and never signs, tags or addresses an event itself.
//!
//! The recipient's host decides admission (grant, sender generation, candidate
//! identity, deadline, idempotency) **before** returning a ticket, so a model
//! runtime never sees an unadmitted request. Progress (43003) is deliberately
//! absent until a consumer needs it.
//!
//! The tag codec lives here so the host and any later consumer read the same
//! event fields the same way.

use serde::{Deserialize, Serialize};

use super::{
    absent_or_valued, absent_or_valued_hex64, channel, channel_id, event_id, hex64_field, required,
    PubkeyHex,
};
use crate::SdkError;

/// The only capability this slice grants: QA review of one exact candidate.
pub const CAPABILITY_QA_REVIEW_CANDIDATE: &str = "qa.review_candidate";

/// Longest accepted bounded token (`pipelineId`, `stageId`, `idempotencyKey`).
pub const MAX_JOB_TOKEN_LEN: usize = 128;

/// Longest accepted capability name.
pub const MAX_JOB_CAPABILITY_LEN: usize = 64;

/// Largest run budget a request may name, in seconds.
pub const MAX_JOB_BUDGET_SECONDS: u32 = 24 * 60 * 60;

/// Highest attempt number a request may carry.
pub const MAX_JOB_ATTEMPTS: u32 = 3;

/// Longest summary/reason text a handoff event may carry, in bytes.
pub const MAX_JOB_TEXT_BYTES: usize = 4096;

/// Signed tag names carried by handoff events.
pub mod tag {
    /// Requested recipient capability.
    pub const CAPABILITY: &str = "capability";
    /// Cross-group pipeline identity.
    pub const PIPELINE_ID: &str = "pipeline_id";
    /// Stage within the pipeline.
    pub const STAGE_ID: &str = "stage_id";
    /// Event that caused this request (candidate post or prior result).
    pub const PARENT_EVENT: &str = "parent_event";
    /// Exact candidate commit/tree SHA.
    pub const CANDIDATE_SHA: &str = "candidate_sha";
    /// Digest of the pinned preview bundle manifest.
    pub const PREVIEW_BUNDLE_DIGEST: &str = "preview_bundle_digest";
    /// Recipient run budget in seconds.
    pub const BUDGET_SECONDS: &str = "budget_seconds";
    /// Absolute deadline, Unix seconds.
    pub const DEADLINE: &str = "deadline";
    /// Attempt number, starting at 1.
    pub const ATTEMPT: &str = "attempt";
    /// Sender-chosen key; one run per sender and key.
    pub const IDEMPOTENCY_KEY: &str = "idempotency_key";
    /// Request event a decision, result or cancel refers to.
    pub const REQUEST: &str = "request";
    /// Acceptance event a result refers to.
    pub const ACCEPTED: &str = "accepted";
    /// Domain verdict of a result.
    pub const VERDICT: &str = "verdict";
    /// Machine-readable deny/error/cancel reason.
    pub const REASON: &str = "reason";
    /// Hash of protected evidence a result refers to.
    pub const EVIDENCE_SHA256: &str = "evidence_sha256";
}

/// The handoff identity every request, ticket and linked event carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JobHandoff {
    /// Requested recipient capability.
    pub capability: String,
    /// Cross-group pipeline identity.
    pub pipeline_id: String,
    /// Stage within the pipeline.
    pub stage_id: String,
    /// Event that caused this request.
    #[serde(deserialize_with = "hex64_field")]
    pub parent_event_id: String,
    /// Exact candidate SHA (40 or 64 lowercase hex).
    pub candidate_sha: String,
    /// `sha256:<64 hex>` digest of the pinned preview bundle manifest.
    pub preview_bundle_digest: String,
    /// Recipient run budget in seconds.
    pub budget_seconds: u32,
    /// Absolute deadline, Unix seconds.
    pub deadline: u64,
    /// Attempt number, starting at 1.
    pub attempt: u32,
    /// Sender-chosen idempotency key.
    pub idempotency_key: String,
}

impl JobHandoff {
    /// Validate and normalize every field.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError::InvalidInput`] naming the first malformed field.
    pub fn validated(&self) -> Result<Self, SdkError> {
        Ok(Self {
            capability: capability(&self.capability)?,
            pipeline_id: token(&self.pipeline_id, "pipelineId")?,
            stage_id: token(&self.stage_id, "stageId")?,
            parent_event_id: event_id(&self.parent_event_id, "parentEventId")?,
            candidate_sha: candidate_sha(&self.candidate_sha)?,
            preview_bundle_digest: bundle_digest(&self.preview_bundle_digest)?,
            budget_seconds: bounded(
                self.budget_seconds,
                1,
                MAX_JOB_BUDGET_SECONDS,
                "budgetSeconds",
            )?,
            deadline: if self.deadline == 0 || self.deadline > u64::from(u32::MAX) {
                return Err(SdkError::InvalidInput(
                    "deadline must be positive Unix seconds".into(),
                ));
            } else {
                self.deadline
            },
            attempt: bounded(self.attempt, 1, MAX_JOB_ATTEMPTS, "attempt")?,
            idempotency_key: token(&self.idempotency_key, "idempotencyKey")?,
        })
    }

    /// The signed tags this handoff contributes to an event, in fixed order.
    #[must_use]
    pub fn tag_pairs(&self) -> Vec<(&'static str, String)> {
        vec![
            (tag::CAPABILITY, self.capability.clone()),
            (tag::PIPELINE_ID, self.pipeline_id.clone()),
            (tag::STAGE_ID, self.stage_id.clone()),
            (tag::PARENT_EVENT, self.parent_event_id.clone()),
            (tag::CANDIDATE_SHA, self.candidate_sha.clone()),
            (
                tag::PREVIEW_BUNDLE_DIGEST,
                self.preview_bundle_digest.clone(),
            ),
            (tag::BUDGET_SECONDS, self.budget_seconds.to_string()),
            (tag::DEADLINE, self.deadline.to_string()),
            (tag::ATTEMPT, self.attempt.to_string()),
            (tag::IDEMPOTENCY_KEY, self.idempotency_key.clone()),
        ]
    }

    /// Read and validate a handoff from signed event tags.
    ///
    /// Every handoff tag must occur exactly once; a missing, repeated or
    /// malformed tag is an error, never a default.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError::InvalidInput`] naming the first bad tag.
    pub fn from_tags(tags: &nostr::Tags) -> Result<Self, SdkError> {
        let one = |name: &str| single_tag(tags, name);
        let number = |name: &str| -> Result<u64, SdkError> {
            let raw = one(name)?;
            if raw.is_empty() || raw.len() > 10 || !raw.bytes().all(|b| b.is_ascii_digit()) {
                return Err(SdkError::InvalidInput(format!("{name} must be decimal")));
            }
            raw.parse::<u64>()
                .map_err(|_| SdkError::InvalidInput(format!("{name} must be decimal")))
        };
        let small = |name: &str| -> Result<u32, SdkError> {
            u32::try_from(number(name)?)
                .map_err(|_| SdkError::InvalidInput(format!("{name} is out of range")))
        };
        Self {
            capability: one(tag::CAPABILITY)?,
            pipeline_id: one(tag::PIPELINE_ID)?,
            stage_id: one(tag::STAGE_ID)?,
            parent_event_id: one(tag::PARENT_EVENT)?,
            candidate_sha: one(tag::CANDIDATE_SHA)?,
            preview_bundle_digest: one(tag::PREVIEW_BUNDLE_DIGEST)?,
            budget_seconds: small(tag::BUDGET_SECONDS)?,
            deadline: number(tag::DEADLINE)?,
            attempt: small(tag::ATTEMPT)?,
            idempotency_key: one(tag::IDEMPOTENCY_KEY)?,
        }
        .validated()
        .and_then(|parsed| {
            // A canonical event spells every field the way `validated` would;
            // an alternative spelling (uppercase hex) is a different request.
            let canonical = parsed.tag_pairs();
            for (name, value) in &canonical {
                if one(name)? != *value {
                    return Err(SdkError::InvalidInput(format!("{name} is not canonical")));
                }
            }
            Ok(parsed)
        })
    }
}

/// Return the value of the one tag named `name`.
///
/// # Errors
///
/// Returns [`SdkError::InvalidInput`] when the tag is absent, repeated or
/// carries other than exactly one value.
pub fn single_tag(tags: &nostr::Tags, name: &str) -> Result<String, SdkError> {
    let mut found = tags
        .iter()
        .map(nostr::Tag::as_slice)
        .filter(|parts| parts.first().map(String::as_str) == Some(name));
    let first = found
        .next()
        .ok_or_else(|| SdkError::InvalidInput(format!("{name} tag is missing")))?;
    if found.next().is_some() || first.len() != 2 {
        return Err(SdkError::InvalidInput(format!(
            "{name} tag must occur once with one value"
        )));
    }
    Ok(first[1].clone())
}

/// Arguments for `job.request`: ask one granted recipient for one capability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JobRequestArgs {
    /// Project handoff channel.
    #[serde(deserialize_with = "channel_id")]
    pub channel_id: String,
    /// Recipient agent identity.
    pub recipient_pubkey: PubkeyHex,
    /// Handoff identity and bounds.
    pub handoff: JobHandoff,
}

impl JobRequestArgs {
    /// Validate and normalize.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError::InvalidInput`] for any malformed field.
    pub fn validated(&self) -> Result<Self, SdkError> {
        Ok(Self {
            channel_id: channel(&self.channel_id)?,
            recipient_pubkey: PubkeyHex::parse(self.recipient_pubkey.as_str())?,
            handoff: self.handoff.validated()?,
        })
    }
}

/// Arguments for `job.admit`: let the recipient host decide one pending request.
///
/// Without `requestEventId` the host decides the oldest undecided request
/// addressed to this agent in the channel; with it, that exact request (or
/// replays its recorded decision).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JobAdmitArgs {
    /// Project handoff channel.
    #[serde(deserialize_with = "channel_id")]
    pub channel_id: String,
    /// Exact request to decide.
    #[serde(
        default,
        deserialize_with = "absent_or_valued_hex64",
        skip_serializing_if = "Option::is_none"
    )]
    pub request_event_id: Option<String>,
}

impl JobAdmitArgs {
    /// Validate and normalize.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError::InvalidInput`] for a malformed channel or event id.
    pub fn validated(&self) -> Result<Self, SdkError> {
        Ok(Self {
            channel_id: channel(&self.channel_id)?,
            request_event_id: self
                .request_event_id
                .as_deref()
                .map(|id| event_id(id, "requestEventId"))
                .transpose()?,
        })
    }
}

/// Domain verdict a recipient returns for an admitted job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum JobVerdict {
    /// The candidate passed review.
    Pass,
    /// The candidate has a defect.
    Defect,
    /// Review could not run to a verdict.
    Blocked,
    /// Required input was missing.
    InputIncomplete,
    /// The recipient failed; published as a job error (43006).
    Error,
}

impl JobVerdict {
    /// Stable wire spelling, also used as the signed `verdict` tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Defect => "DEFECT",
            Self::Blocked => "BLOCKED",
            Self::InputIncomplete => "INPUT_INCOMPLETE",
            Self::Error => "ERROR",
        }
    }
}

/// Arguments for `job.result`: publish the one result of an admitted job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JobResultArgs {
    /// Admitted request this result completes.
    #[serde(deserialize_with = "hex64_field")]
    pub request_event_id: String,
    /// Domain verdict.
    pub verdict: JobVerdict,
    /// Short human-readable summary.
    pub summary: String,
    /// Hash of protected evidence.
    #[serde(
        default,
        deserialize_with = "absent_or_valued_hex64",
        skip_serializing_if = "Option::is_none"
    )]
    pub evidence_sha256: Option<String>,
}

impl JobResultArgs {
    /// Validate and normalize.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError::InvalidInput`] for a malformed id or summary.
    pub fn validated(&self) -> Result<Self, SdkError> {
        Ok(Self {
            request_event_id: event_id(&self.request_event_id, "requestEventId")?,
            verdict: self.verdict,
            summary: text(&self.summary, "summary")?,
            evidence_sha256: self
                .evidence_sha256
                .as_deref()
                .map(|value| event_id(value, "evidenceSha256"))
                .transpose()?,
        })
    }
}

/// Arguments for `job.cancel`: the sender withdraws its own request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JobCancelArgs {
    /// Request this sender published.
    #[serde(deserialize_with = "hex64_field")]
    pub request_event_id: String,
    /// Short reason.
    pub reason: String,
}

impl JobCancelArgs {
    /// Validate and normalize.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError::InvalidInput`] for a malformed id or reason.
    pub fn validated(&self) -> Result<Self, SdkError> {
        Ok(Self {
            request_event_id: event_id(&self.request_event_id, "requestEventId")?,
            reason: text(&self.reason, "reason")?,
        })
    }
}

/// What the recipient host decided in one `job.admit` call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobDecision {
    /// No undecided request is addressed to this agent in the channel.
    None,
    /// Admitted: the ticket is the only work input; one run per ticket.
    Accepted,
    /// Denied before any work, with a signed job error.
    Denied,
    /// The sender cancelled before admission or before a result.
    Cancelled,
}

/// The admitted job a recipient runtime may work on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JobTicket {
    /// Signed request event.
    #[serde(deserialize_with = "hex64_field")]
    pub request_event_id: String,
    /// Verified sender identity.
    pub sender_pubkey: PubkeyHex,
    /// Project handoff channel.
    #[serde(deserialize_with = "channel_id")]
    pub channel_id: String,
    /// Verified handoff fields, read from the signed request.
    pub handoff: JobHandoff,
}

/// Outcome of `job.admit`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JobAdmission {
    /// The decision.
    pub decision: JobDecision,
    /// Decided request, absent for [`JobDecision::None`].
    #[serde(
        default,
        deserialize_with = "absent_or_valued_hex64",
        skip_serializing_if = "Option::is_none"
    )]
    pub request_event_id: Option<String>,
    /// Signed acceptance (43002) or error (43006) event.
    #[serde(
        default,
        deserialize_with = "absent_or_valued_hex64",
        skip_serializing_if = "Option::is_none"
    )]
    pub decision_event_id: Option<String>,
    /// Machine-readable deny reason.
    #[serde(
        default,
        deserialize_with = "absent_or_valued",
        skip_serializing_if = "Option::is_none"
    )]
    pub reason: Option<String>,
    /// Admitted job, present only when accepted.
    #[serde(
        default,
        deserialize_with = "absent_or_valued",
        skip_serializing_if = "Option::is_none"
    )]
    pub ticket: Option<JobTicket>,
}

impl JobAdmission {
    /// Check that members agree with the decision.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError::InvalidInput`] when a member is present or absent
    /// contrary to the decision, or an identifier is malformed.
    pub fn validate(&self) -> Result<(), SdkError> {
        let shape = (
            self.request_event_id.is_some(),
            self.decision_event_id.is_some(),
            self.reason.is_some(),
            self.ticket.is_some(),
        );
        let expected = match self.decision {
            JobDecision::None => (false, false, false, false),
            JobDecision::Accepted => (true, true, false, true),
            JobDecision::Denied => (true, true, true, false),
            JobDecision::Cancelled => (true, false, false, false),
        };
        if shape != expected {
            return Err(SdkError::InvalidInput(
                "job admission members contradict its decision".into(),
            ));
        }
        for id in [&self.request_event_id, &self.decision_event_id]
            .into_iter()
            .flatten()
        {
            event_id(id, "eventId")?;
        }
        if let Some(reason) = &self.reason {
            token(reason, "reason")?;
        }
        if let Some(ticket) = &self.ticket {
            event_id(&ticket.request_event_id, "requestEventId")?;
            channel(&ticket.channel_id)?;
            if ticket.handoff.validated()? != ticket.handoff
                || self.request_event_id.as_deref() != Some(ticket.request_event_id.as_str())
            {
                return Err(SdkError::InvalidInput(
                    "job ticket does not match the admitted request".into(),
                ));
            }
        }
        Ok(())
    }
}

fn token(value: &str, label: &str) -> Result<String, SdkError> {
    if value.is_empty()
        || value.len() > MAX_JOB_TOKEN_LEN
        || !value.bytes().all(|b| (0x21..=0x7e).contains(&b))
    {
        return Err(SdkError::InvalidInput(format!(
            "{label} must be 1-{MAX_JOB_TOKEN_LEN} printable ASCII characters without spaces"
        )));
    }
    Ok(value.to_owned())
}

fn capability(value: &str) -> Result<String, SdkError> {
    if value.is_empty()
        || value.len() > MAX_JOB_CAPABILITY_LEN
        || !value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
    {
        return Err(SdkError::InvalidInput(
            "capability must be lowercase [a-z0-9._-]".into(),
        ));
    }
    Ok(value.to_owned())
}

fn candidate_sha(value: &str) -> Result<String, SdkError> {
    if !(value.len() == 40 || value.len() == 64) || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(SdkError::InvalidInput(
            "candidateSha must be 40 or 64 hex characters".into(),
        ));
    }
    Ok(value.to_ascii_lowercase())
}

fn bundle_digest(value: &str) -> Result<String, SdkError> {
    let hex = value
        .strip_prefix("sha256:")
        .filter(|hex| hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| {
            SdkError::InvalidInput("previewBundleDigest must be sha256:<64 hex>".into())
        })?;
    Ok(format!("sha256:{}", hex.to_ascii_lowercase()))
}

fn bounded(value: u32, min: u32, max: u32, label: &str) -> Result<u32, SdkError> {
    if !(min..=max).contains(&value) {
        return Err(SdkError::InvalidInput(format!(
            "{label} must be between {min} and {max}"
        )));
    }
    Ok(value)
}

fn text(value: &str, label: &str) -> Result<String, SdkError> {
    let value = required(value, label, MAX_JOB_TEXT_BYTES)?;
    if value.len() > MAX_JOB_TEXT_BYTES {
        return Err(SdkError::ContentTooLarge {
            max: MAX_JOB_TEXT_BYTES,
            got: value.len(),
        });
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handoff() -> JobHandoff {
        JobHandoff {
            capability: CAPABILITY_QA_REVIEW_CANDIDATE.into(),
            pipeline_id: "pipeline-1".into(),
            stage_id: "qa".into(),
            parent_event_id: "a".repeat(64),
            candidate_sha: "0123456789abcdef0123456789abcdef01234567".into(),
            preview_bundle_digest: format!("sha256:{}", "b".repeat(64)),
            budget_seconds: 600,
            deadline: 1_900_000_000,
            attempt: 1,
            idempotency_key: "pipeline-1:qa:1".into(),
        }
    }

    fn tags(pairs: Vec<(&str, String)>) -> nostr::Tags {
        nostr::Tags::from_list(
            pairs
                .into_iter()
                .map(|(name, value)| nostr::Tag::parse([name, value.as_str()]).unwrap())
                .collect(),
        )
    }

    #[test]
    fn handoff_round_trips_through_signed_tags() {
        let handoff = handoff();
        assert_eq!(
            JobHandoff::from_tags(&tags(handoff.tag_pairs())).unwrap(),
            handoff
        );
    }

    #[test]
    fn missing_repeated_or_noncanonical_identity_tags_are_rejected() {
        let base = handoff().tag_pairs();
        let without: Vec<_> = base
            .iter()
            .filter(|(name, _)| *name != tag::CANDIDATE_SHA)
            .cloned()
            .collect();
        assert!(JobHandoff::from_tags(&tags(without)).is_err());
        let mut repeated = base.clone();
        repeated.push((
            tag::PREVIEW_BUNDLE_DIGEST,
            format!("sha256:{}", "c".repeat(64)),
        ));
        assert!(JobHandoff::from_tags(&tags(repeated)).is_err());
        let upper: Vec<_> = base
            .iter()
            .map(|(name, value)| {
                if *name == tag::CANDIDATE_SHA {
                    (*name, value.to_ascii_uppercase())
                } else {
                    (*name, value.clone())
                }
            })
            .collect();
        assert!(JobHandoff::from_tags(&tags(upper)).is_err());
    }

    #[test]
    fn handoff_bounds_are_enforced() {
        for broken in [
            JobHandoff {
                candidate_sha: "abc".into(),
                ..handoff()
            },
            JobHandoff {
                preview_bundle_digest: "md5:abc".into(),
                ..handoff()
            },
            JobHandoff {
                budget_seconds: 0,
                ..handoff()
            },
            JobHandoff {
                attempt: MAX_JOB_ATTEMPTS + 1,
                ..handoff()
            },
            JobHandoff {
                deadline: 0,
                ..handoff()
            },
            JobHandoff {
                capability: "QA Review".into(),
                ..handoff()
            },
        ] {
            assert!(broken.validated().is_err(), "{broken:?}");
        }
    }

    #[test]
    fn admission_members_must_match_the_decision() {
        let denied_without_reason = JobAdmission {
            decision: JobDecision::Denied,
            request_event_id: Some("a".repeat(64)),
            decision_event_id: Some("b".repeat(64)),
            reason: None,
            ticket: None,
        };
        assert!(denied_without_reason.validate().is_err());
        let none = JobAdmission {
            decision: JobDecision::None,
            request_event_id: None,
            decision_event_id: None,
            reason: None,
            ticket: None,
        };
        assert!(none.validate().is_ok());
    }
}
