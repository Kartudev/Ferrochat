//! Live protocol client on top of `whatsapp-rust`.
//!
//! * The library's Signal/session state lives in a SQLCipher database
//!   (`session.sqlcipher`). Its 256-bit raw key is generated on first run and
//!   kept in the OS keyring (see [`crate::auth::secrets::signal_store_key`]),
//!   so only ciphertext touches the disk.
//! * Library events are forwarded through a channel and converted to
//!   [`super::Event`]s by the pure functions in [`super::convert`].

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use diesel::RunQueryDsl;
use futures::future::BoxFuture;
use futures::stream::Stream;
use tokio::sync::mpsc;
use whatsapp_rust::bot::{Bot, BotHandle};
use whatsapp_rust::download::{DownloadParams, MediaType};
use whatsapp_rust::prelude::{Event as WaEvent, EventHandler};
use whatsapp_rust::send::SendOptions;
use whatsapp_rust::upload::UploadOptions;
use whatsapp_rust::wacore::net::HttpRequest;
use whatsapp_rust::wacore::store::DevicePropsOverride;
use whatsapp_rust::wacore_binary::jid::JidExt;
use whatsapp_rust::{Client, Jid};
use whatsapp_rust_sqlite_storage::{SqliteStore, SqliteStoreConfig};

use super::call::{self, CallAudio, CallBook};
use super::convert::{self, Envelope, Uploaded};
use super::social::{CommunityInfo, PhoneLookup};
use super::types::{
    ChatState, CollectionResync, ContactInfo, Content, GroupInfo, ListDef, MUTE_OFF, MediaKind,
    MediaRef, Message, MsgKey, NameSource, Origin, OutgoingKind, OutgoingMedia, Quote,
    ResyncReport,
};
use super::{Connection, Event, WaClient};
use crate::auth::secrets::{SecretStore, get_signal_store_key, signal_store_key};
use crate::error::{Error, MaskedJid, Result};
use zeroize::Zeroizing;

/// Live client handle.
pub struct RealClient {
    client: Arc<Client>,
    handle: Mutex<Option<BotHandle>>,
    signal_db: PathBuf,
    /// `contact` app-state mutations seen (counted where the library
    /// dispatches them, so a resync can tell whether the address book came).
    contacts_seen: Arc<AtomicUsize>,
    /// "Read receipts" privacy value and when it was fetched.
    read_receipts: Mutex<Option<(std::time::Instant, bool)>>,
    /// Ringing offers and live calls.
    calls: CallBook,
}

/// How long the fetched "read receipts" privacy value is trusted.
const PRIVACY_TTL: std::time::Duration = std::time::Duration::from_secs(10 * 60);

impl RealClient {
    /// Call events of one library event (ringing offers are kept for
    /// Accept / Decline).
    fn call_events(&self, ev: &WaEvent) -> Vec<Event> {
        match ev {
            WaEvent::IncomingCall(c) => {
                let signal = call::signal_from_call(c);
                self.calls.observe(c, signal.as_ref());
                signal.map(Event::Call).into_iter().collect()
            }
            WaEvent::MissedCall(m) => {
                self.calls.forget(&m.call_id);
                vec![Event::Call(call::signal_from_missed(m))]
            }
            WaEvent::CallEndedElsewhere(e) => {
                self.calls.forget(&e.call_id);
                vec![Event::Call(call::signal_from_elsewhere(e))]
            }
            _ => Vec::new(),
        }
    }

    /// Is the account's "read receipts" privacy setting on? Cached for a few
    /// minutes; an unreadable setting counts as "off" (never leak a view).
    async fn read_receipts_on(&self) -> bool {
        if let Ok(g) = self.read_receipts.lock()
            && let Some((at, v)) = *g
            && at.elapsed() < PRIVACY_TTL
        {
            return v;
        }
        use whatsapp_rust::wacore::iq::privacy::{PrivacyCategory, PrivacyValue};
        let on = match self.client.fetch_privacy_settings().await {
            Ok(s) => !matches!(
                s.get_value(&PrivacyCategory::ReadReceipts),
                Some(PrivacyValue::None)
            ),
            Err(e) => {
                tracing::warn!(error = %e, "could not read the read-receipts privacy setting");
                return false;
            }
        };
        if let Ok(mut g) = self.read_receipts.lock() {
            *g = Some((std::time::Instant::now(), on));
        }
        on
    }
}

fn parse_jid(s: &str) -> Result<Jid> {
    s.parse::<Jid>()
        .map_err(|e| Error::Protocol(format!("invalid jid {}: {e}", MaskedJid(s))))
}

fn media_type(kind: MediaKind) -> MediaType {
    match kind {
        MediaKind::Image => MediaType::Image,
        MediaKind::Document => MediaType::Document,
        MediaKind::Audio => MediaType::Audio,
        MediaKind::Video => MediaType::Video,
        MediaKind::Sticker => MediaType::Sticker,
    }
}

/// Largest profile picture preview fetched (they are a few kB in practice).
pub const MAX_AVATAR_BYTES: usize = 1024 * 1024;

