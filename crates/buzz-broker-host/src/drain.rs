use buzz_sdk::broker::AuthorityIdentity;

use crate::publisher::{PublishError, RelayPublisher};
use crate::signing::HostSigner;
use crate::store::{AuthorityStore, StoreError};

pub(crate) async fn drain_pending(
    store: &AuthorityStore,
    identity: &AuthorityIdentity,
    signer: &HostSigner,
    publisher: &RelayPublisher,
) -> Result<bool, DrainError> {
    let _publication_guard = store.publication_lock_for_identity(identity).await?;
    let mut drained = false;
    if let Some(pending) = store.pending_message_post(identity).await? {
        if !signer.matches_executor(identity) || !signer.matches_event_owner(&pending.event) {
            return Err(StoreError::InvalidIdentity.into());
        }
        publisher
            .publish_pending(signer, &pending.relay_url, &pending.event)
            .await?;
        store
            .complete_message_effect(
                &pending.credential,
                &pending.request_id,
                &pending.event.id.to_hex(),
            )
            .await?;
        drained = true;
    }
    // Stored, unpublished peer handoff events of this generation: republish
    // the exact signed bytes (never re-sign), then mark them published.
    let (relay_url, peer_events) = store.pending_peer_events(identity).await?;
    for pending in peer_events {
        if !signer.matches_executor(identity) || !signer.matches_event_owner(&pending.event) {
            return Err(StoreError::InvalidIdentity.into());
        }
        publisher
            .publish_pending(signer, &relay_url, &pending.event)
            .await?;
        store.mark_peer_published(&pending.key).await?;
        drained = true;
    }
    Ok(drained)
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum DrainError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Publish(#[from] PublishError),
}
