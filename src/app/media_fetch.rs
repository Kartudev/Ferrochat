//! Media downloads that survive expired CDN copies, like WhatsApp Desktop:
//! when the CDN answers 404 / 410 (`Error::MediaExpired`, typical for old
//! history), a re-upload is requested (`WaClient::reupload`, the "media
//! retry" receipt), the cached message is pointed at the new path and the
//! download is tried once more.
//!
//! Tasks cannot change `App`, so a refreshed reference is written to the
//! cache right away and parked in [`FreshMedia`]; `App::update` applies it to
//! the open conversation on its next message ([`App::apply_fresh_media`]).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::error::Error;
use crate::net::{ClientHandle, MediaRef, Message as ChatMessage, MsgKey};
use crate::store::Store;

use super::App;

/// Re-uploaded media references not yet applied to the open conversation,
/// by `(chat, message id)`.
pub type FreshMedia = Arc<Mutex<HashMap<(String, String), MediaRef>>>;

/// What a download needs to ask for a re-upload of its message's media.
#[derive(Clone)]
pub struct MediaOrigin {
    /// Chat as cached (display JID).
    pub chat: String,
    /// Message id.
    pub id: String,
    /// The message key with the JIDs it came from (LIDs included).
    pub wire: MsgKey,
    store: Store,
    fresh: FreshMedia,
}

impl std::fmt::Debug for MediaOrigin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "MediaOrigin({:?})", self.wire)
    }
}

/// A user-facing reason for a failed download (no paths, keys or JIDs).
fn reason(e: &Error) -> String {
    match e {
        Error::MediaExpired | Error::MediaGone(_) => e.user_message(),
        other => other.to_string(),
    }
}

/// Downloads `mref`; on an expired CDN copy asks for a re-upload (when the
/// message is known) and tries once more with the new path.
pub async fn download(
    client: &ClientHandle,
    mref: &MediaRef,
    origin: Option<&MediaOrigin>,
) -> Result<Vec<u8>, String> {
    match client.download(mref).await {
        Ok(bytes) => Ok(bytes),
        Err(Error::MediaExpired) => {
            let Some(origin) = origin else {
                return Err(reason(&Error::MediaExpired));
            };
            let fresh = reupload(client, mref, origin).await?;
            client.download(&fresh).await.map_err(|e| reason(&e))
        }
        Err(e) => Err(reason(&e)),
    }
}

/// Asks for a re-upload of `origin`'s media, caches the new reference and
/// parks it for the open conversation.
pub async fn reupload(
    client: &ClientHandle,
    mref: &MediaRef,
    origin: &MediaOrigin,
) -> Result<MediaRef, String> {
    tracing::info!(kind = ?mref.kind, "media expired on the CDN, asking for a re-upload");
    let fresh = client.reupload(&origin.wire, mref).await.map_err(|e| {
        tracing::warn!(error = %e, "media re-upload failed");
        reason(&e)
    })?;
    if let Ok(mut m) = origin.fresh.lock() {
        m.insert((origin.chat.clone(), origin.id.clone()), fresh.clone());
    }
    let (store, chat, id, saved) = (
        origin.store.clone(),
        origin.chat.clone(),
        origin.id.clone(),
        fresh.clone(),
    );
    let stored = tokio::task::spawn_blocking(move || store.set_media_ref(&chat, &id, &saved)).await;
    if !matches!(stored, Ok(Ok(_))) {
        tracing::warn!("re-uploaded media path not cached");
    }
    Ok(fresh)
}

impl App {
    /// Where a download of `msg`'s media can ask for a re-upload.
    pub(crate) fn media_origin(&self, msg: &ChatMessage) -> MediaOrigin {
        let wire = |j: &str| super::state::wire_jid(&self.wire, j).to_string();
        MediaOrigin {
            chat: msg.chat.clone(),
            id: msg.id.clone(),
            wire: MsgKey {
                chat: wire(&msg.chat),
                id: msg.id.clone(),
                from_me: msg.from_me,
                sender: wire(&msg.sender),
            },
            store: self.store.clone(),
            fresh: self.fresh_media.clone(),
        }
    }

    /// Points open messages at media re-uploaded by finished tasks.
    pub(crate) fn apply_fresh_media(&mut self) {
        let fresh: Vec<_> = match self.fresh_media.lock() {
            Ok(mut m) if !m.is_empty() => m.drain().collect(),
            _ => return,
        };
        let Some(conv) = self.open.as_mut() else {
            return;
        };
        for ((chat, id), mref) in fresh {
            if conv.chat != chat {
                continue;
            }
            if let Some(m) = conv.messages.iter_mut().find(|m| m.id == id)
                && let Some(slot) = m.content.media_mut()
            {
                *slot = mref;
            }
        }
    }
}