impl WaClient for RealClient {
    fn send_text<'a>(
        &'a self,
        chat: &'a str,
        id: &'a str,
        text: &'a str,
        reply: Option<&'a Quote>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let jid = parse_jid(chat)?;
            let msg = match reply {
                Some(q) => convert::build_reply(text, q),
                None => convert::build_text(text),
            };
            self.client
                .send_message_with_options(jid, msg, SendOptions::default().with_message_id(id))
                .await
                .map_err(|e| Error::Net(e.to_string()))?;
            Ok(())
        })
    }

    fn send_media<'a>(
        &'a self,
        chat: &'a str,
        id: &'a str,
        media: OutgoingMedia,
    ) -> BoxFuture<'a, Result<Content>> {
        Box::pin(async move {
            let jid = parse_jid(chat)?;
            let mt = match media.kind {
                OutgoingKind::Image { .. } => MediaType::Image,
                OutgoingKind::Document => MediaType::Document,
                OutgoingKind::Video { .. } => MediaType::Video,
                OutgoingKind::Voice { .. } => MediaType::Audio,
            };
            let up = self
                .client
                .upload(media.bytes.clone(), mt, UploadOptions::default())
                .await
                .map_err(|e| Error::Net(format!("upload failed: {e}")))?;
            let uploaded = Uploaded {
                url: up.url,
                direct_path: up.direct_path,
                media_key: up.media_key.to_vec(),
                file_sha256: up.file_sha256.to_vec(),
                file_enc_sha256: up.file_enc_sha256.to_vec(),
                file_length: up.file_length,
                media_key_timestamp: up.media_key_timestamp,
                streaming_sidecar: up.streaming_sidecar,
            };
            let (msg, content) = convert::build_media(&media, &uploaded);
            self.client
                .send_message_with_options(jid, msg, SendOptions::default().with_message_id(id))
                .await
                .map_err(|e| Error::Net(e.to_string()))?;
            Ok(content)
        })
    }

    fn forward<'a>(
        &'a self,
        chat: &'a str,
        id: &'a str,
        source: &'a Message,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let jid = parse_jid(chat)?;
            let msg = convert::build_forward(&source.content, source.forwarded)
                .ok_or_else(|| Error::Protocol("this message can't be forwarded".into()))?;
            self.client
                .send_message_with_options(jid, msg, SendOptions::default().with_message_id(id))
                .await
                .map_err(|e| Error::Net(e.to_string()))?;
            Ok(())
        })
    }

    fn download<'a>(&'a self, media: &'a MediaRef) -> BoxFuture<'a, Result<Vec<u8>>> {
        Box::pin(async move {
            crate::media::check_download_size(media.file_length)?;
            let params = DownloadParams::encrypted(
                media.direct_path.clone(),
                &media.media_key,
                &media.file_sha256,
                &media.file_enc_sha256,
                media.file_length,
                media_type(media.kind),
            );
            let bytes = self
                .client
                .download_from_params(&params)
                .await
                .map_err(|e| {
                    use whatsapp_rust::ErrorChainExt as _;
                    // 404 / 410 after the library refreshed its media
                    // connection: the blob is gone from the CDN.
                    let chain: &(dyn std::error::Error + Send + Sync + 'static) = e.as_ref();
                    if matches!(chain.http_status(), Some(404 | 410)) {
                        Error::MediaExpired
                    } else {
                        Error::Net(format!("download failed: {e}"))
                    }
                })?;
            crate::media::check_download_size(bytes.len() as u64)?;
            Ok(bytes)
        })
    }

    fn reupload<'a>(
        &'a self,
        key: &'a MsgKey,
        media: &'a MediaRef,
    ) -> BoxFuture<'a, Result<MediaRef>> {
        Box::pin(async move {
            use whatsapp_rust::{MediaRetryResult, MediaReuploadRequest};
            let chat = parse_jid(&key.chat)?;
            // Groups name the author; a direct chat is the author itself.
            let participant = if super::is_group_jid(&key.chat) && !key.from_me {
                Some(parse_jid(&key.sender)?)
            } else {
                None
            };
            let result = self
                .client
                .media_reupload()
                .request(&MediaReuploadRequest {
                    msg_id: &key.id,
                    chat_jid: &chat,
                    media_key: &media.media_key,
                    is_from_me: key.from_me,
                    participant: participant.as_ref(),
                })
                .await
                .map_err(|e| Error::Net(format!("media re-upload failed: {e}")))?;
            match result {
                MediaRetryResult::Success { direct_path } => Ok(MediaRef {
                    direct_path,
                    ..media.clone()
                }),
                MediaRetryResult::NotFound => Err(Error::MediaGone("not found".into())),
                MediaRetryResult::DecryptionError => {
                    Err(Error::MediaGone("decryption error".into()))
                }
                MediaRetryResult::GeneralError => Err(Error::MediaGone("general error".into())),
            }
        })
    }

    fn mark_read<'a>(
        &'a self,
        chat: &'a str,
        sender: Option<&'a str>,
        ids: &'a [String],
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let chat = parse_jid(chat)?;
            let sender = sender.map(parse_jid).transpose()?;
            let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
            self.client
                .mark_as_read(&chat, sender.as_ref(), &ids)
                .await
                .map_err(|e| Error::Net(e.to_string()))
        })
    }

    fn mark_played<'a>(
        &'a self,
        chat: &'a str,
        sender: Option<&'a str>,
        ids: &'a [String],
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let chat = parse_jid(chat)?;
            let sender = sender.map(parse_jid).transpose()?;
            let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
            self.client
                .mark_as_played(&chat, sender.as_ref(), &ids)
                .await
                .map_err(|e| Error::Net(e.to_string()))
        })
    }

    fn profile_picture<'a>(&'a self, jid: &'a str) -> BoxFuture<'a, Result<Option<Vec<u8>>>> {
        Box::pin(async move {
            let jid = parse_jid(jid)?;
            let pic = self
                .client
                .contacts()
                .get_profile_picture(&jid, true)
                .await
                .map_err(|e| Error::Net(format!("profile picture lookup failed: {e}")))?;
            let Some(pic) = pic else { return Ok(None) };
            let resp = self
                .client
                .http_client
                .execute(HttpRequest::get(pic.url))
                .await
                .map_err(|e| Error::Net(format!("profile picture download failed: {e}")))?;
            if resp.status_code == 404 || resp.status_code == 410 {
                return Ok(None);
            }
            if resp.status_code != 200 {
                return Err(Error::Net(format!(
                    "profile picture download failed (HTTP {})",
                    resp.status_code
                )));
            }
            if resp.body.len() > MAX_AVATAR_BYTES {
                return Ok(None);
            }
            Ok(Some(resp.body))
        })
    }

    fn send_reaction<'a>(&'a self, key: &'a MsgKey, emoji: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let chat = parse_jid(&key.chat)?;
            let wa_key = convert::reaction_key(
                &key.chat,
                &key.id,
                key.from_me,
                &key.sender,
                chat.is_group(),
            );
            self.client
                .send_reaction(chat, wa_key, emoji)
                .await
                .map_err(|e| Error::Net(e.to_string()))?;
            Ok(())
        })
    }

    fn edit_text<'a>(
        &'a self,
        chat: &'a str,
        id: &'a str,
        text: &'a str,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let jid = parse_jid(chat)?;
            self.client
                .edit_message(jid, id, convert::build_text(text))
                .await
                .map_err(|e| Error::Net(e.to_string()))?;
            Ok(())
        })
    }

    fn revoke<'a>(&'a self, chat: &'a str, id: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let jid = parse_jid(chat)?;
            self.client
                .revoke_message(jid, id, whatsapp_rust::send::RevokeType::Sender)
                .await
                .map_err(|e| Error::Net(e.to_string()))
        })
    }

    fn delete_for_me<'a>(&'a self, key: &'a MsgKey, timestamp: i64) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let chat = parse_jid(&key.chat)?;
            let participant = (chat.is_group() && !key.from_me)
                .then(|| parse_jid(&key.sender))
                .transpose()?;
            self.client
                .chat_actions()
                .delete_message_for_me(
                    &chat,
                    participant.as_ref(),
                    &key.id,
                    key.from_me,
                    false,
                    Some(timestamp),
                )
                .await
                .map_err(|e| Error::Net(e.to_string()))
        })
    }

    fn send_chat_state<'a>(&'a self, chat: &'a str, state: ChatState) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let jid = parse_jid(chat)?;
            let cs = self.client.chatstate();
            match state {
                ChatState::Composing => cs.send_composing(&jid).await,
                ChatState::Recording => cs.send_recording(&jid).await,
                ChatState::Paused => cs.send_paused(&jid).await,
            }
            .map_err(|e| Error::Net(e.to_string()))
        })
    }

    fn subscribe_presence<'a>(&'a self, jid: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let jid = parse_jid(jid)?;
            self.client
                .presence()
                .subscribe(jid)
                .await
                .map_err(|e| Error::Net(e.to_string()))
        })
    }

    fn group_size<'a>(&'a self, chat: &'a str) -> BoxFuture<'a, Result<usize>> {
        Box::pin(async move {
            let jid = parse_jid(chat)?;
            let info = self
                .client
                .groups()
                .query_info(&jid)
                .await
                .map_err(|e| Error::Net(e.to_string()))?;
            let (pn, lid) = (self.client.pn(), self.client.lid());
            let others = info
                .participants
                .iter()
                .filter(|p| {
                    !pn.as_ref().is_some_and(|me| me.is_same_user_as(*p))
                        && !lid.as_ref().is_some_and(|me| me.is_same_user_as(*p))
                })
                .count();
            Ok(others)
        })
    }

    fn set_archived<'a>(&'a self, chat: &'a str, archived: bool) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let jid = parse_jid(chat)?;
            let actions = self.client.chat_actions();
            if archived {
                actions.archive_chat(&jid, None).await
            } else {
                actions.unarchive_chat(&jid, None).await
            }
            .map_err(|e| Error::Net(e.to_string()))
        })
    }

    fn set_pinned<'a>(&'a self, chat: &'a str, pinned: bool) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let jid = parse_jid(chat)?;
            let actions = self.client.chat_actions();
            if pinned {
                actions.pin_chat(&jid).await
            } else {
                actions.unpin_chat(&jid).await
            }
            .map_err(|e| Error::Net(e.to_string()))
        })
    }

    fn save_list<'a>(&'a self, list: &'a ListDef) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            use whatsapp_rust::waproto::buffa::MessageField;
            use whatsapp_rust::waproto::whatsapp as wa;
            let value = wa::SyncActionValue {
                label_edit_action: MessageField::some(convert::list_edit_action(list)),
                timestamp: Some(now_secs() * 1000),
                ..Default::default()
            };
            self.client
                .send_app_state_action(&whatsapp_rust::schemas::LABEL_EDIT, &[&list.id], &value)
                .await
                .map_err(|e| Error::Net(e.to_string()))
        })
    }

    fn delete_list<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.client
                .labels()
                .delete_label(id)
                .await
                .map_err(|e| Error::Net(e.to_string()))
        })
    }

    fn set_list_member<'a>(
        &'a self,
        list: &'a str,
        chat: &'a str,
        member: bool,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let jid = parse_jid(chat)?;
            let labels = self.client.labels();
            if member {
                labels.add_chat_label(list, &jid).await
            } else {
                labels.remove_chat_label(list, &jid).await
            }
            .map_err(|e| Error::Net(e.to_string()))
        })
    }

    fn resync_app_state(&self) -> BoxFuture<'_, Result<ResyncReport>> {
        Box::pin(async move {
            use whatsapp_rust::sync_task::MajorSyncTask;
            use whatsapp_rust::wacore::appstate::hash::HashState;
            use whatsapp_rust::wacore::appstate::patch_decode::WAPatchName;
            // Contacts live in `critical_unblock_low`; archive, pin, mute,
            // read state and labels in `regular_low` / `regular` /
            // `regular_high`.
            //
            // `full_sync: true` alone is a no-op on a collection that is
            // already synced: wacore 0.7 drops a snapshot whose version is
            // not newer than the persisted one ("Skipping stale snapshot",
            // `appstate_sync::snapshot_is_stale`), so no mutation is replayed
            // and no `ContactUpdate` is emitted. Resetting the persisted
            // version (and the mutation MACs that belong to it) first makes
            // the snapshot the new baseline, exactly as for a freshly linked
            // device. If the request fails half-way the version stays 0 and
            // the next sync of that collection asks for a snapshot again.
            let backend = self.client.persistence_manager().backend();
            let had_sync_key = backend
                .get_latest_sync_key_id()
                .await
                .map(|k| k.is_some())
                .unwrap_or(false);
            if !had_sync_key {
                tracing::warn!(
                    "app-state resync: no app-state sync key stored yet; the library asks the \
                     phone for it (AppStateSyncKeyRequest), the phone must be online"
                );
            }
            let contacts_before = self.contacts_seen.load(Ordering::Relaxed);
            let mut collections = Vec::new();
            for name in [
                WAPatchName::CriticalUnblockLow,
                WAPatchName::RegularLow,
                WAPatchName::Regular,
                WAPatchName::RegularHigh,
            ] {
                let before = backend
                    .get_version(name.as_str())
                    .await
                    .map(|s| s.version)
                    .unwrap_or(0);
                if before > 0 {
                    if let Err(e) = backend.clear_mutation_macs(name.as_str()).await {
                        tracing::warn!(
                            "app-state resync: {}: cannot reset MACs: {e}",
                            name.as_str()
                        );
                        collections.push(CollectionResync {
                            name: name.as_str(),
                            before,
                            after: before,
                        });
                        continue;
                    }
                    if let Err(e) = backend
                        .set_version(name.as_str(), HashState::default())
                        .await
                    {
                        tracing::warn!(
                            "app-state resync: {}: cannot reset version: {e}",
                            name.as_str()
                        );
                        collections.push(CollectionResync {
                            name: name.as_str(),
                            before,
                            after: before,
                        });
                        continue;
                    }
                }
                self.client
                    .process_sync_task(MajorSyncTask::AppStateSync {
                        name,
                        full_sync: true,
                    })
                    .await;
                let after = backend
                    .get_version(name.as_str())
                    .await
                    .map(|s| s.version)
                    .unwrap_or(0);
                tracing::info!(
                    "app-state resync: {} version {before} -> {after} ({})",
                    name.as_str(),
                    if after > 0 {
                        "snapshot applied"
                    } else {
                        "not applied, will retry"
                    }
                );
                collections.push(CollectionResync {
                    name: name.as_str(),
                    before,
                    after,
                });
            }
            Ok(ResyncReport {
                collections,
                had_sync_key,
                contacts: self
                    .contacts_seen
                    .load(Ordering::Relaxed)
                    .saturating_sub(contacts_before),
            })
        })
    }

    fn known_lid_pairs(&self) -> BoxFuture<'_, Result<Vec<(String, String)>>> {
        Box::pin(async move {
            let backend = self.client.persistence_manager().backend();
            let rows = backend
                .get_all_lid_mappings()
                .await
                .map_err(|e| Error::Net(e.to_string()))?;
            Ok(rows
                .into_iter()
                .filter(|r| !r.lid.is_empty() && !r.phone_number.is_empty())
                .map(|r| {
                    (
                        format!("{}@lid", r.lid),
                        format!("{}@s.whatsapp.net", r.phone_number),
                    )
                })
                .collect())
        })
    }

    fn send_status_view<'a>(
        &'a self,
        author: &'a str,
        ids: &'a [String],
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async move {
            if ids.is_empty() || !self.read_receipts_on().await {
                return Ok(false);
            }
            let chat = Jid::status_broadcast();
            let sender = parse_jid(author)?;
            let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
            self.client
                .mark_as_read(&chat, Some(&sender), &ids)
                .await
                .map_err(|e| Error::Net(e.to_string()))?;
            Ok(true)
        })
    }

    fn set_starred<'a>(&'a self, key: &'a MsgKey, starred: bool) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let chat = parse_jid(&key.chat)?;
            let participant = (chat.is_group() && !key.from_me)
                .then(|| parse_jid(&key.sender))
                .transpose()?;
            let actions = self.client.chat_actions();
            if starred {
                actions
                    .star_message(&chat, participant.as_ref(), &key.id, key.from_me)
                    .await
            } else {
                actions
                    .unstar_message(&chat, participant.as_ref(), &key.id, key.from_me)
                    .await
            }
            .map_err(|e| Error::Net(e.to_string()))
        })
    }

    fn create_group<'a>(
        &'a self,
        subject: &'a str,
        participants: &'a [String],
        icon: Option<Vec<u8>>,
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            use whatsapp_rust::features::{GroupCreateOptions, GroupParticipantOptions};
            let mut parts = Vec::with_capacity(participants.len());
            for p in participants {
                parts.push(GroupParticipantOptions::from_phone(parse_jid(p)?));
            }
            let opts = GroupCreateOptions::new(subject).with_participants(parts);
            let made = self
                .client
                .groups()
                .create_group(opts)
                .await
                .map_err(|e| Error::Net(format!("could not create the group: {e}")))?;
            let jid = made.metadata.id.clone();
            if let Some(jpeg) = icon {
                // The group exists either way; a failed icon is only logged.
                if let Err(e) = self
                    .client
                    .groups()
                    .set_profile_picture(jid.clone(), jpeg)
                    .await
                {
                    tracing::warn!(error = %e, "group icon was not set");
                }
            }
            Ok(convert::bare_jid(&jid.to_string()))
        })
    }

    fn create_community<'a>(
        &'a self,
        name: &'a str,
        description: &'a str,
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            use whatsapp_rust::features::CreateCommunityOptions;
            let mut opts = CreateCommunityOptions::new(name);
            opts.description = Some(description.to_string()).filter(|d| !d.is_empty());
            let made = self
                .client
                .community()
                .create(opts)
                .await
                .map_err(|e| Error::Net(format!("could not create the community: {e}")))?;
            Ok(convert::bare_jid(&made.metadata.id.to_string()))
        })
    }

    fn communities(&self) -> BoxFuture<'_, Result<Vec<CommunityInfo>>> {
        Box::pin(async move {
            let parents = self
                .client
                .community()
                .get_participating()
                .await
                .map_err(|e| Error::Net(format!("communities: {e}")))?;
            // Every group we are in: subgroups we joined, and the fallback
            // source when the subgroup query fails.
            let groups = self
                .client
                .groups()
                .get_participating()
                .await
                .map_err(|e| Error::Net(format!("groups: {e}")))?;
            let mut out = Vec::new();
            for (jid, meta) in parents.iter() {
                let known = self.client.community().get_subgroups(jid).await.ok();
                out.push(convert::community_info(
                    jid,
                    meta,
                    known.as_deref(),
                    &groups,
                ));
            }
            Ok(out)
        })
    }

    fn check_numbers<'a>(
        &'a self,
        digits: &'a [String],
    ) -> BoxFuture<'a, Result<Vec<PhoneLookup>>> {
        Box::pin(async move {
            let jids: Vec<Jid> = digits.iter().map(|d| Jid::pn(d.as_str())).collect();
            let found = self
                .client
                .contacts()
                .is_on_whatsapp(&jids)
                .await
                .map_err(|e| Error::Net(format!("number check failed: {e}")))?;
            Ok(digits
                .iter()
                .map(|d| convert::phone_lookup(d, &found))
                .collect())
        })
    }

    fn post_status_text<'a>(
        &'a self,
        text: &'a str,
        background: u32,
        font: i32,
        recipients: &'a [String],
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            use whatsapp_rust::buffa::Enumeration;
            use whatsapp_rust::waproto::whatsapp::message::extended_text_message::FontType;
            let mut to = Vec::with_capacity(recipients.len());
            for r in recipients {
                to.push(parse_jid(r)?);
            }
            // Unknown font ids fall back to the system font.
            let font = FontType::from_i32(font).unwrap_or(FontType::SYSTEM);
            let sent = self
                .client
                .status()
                .send_text(text, background, font, &to, Default::default())
                .await
                .map_err(|e| Error::Net(format!("could not post the status: {e}")))?;
            Ok(sent.message_id)
        })
    }

    fn reject_call<'a>(&'a self, call_id: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move { self.calls.reject(&self.client, call_id).await })
    }

    fn accept_call<'a>(&'a self, call_id: &'a str, audio: CallAudio) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move { self.calls.accept(&self.client, call_id, audio).await })
    }

    fn start_call<'a>(&'a self, peer: &'a str, audio: CallAudio) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let jid = parse_jid(peer)?;
            self.calls.start(&self.client, &jid, audio).await
        })
    }

    fn end_call<'a>(&'a self, call_id: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move { self.calls.end(&self.client, call_id).await })
    }

    fn set_call_muted(&self, call_id: &str, muted: bool) {
        self.calls.set_muted(call_id, muted);
    }

    fn contact_info<'a>(&'a self, jid: &'a str) -> BoxFuture<'a, Result<ContactInfo>> {
        Box::pin(async move {
            let j = parse_jid(jid)?;
            let info = self
                .client
                .contacts()
                .get_user_info(std::slice::from_ref(&j))
                .await
                .map_err(|e| Error::Net(format!("contact info: {e}")))?;
            let u = info
                .values()
                .find(|u| u.jid.user == j.user || u.lid.as_ref().is_some_and(|l| l.user == j.user))
                .or_else(|| info.values().next());
            Ok(match u {
                Some(u) => convert::contact_info(
                    u.status.as_deref(),
                    u.verified_name.as_ref().and_then(|v| v.name.as_deref()),
                ),
                None => ContactInfo::default(),
            })
        })
    }

    fn group_info<'a>(&'a self, chat: &'a str) -> BoxFuture<'a, Result<GroupInfo>> {
        Box::pin(async move {
            let jid = parse_jid(chat)?;
            let meta = self
                .client
                .groups()
                .get_metadata(&jid)
                .await
                .map_err(|e| Error::Net(format!("group info: {e}")))?;
            Ok(convert::group_info(&meta))
        })
    }

    fn common_groups<'a>(
        &'a self,
        ids: &'a [String],
    ) -> BoxFuture<'a, Result<Vec<(String, String)>>> {
        Box::pin(async move {
            let groups = self
                .client
                .groups()
                .get_participating()
                .await
                .map_err(|e| Error::Net(format!("groups: {e}")))?;
            Ok(convert::common_groups(&groups, ids))
        })
    }

    fn is_blocked<'a>(&'a self, jid: &'a str) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async move {
            let j = parse_jid(jid)?;
            self.client
                .blocking()
                .is_blocked(&j)
                .await
                .map_err(|e| Error::Net(format!("block list: {e}")))
        })
    }

    fn set_blocked<'a>(&'a self, jid: &'a str, blocked: bool) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let j = parse_jid(jid)?;
            let b = self.client.blocking();
            if blocked {
                b.block(&j).await
            } else {
                b.unblock(&j).await
            }
            .map_err(|e| Error::Net(format!("block: {e}")))
        })
    }

    fn set_mute<'a>(&'a self, chat: &'a str, end: i64) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let jid = parse_jid(chat)?;
            let actions = self.client.chat_actions();
            if end == MUTE_OFF {
                actions.unmute_chat(&jid).await
            } else if end < 0 {
                actions.mute_chat(&jid).await
            } else {
                // The protocol counts the mute end in milliseconds.
                actions
                    .mute_chat_until(&jid, end.saturating_mul(1000))
                    .await
            }
            .map_err(|e| Error::Net(e.to_string()))
        })
    }

    fn mark_chat_read<'a>(
        &'a self,
        chat: &'a str,
        read: bool,
        last: Option<&'a (MsgKey, i64)>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let jid = parse_jid(chat)?;
            let range = message_range(&jid, last)?;
            self.client
                .chat_actions()
                .mark_chat_as_read(&jid, read, range)
                .await
                .map_err(|e| Error::Net(e.to_string()))
        })
    }

    fn clear_chat<'a>(
        &'a self,
        chat: &'a str,
        keep_starred: bool,
        last: Option<&'a (MsgKey, i64)>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let jid = parse_jid(chat)?;
            let range = message_range(&jid, last)?;
            self.client
                .chat_actions()
                .clear_chat(&jid, !keep_starred, false, range)
                .await
                .map_err(|e| Error::Net(e.to_string()))
        })
    }

    fn delete_chat<'a>(
        &'a self,
        chat: &'a str,
        last: Option<&'a (MsgKey, i64)>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let jid = parse_jid(chat)?;
            let range = message_range(&jid, last)?;
            self.client
                .chat_actions()
                .delete_chat(&jid, false, range)
                .await
                .map_err(|e| Error::Net(e.to_string()))
        })
    }

    fn logout(&self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            // Deregister (best effort: it cannot reach the server offline),
            // then stop the client, and only then delete its database.
            if tokio::time::timeout(LOGOUT_TIMEOUT, self.client.logout())
                .await
                .is_err()
            {
                tracing::warn!("logout request timed out; unlinking locally");
            }
            let handle = self.handle.lock().ok().and_then(|mut h| h.take());
            if let Some(h) = handle
                && tokio::time::timeout(LOGOUT_TIMEOUT, h.shutdown())
                    .await
                    .is_err()
            {
                tracing::warn!("client shutdown timed out");
            }
            remove_db(&self.signal_db)
        })
    }
}

/// Upper bound for the logout IQ and the client shutdown.
pub const LOGOUT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);

/// Removes the encrypted session database and its WAL/SHM side files.
/// Missing files are fine; any other failure is reported.
pub fn remove_db(path: &Path) -> Result<()> {
    let mut first_err = None;
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let mut p = path.as_os_str().to_owned();
        p.push(suffix);
        match std::fs::remove_file(PathBuf::from(p)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                first_err.get_or_insert(e);
            }
        }
    }
    match first_err {
        Some(e) => Err(Error::Io(e)),
        None => Ok(()),
    }
}

/// Library event handler: forwards into our channel, conversion happens on
/// our own task so the dispatch path stays cheap.
struct Forwarder {
    tx: mpsc::UnboundedSender<Arc<WaEvent>>,
    contacts_seen: Arc<AtomicUsize>,
}

impl EventHandler for Forwarder {
    fn handle_event(&self, event: Arc<WaEvent>) {
        if matches!(&*event, WaEvent::ContactUpdate(_)) {
            self.contacts_seen.fetch_add(1, Ordering::Relaxed);
        }
        let _ = self.tx.send(event);
    }
}

/// Why the session store could not be opened.
#[derive(Debug)]
pub enum SessionError {
    /// The file is not decryptable with this key (SQLITE_NOTADB): the key
    /// was lost or replaced. Only the user may decide to discard it.
    KeyMismatch,
    /// Anything else (I/O, permissions, busy, migrations): never a reason
    /// to delete the session.
    Other(Error),
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionError::KeyMismatch => f.write_str("session store key mismatch"),
            SessionError::Other(e) => write!(f, "{e}"),
        }
    }
}

fn pragma_key(key_hex: &str) -> Zeroizing<String> {
    Zeroizing::new(format!("PRAGMA key = \"x'{key_hex}'\";"))
}

/// Opens the SQLCipher-encrypted session store with a hex raw key. Never
/// deletes anything.
pub async fn open_store(
    path: &Path,
    key_hex: Zeroizing<String>,
) -> std::result::Result<SqliteStore, SessionError> {
    if path.exists() {
        // Fail fast on a wrong key: the library's pool would otherwise keep
        // retrying the connection for its full timeout.
        let (p, k) = (path.to_path_buf(), key_hex.clone());
        tokio::task::spawn_blocking(move || verify_key(&p, &k))
            .await
            .map_err(|_| SessionError::Other(Error::Cancelled))??;
    }
    let url = path.to_string_lossy().to_string();
    let config = SqliteStoreConfig::default().with_connection_init(move |conn| {
        // Raw-key form: no PBKDF2 at every open, the key is already 256 random bits.
        diesel::sql_query(pragma_key(&key_hex).as_str()).execute(conn)?;
        // Fails here (not later) when the key does not match the file.
        diesel::sql_query("SELECT count(*) FROM sqlite_master;").execute(conn)?;
        Ok(())
    });
    SqliteStore::with_config(&url, config)
        .await
        .map_err(|e| SessionError::Other(Error::Auth(format!("cannot open session store: {e}"))))
}

/// Checks the key against an existing file. Only `SQLITE_NOTADB` (what
/// SQLCipher returns for a wrong key) counts as a key mismatch.
pub fn verify_key(path: &Path, key_hex: &str) -> std::result::Result<(), SessionError> {
    let classify = |e: rusqlite::Error| match e.sqlite_error_code() {
        Some(rusqlite::ErrorCode::NotADatabase) => SessionError::KeyMismatch,
        _ => SessionError::Other(Error::Store(e)),
    };
    let conn = rusqlite::Connection::open(path).map_err(classify)?;
    conn.execute_batch(&pragma_key(key_hex)).map_err(classify)?;
    conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| {
        r.get::<_, i64>(0)
    })
    .map_err(classify)?;
    Ok(())
}

/// Starts the live client and returns its event stream. The first event is
/// [`Event::SessionFound`] / [`Event::NoSession`], then [`Event::Ready`].
pub fn start(
    data_dir: PathBuf,
    signal_db: PathBuf,
    secrets: Arc<dyn SecretStore>,
) -> impl Stream<Item = Event> + Send + 'static {
    let (tx, rx) = mpsc::unbounded_channel::<Event>();
    tokio::spawn(async move {
        if let Err(e) = run(data_dir, signal_db, secrets, tx.clone()).await {
            tracing::error!(error = %e, "protocol client stopped");
            let _ = tx.send(Event::Fatal(e.user_message()));
        }
    });
    futures::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|e| (e, rx)) })
}

/// Loads the key and opens the store. Returns `Ok(None)` after emitting
/// [`Event::SessionUnreadable`] when the user has to decide.
pub async fn prepare_session(
    data_dir: &Path,
    signal_db: &Path,
    secrets: Arc<dyn SecretStore>,
    tx: &mpsc::UnboundedSender<Event>,
) -> Result<Option<SqliteStore>> {
    crate::config::secure_dir(data_dir)?;
    let had_session = signal_db.exists();
    let key = tokio::task::spawn_blocking(move || {
        if had_session {
            // Never mint a new key over an existing session: that would make
            // it unreadable. Missing/malformed key → ask the user.
            get_signal_store_key(secrets.as_ref())
        } else {
            signal_store_key(secrets.as_ref()).map(|(k, _)| Some(k))
        }
    })
    .await
    .map_err(|_| Error::Cancelled)??;
    let Some(key) = key else {
        let _ = tx.send(Event::SessionUnreadable(
            "The key for your saved session is missing from the system keyring.".into(),
        ));
        return Ok(None);
    };
    let _ = tx.send(if had_session {
        Event::SessionFound
    } else {
        Event::NoSession
    });
    let _ = tx.send(Event::Connection(Connection::Connecting));
    match open_store(signal_db, Zeroizing::new(key.expose().to_string())).await {
        Ok(store) => {
            crate::config::secure_db_files(signal_db);
            Ok(Some(store))
        }
        Err(SessionError::KeyMismatch) => {
            let _ = tx.send(Event::SessionUnreadable(
                "Your saved session cannot be decrypted with the key in the system keyring.".into(),
            ));
            Ok(None)
        }
        Err(SessionError::Other(e)) => Err(e),
    }
}

async fn run(
    data_dir: PathBuf,
    signal_db: PathBuf,
    secrets: Arc<dyn SecretStore>,
    tx: mpsc::UnboundedSender<Event>,
) -> Result<()> {
    let Some(store) = prepare_session(&data_dir, &signal_db, secrets, &tx).await? else {
        // Wait for the UI to restart us after the user decided.
        tx.closed().await;
        return Ok(());
    };

    let (wa_tx, mut wa_rx) = mpsc::unbounded_channel::<Arc<WaEvent>>();
    let contacts_seen = Arc::new(AtomicUsize::new(0));
    let bot = Bot::builder()
        .with_backend(store)
        .with_device_props(device_props())
        .with_event_handler(Forwarder {
            tx: wa_tx,
            contacts_seen: contacts_seen.clone(),
        })
        .build()
        .await
        .map_err(|e| Error::Protocol(format!("client setup failed: {e}")))?;
    let client = bot.client();
    let handle = bot.spawn();
    let real = Arc::new(RealClient {
        client: client.clone(),
        handle: Mutex::new(Some(handle)),
        signal_db: signal_db.clone(),
        contacts_seen,
        read_receipts: Mutex::new(None),
        calls: CallBook::new(tx.clone()),
    });
    let _ = tx.send(Event::Ready(real.clone()));

    let mut attempt = 0u32;
    loop {
        tokio::select! {
            _ = tx.closed() => {
                // The UI dropped the subscription (logout / reload / exit).
                let handle = real.handle.lock().ok().and_then(|mut h| h.take());
                if let Some(h) = handle {
                    let _ = tokio::time::timeout(LOGOUT_TIMEOUT, h.shutdown()).await;
                }
                return Ok(());
            }
            ev = wa_rx.recv() => {
                let Some(ev) = ev else { return Ok(()) };
                let terminal = is_terminal(&ev);
                let mut outs = real.call_events(&ev);
                outs.extend(translate(&ev, &client, &mut attempt));
                for out in outs {
                    if tx.send(out).is_err() {
                        break;
                    }
                }
                if terminal {
                    // Stop the library's own reconnect loop; the app decides
                    // (backoff / user retry / re-pair).
                    let handle = real.handle.lock().ok().and_then(|mut h| h.take());
                    if let Some(h) = handle {
                        let _ = tokio::time::timeout(LOGOUT_TIMEOUT, h.shutdown()).await;
                    }
                    tx.closed().await;
                    return Ok(());
                }
            }
        }
    }
}

/// What this companion tells the phone about itself at pairing (shown in
/// the phone's "Linked devices"). `DESKTOP` is the protocol's category for a
/// native desktop client; the sync fields keep the library's default (the
/// same recent-history sync WhatsApp Web asks for). Only sent when a device
/// is linked: existing links keep the props they were paired with.
fn device_props() -> DevicePropsOverride {
    DevicePropsOverride::new()
        .with_os("Ferrochat")
        .with_platform_type(whatsapp_rust::waproto::whatsapp::device_props::PlatformType::DESKTOP)
}

/// Events after which the library must not keep reconnecting on its own.
fn is_terminal(ev: &WaEvent) -> bool {
    match ev {
        WaEvent::StreamReplaced(_)
        | WaEvent::LoggedOut(_)
        | WaEvent::TemporaryBan(_)
        | WaEvent::ClientOutdated(_) => true,
        WaEvent::ConnectFailure(f) => !f.reason.should_reconnect(),
        _ => false,
    }
}

/// The `messageRange` of a chat action: the newest message and its time,
/// so the phone applies the action only to what this device had seen.
fn message_range(
    chat: &Jid,
    last: Option<&(MsgKey, i64)>,
) -> Result<Option<whatsapp_rust::SyncActionMessageRange>> {
    let Some((key, ts)) = last else {
        return Ok(None);
    };
    let participant = (chat.is_group() && !key.from_me)
        .then(|| parse_jid(&key.sender))
        .transpose()?;
    let k = whatsapp_rust::message_key(key.id.clone(), chat, key.from_me, participant.as_ref());
    Ok(Some(whatsapp_rust::message_range(
        *ts,
        None,
        vec![(k, *ts)],
    )))
}

fn now_secs() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Max messages / chats per event, so the UI applies a big history sync in
/// slices between frames instead of freezing.
pub const HISTORY_CHUNK: usize = 500;

/// Splits a converted history blob into small events.
pub fn history_events(parts: convert::HistoryParts, progress: Option<u32>) -> Vec<Event> {
    let mut v = Vec::new();
    if !parts.lid_pairs.is_empty() {
        v.push(Event::LidMappings(parts.lid_pairs));
    }
    let mut chats = parts.chats.chunks(HISTORY_CHUNK).peekable();
    let mut first = true;
    while chats.peek().is_some() || first {
        let chunk = chats.next().map(<[_]>::to_vec).unwrap_or_default();
        v.push(Event::History {
            chats: chunk,
            names: if first {
                parts.names.clone()
            } else {
                Vec::new()
            },
            progress,
        });
        first = false;
    }
    for chunk in parts.messages.chunks(HISTORY_CHUNK) {
        v.push(Event::Messages {
            messages: chunk.to_vec(),
            origin: Origin::History,
        });
    }
    v
}

/// Library event → zero or more app events.
fn translate(ev: &WaEvent, client: &Client, attempt: &mut u32) -> Vec<Event> {
    match ev {
        WaEvent::PairingQrCode(q) => vec![Event::Qr {
            code: q.code.clone(),
            valid_for_secs: q.timeout.as_secs(),
        }],
        WaEvent::PairingQrCodesExhausted(_) => vec![Event::QrExhausted],
        WaEvent::PairSuccess(p) => vec![Event::PairSuccess {
            jid: convert::bare_jid(&p.id.to_string()),
        }],
        WaEvent::PairError(p) => vec![Event::PairError(p.error.clone())],
        WaEvent::Connected(_) => {
            *attempt = 0;
            vec![
                Event::Connection(Connection::Connected),
                Event::Connected {
                    jid: client.pn().map(|j| convert::bare_jid(&j.to_string())),
                },
            ]
        }
        WaEvent::Disconnected(_) | WaEvent::StreamError(_) => {
            *attempt += 1;
            vec![Event::Connection(Connection::Reconnecting {
                attempt: *attempt,
            })]
        }
        WaEvent::ConnectFailure(f) => {
            if f.reason.is_logged_out() {
                vec![Event::LoggedOut {
                    reason: "WhatsApp ended this session. Link the device again.".into(),
                }]
            } else if f.reason.should_reconnect() {
                *attempt += 1;
                vec![Event::Connection(Connection::Reconnecting {
                    attempt: *attempt,
                })]
            } else {
                vec![Event::Connection(Connection::Offline {
                    reason: format!("WhatsApp refused the connection ({:?}).", f.reason),
                })]
            }
        }
        WaEvent::LoggedOut(l) => vec![Event::LoggedOut {
            reason: format!("This device was unlinked ({:?}).", l.reason),
        }],
        // Not a logout: the session is still valid, another client took the
        // stream. Never wipe anything here.
        WaEvent::StreamReplaced(_) => vec![Event::Connection(Connection::Replaced)],
        WaEvent::TemporaryBan(_) => vec![Event::Fatal(
            "WhatsApp temporarily banned this account. Third-party clients break WhatsApp's \
             terms of service."
                .into(),
        )],
        WaEvent::ClientOutdated(_) => vec![Event::Fatal(
            "WhatsApp rejected this client version. Update Ferrochat.".into(),
        )],
        WaEvent::Messages(batch) => {
            let drained = !matches!(batch.origin, whatsapp_rust::prelude::BatchOrigin::Live)
                || batch.messages.iter().any(|m| m.info.is_offline);
            let own = client.pn().map(|j| convert::bare_jid(&j.to_string()));
            let mut out = Vec::new();
            let mut extra = Vec::new();
            let mut pairs = Vec::new();
            let mut business = Vec::new();
            let mut statuses = Vec::new();
            for m in batch.messages.iter() {
                let info = &m.info;
                if info.source.chat.is_status_broadcast() {
                    // Statuses are not conversations: they never become
                    // chat rows or messages.
                    let author = info.source.sender.to_string();
                    if let Some(id) = super::social::status_revoked_target(&m.message) {
                        extra.push(Event::StatusRevoked {
                            author: convert::bare_jid(&author),
                            id,
                        });
                    } else if let Some(item) = super::social::status_from_wa(
                        &info.id.to_string(),
                        &author,
                        &info.push_name,
                        info.source.is_from_me,
                        info.timestamp.timestamp(),
                        &m.message,
                    ) {
                        statuses.push(item);
                    }
                    continue;
                }
                if !info.source.is_from_me
                    && let Some(name) = info
                        .verified_name
                        .as_ref()
                        .and_then(|v| v.name.clone())
                        .filter(|n| !n.is_empty())
                {
                    business.push(Event::ContactName {
                        jid: convert::bare_jid(&info.source.sender.to_string()),
                        name,
                        source: NameSource::Business,
                    });
                }
                let env = Envelope {
                    id: info.id.to_string(),
                    chat: info.source.chat.to_string(),
                    sender: info.source.sender.to_string(),
                    from_me: info.source.is_from_me,
                    push_name: info.push_name.to_string(),
                    timestamp: info.timestamp.timestamp(),
                    sender_alt: info.source.sender_alt.as_ref().map(|j| j.to_string()),
                    recipient_alt: info.source.recipient_alt.as_ref().map(|j| j.to_string()),
                    ephemeral_secs: info.ephemeral_expiration,
                };
                pairs.extend(env.lid_pairs());
                match convert::parse_incoming(&env, &m.message) {
                    Some(convert::Parsed::Message(msg)) => out.push(*msg),
                    Some(convert::Parsed::Reaction { target, emoji }) => {
                        let reactor = match (&own, env.from_me) {
                            (Some(me), true) => me.clone(),
                            _ => convert::bare_jid(&env.sender),
                        };
                        extra.push(Event::Reaction {
                            chat: convert::bare_jid(&env.chat),
                            target,
                            reactor,
                            emoji,
                            ts: env.timestamp,
                        });
                    }
                    Some(convert::Parsed::Edit { target, content }) => {
                        extra.push(Event::Edited {
                            chat: convert::bare_jid(&env.chat),
                            id: target,
                            sender: convert::bare_jid(&env.sender),
                            from_me: env.from_me,
                            content: Box::new(content),
                        });
                    }
                    None => {}
                }
            }
            let mut v = business;
            if !statuses.is_empty() {
                v.push(Event::Status(statuses));
            }
            if !pairs.is_empty() {
                v.push(Event::LidMappings(pairs));
            }
            if !out.is_empty() {
                v.push(Event::Messages {
                    messages: out,
                    origin: if drained {
                        Origin::Offline
                    } else {
                        Origin::Live
                    },
                });
            }
            // Reactions and edits refer to stored messages, so they follow
            // the batch that may contain their targets.
            v.extend(extra);
            v
        }
        WaEvent::Receipt(r) => {
            let chat = convert::bare_jid(&r.source.chat.to_string());
            if convert::is_self_read(&r.r#type) {
                return vec![Event::ChatRead { chat }];
            }
            match convert::receipt_status(&r.r#type) {
                Some(status) => vec![Event::Receipt {
                    chat,
                    ids: r.message_ids.iter().map(|i| i.to_string()).collect(),
                    status,
                    participant: r
                        .source
                        .is_group
                        .then(|| convert::bare_jid(&r.source.sender.to_string())),
                    timestamp: r.timestamp.timestamp(),
                }],
                None => vec![],
            }
        }
        WaEvent::HistorySync(lazy) => match lazy.get() {
            Some(hs) => history_events(convert::from_history(hs, now_secs()), lazy.progress()),
            None => vec![],
        },
        WaEvent::PushNameUpdate(p) => vec![Event::ContactName {
            jid: convert::bare_jid(&p.jid.to_string()),
            name: p.new_push_name.clone(),
            source: NameSource::Push,
        }],
        WaEvent::ContactUpdate(c) => convert::contact_events(&c.jid.to_string(), &c.action),
        WaEvent::BusinessStatusUpdate(b) => match &b.verified_name {
            Some(name) if !name.is_empty() => vec![Event::ContactName {
                jid: convert::bare_jid(&b.jid.to_string()),
                name: name.clone(),
                source: NameSource::Business,
            }],
            _ => vec![],
        },
        WaEvent::ArchiveUpdate(a) => vec![Event::ChatFlag {
            chat: convert::bare_jid(&a.jid.to_string()),
            flag: convert::archive_flag(&a.action),
        }],
        WaEvent::PinUpdate(p) => vec![Event::ChatFlag {
            chat: convert::bare_jid(&p.jid.to_string()),
            flag: convert::pin_flag(&p.action),
        }],
        WaEvent::MuteUpdate(m) => vec![Event::ChatFlag {
            chat: convert::bare_jid(&m.jid.to_string()),
            flag: convert::mute_flag(&m.action),
        }],
        WaEvent::ClearChatUpdate(c) => vec![Event::ChatCleared {
            chat: convert::bare_jid(&c.jid.to_string()),
            keep_starred: !c.delete_starred,
        }],
        WaEvent::DeleteChatUpdate(d) => vec![Event::ChatDeleted {
            chat: convert::bare_jid(&d.jid.to_string()),
        }],
        WaEvent::LabelEditUpdate(l) => vec![convert::list_edit_event(&l.label_id, &l.action)],
        WaEvent::LabelAssociationUpdate(l) => vec![Event::ListChat {
            list: l.label_id.clone(),
            chat: convert::bare_jid(&l.chat_jid.to_string()),
            member: l.action.labeled.unwrap_or(false),
        }],
        WaEvent::ChatPresence(c) => vec![Event::ChatState {
            chat: convert::bare_jid(&c.source.chat.to_string()),
            participant: c
                .source
                .is_group
                .then(|| convert::bare_jid(&c.source.sender.to_string())),
            state: convert::chat_state(c.state, c.media),
        }],
        WaEvent::Presence(p) => vec![Event::Presence {
            jid: convert::bare_jid(&p.from.to_string()),
            online: !p.unavailable,
            last_seen: p.last_seen.map(|t| t.timestamp()),
        }],
        WaEvent::PictureUpdate(p) => vec![Event::PictureChanged {
            jid: convert::bare_jid(&p.jid.to_string()),
        }],
        WaEvent::DeleteMessageForMeUpdate(d) => vec![Event::DeletedForMe {
            chat: convert::bare_jid(&d.chat_jid.to_string()),
            id: d.message_id.clone(),
        }],
        WaEvent::StarUpdate(u) => vec![Event::Star(convert::star_change(u))],
        WaEvent::MarkChatAsReadUpdate(m) => match m.action.read {
            Some(true) => vec![Event::ChatRead {
                chat: convert::bare_jid(&m.jid.to_string()),
            }],
            Some(false) => vec![Event::ChatMarkedUnread {
                chat: convert::bare_jid(&m.jid.to_string()),
            }],
            None => vec![],
        },
        _ => vec![],
    }
}
