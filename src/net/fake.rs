//! In-process fake of the protocol layer.
//!
//! Drives `--mock` mode, screenshot mode and the integration tests. It
//! mimics the live client's event order (session probe → `Ready` → QR or
//! `Connected` → history) and its receipt behaviour (sent → delivered →
//! read after configurable delays).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::future::BoxFuture;
use futures::stream::Stream;
use tokio::sync::mpsc;

use super::social::{CommunityInfo, PhoneLookup, StatusItem};
use super::types::{
    ChatFlag, ChatMeta, ChatState, CollectionResync, ContactInfo, Content, GroupInfo, ListDef,
    MediaRef, Message, MsgKey, NameSource, Origin, OutgoingMedia, Participant, Quote,
    ReceiptStatus, ResyncReport,
};
use super::{Connection, Event, WaClient};
use crate::error::{Error, Result};
use crate::mock::MockData;

/// Knobs for the fake.
#[derive(Debug, Clone)]
pub struct FakeOptions {
    /// Start with a session (skip the QR screen).
    pub paired: bool,
    /// Number of QR codes to rotate through before `QrExhausted`.
    pub qr_codes: u32,
    /// Interval between QR codes.
    pub qr_interval: Duration,
    /// Emit delivered/read receipts for sent messages.
    pub auto_receipts: bool,
    /// Delay before each receipt step.
    pub receipt_delay: Duration,
    /// The peer answers text messages.
    pub auto_reply: bool,
    /// Make every send fail (tests the failed-tick path).
    pub fail_sends: bool,
    /// Other participants reported for every group.
    pub group_size: usize,
    /// The account's "read receipts" privacy setting.
    pub read_receipts: bool,
}

impl Default for FakeOptions {
    fn default() -> Self {
        Self {
            paired: true,
            qr_codes: 3,
            qr_interval: Duration::from_secs(20),
            auto_receipts: true,
            receipt_delay: Duration::from_millis(800),
            auto_reply: false,
            fail_sends: false,
            group_size: 3,
            read_receipts: true,
        }
    }
}

/// A recorded outgoing action.
#[derive(Debug, Clone, PartialEq)]
pub enum Sent {
    /// A media re-upload request.
    Reupload {
        /// Message id.
        id: String,
    },
    /// A forwarded message.
    Forward {
        /// Target chat.
        chat: String,
        /// New id.
        id: String,
        /// Id of the forwarded message.
        source: String,
        /// The forwarding score sent.
        score: u32,
    },
    /// Text.
    Text {
        /// Chat.
        chat: String,
        /// Id.
        id: String,
        /// Body.
        text: String,
        /// Id of the quoted message when it was a reply.
        reply_to: Option<String>,
    },
    /// Reaction (empty emoji = removed).
    Reaction {
        /// Target message.
        key: MsgKey,
        /// Emoji.
        emoji: String,
    },
    /// Edit of one of our messages.
    Edit {
        /// Chat.
        chat: String,
        /// Id.
        id: String,
        /// New text.
        text: String,
    },
    /// Delete for everyone.
    Revoke {
        /// Chat.
        chat: String,
        /// Id.
        id: String,
    },
    /// Delete for me.
    DeleteForMe {
        /// Target message.
        key: MsgKey,
    },
    /// Typing / recording indicator we sent.
    ChatState {
        /// Chat.
        chat: String,
        /// State.
        state: ChatState,
    },
    /// Presence subscription.
    Subscribed {
        /// Contact.
        jid: String,
    },
    /// Media.
    Media {
        /// Chat.
        chat: String,
        /// Id.
        id: String,
        /// Resulting content.
        content: Content,
    },
    /// Read receipt.
    Read {
        /// Chat.
        chat: String,
        /// Ids.
        ids: Vec<String>,
    },
    /// Played receipt.
    Played {
        /// Chat.
        chat: String,
        /// Ids.
        ids: Vec<String>,
    },
    /// Logout.
    Logout,
    /// Archive / unarchive.
    Archive {
        /// Chat.
        chat: String,
        /// New state.
        archived: bool,
    },
    /// Pin / unpin.
    Pin {
        /// Chat.
        chat: String,
        /// New state.
        pinned: bool,
    },
    /// Contact / group info query.
    InfoQuery(String),
    /// Block / unblock.
    Block {
        /// Contact.
        jid: String,
        /// New state.
        blocked: bool,
    },
    /// Mute / unmute.
    Mute {
        /// Chat.
        chat: String,
        /// Mute end (see `ChatMeta::mute_end`).
        end: i64,
    },
    /// Chat marked read / unread.
    MarkChat {
        /// Chat.
        chat: String,
        /// Read.
        read: bool,
        /// Newest message id sent as the message range.
        last: Option<String>,
    },
    /// Chat cleared.
    ClearChat {
        /// Chat.
        chat: String,
        /// Starred messages kept.
        keep_starred: bool,
    },
    /// Chat deleted.
    DeleteChat {
        /// Chat.
        chat: String,
    },
    /// List created or edited.
    SaveList(ListDef),
    /// List deleted.
    DeleteList(String),
    /// List membership changed.
    ListMember {
        /// List id.
        list: String,
        /// Chat.
        chat: String,
        /// Now a member.
        member: bool,
    },
    /// Full app-state resync requested.
    Resync,
    /// "Viewed" receipt for statuses (only when read receipts are on).
    StatusView {
        /// Author.
        author: String,
        /// Status ids.
        ids: Vec<String>,
    },
    /// Star / unstar.
    Star {
        /// Message.
        key: MsgKey,
        /// New state.
        starred: bool,
    },
    /// Group created.
    CreateGroup {
        /// Subject.
        subject: String,
        /// Participants.
        participants: Vec<String>,
        /// An icon was supplied.
        icon: bool,
    },
    /// Community created.
    CreateCommunity {
        /// Name.
        name: String,
    },
    /// On-WhatsApp check.
    CheckNumbers(Vec<String>),
    /// Text status posted.
    PostStatus {
        /// Text.
        text: String,
        /// Number of recipients.
        recipients: usize,
    },
    /// Declined an incoming call.
    RejectCall(String),
    /// Answered an incoming call.
    AcceptCall(String),
    /// Placed a call.
    StartCall {
        /// Peer.
        peer: String,
        /// Call id the fake gave it.
        call_id: String,
    },
    /// Hung up.
    EndCall(String),
    /// Mute changed.
    MuteCall {
        /// Call id.
        call_id: String,
        /// Muted.
        muted: bool,
    },
}

/// The fake client.
pub struct FakeClient {
    tx: mpsc::UnboundedSender<Event>,
    opts: FakeOptions,
    own_jid: String,
    log: Mutex<Vec<Sent>>,
    media: Mutex<HashMap<String, Vec<u8>>>,
    /// Media whose CDN copy "expired": served again after a re-upload.
    expired: Mutex<HashMap<String, Vec<u8>>>,
    avatars: HashMap<String, Vec<u8>>,
    pending_pair: Mutex<Option<(Vec<Event>, Vec<Message>)>>,
    resync_reports: Mutex<std::collections::VecDeque<ResyncReport>>,
    lid_pairs: Mutex<Vec<(String, String)>>,
    communities: Vec<CommunityInfo>,
    registered: Vec<String>,
    next_group: Mutex<u32>,
    /// Group metadata served by `group_info`, built from the mock history.
    groups: HashMap<String, GroupInfo>,
    /// Blocked contacts.
    blocked: Mutex<std::collections::HashSet<String>>,
}

impl FakeClient {
    /// Everything the app asked the fake to do, in order.
    pub fn log(&self) -> Vec<Sent> {
        self.log.lock().map(|l| l.clone()).unwrap_or_default()
    }

    /// Makes the media at `direct_path` expire: downloads fail with
    /// `MediaExpired` until it is re-uploaded. `gone` = nobody has it.
    pub fn expire_media(&self, direct_path: &str, gone: bool) {
        let bytes = self
            .media
            .lock()
            .ok()
            .and_then(|mut m| m.remove(direct_path));
        if let (Some(b), false, Ok(mut e)) = (bytes, gone, self.expired.lock()) {
            e.insert(direct_path.to_string(), b);
        } else if gone && let Ok(mut e) = self.expired.lock() {
            e.insert(direct_path.to_string(), Vec::new());
        }
    }

    fn record(&self, s: Sent) {
        if let Ok(mut l) = self.log.lock() {
            l.push(s);
        }
    }

    /// Simulates the phone scanning the QR code (tests / mock login).
    pub fn scan(&self) {
        let _ = self.tx.send(Event::PairSuccess {
            jid: self.own_jid.clone(),
        });
        let _ = self.tx.send(Event::Connected {
            jid: Some(self.own_jid.clone()),
        });
        let pending = self.pending_pair.lock().ok().and_then(|mut p| p.take());
        if let Some((history, messages)) = pending {
            for e in history {
                let _ = self.tx.send(e);
            }
            let _ = self.tx.send(Event::Messages {
                messages,
                origin: Origin::History,
            });
        }
    }

    /// Scripts the outcome of the next resync requests (tests).
    pub fn script_resync(&self, reports: Vec<ResyncReport>) {
        if let Ok(mut q) = self.resync_reports.lock() {
            q.extend(reports);
        }
    }

    /// LID pairs the "library" reports as already known (tests).
    pub fn set_known_lid_pairs(&self, pairs: Vec<(String, String)>) {
        if let Ok(mut p) = self.lid_pairs.lock() {
            *p = pairs;
        }
    }

    /// Simulates call signalling (a ringing offer, the caller giving up, ...).
    pub fn simulate_call(&self, signal: super::CallSignal) {
        let _ = self.tx.send(Event::Call(signal));
    }

    /// Simulates an incoming live message.
    pub fn simulate_incoming(&self, msg: Message) {
        let _ = self.tx.send(Event::Messages {
            messages: vec![msg],
            origin: Origin::Live,
        });
    }

    /// Simulates messages drained from the server's offline queue.
    pub fn simulate_offline_drain(&self, msgs: Vec<Message>) {
        let _ = self.tx.send(Event::Messages {
            messages: msgs,
            origin: Origin::Offline,
        });
    }

    /// Simulates another WhatsApp Web session taking over the stream.
    pub fn simulate_replaced(&self) {
        let _ = self.tx.send(Event::Connection(Connection::Replaced));
    }

    /// Simulates the phone unlinking this device.
    pub fn simulate_unlink(&self) {
        let _ = self.tx.send(Event::LoggedOut {
            reason: "This device was unlinked from your phone.".into(),
        });
    }

    /// Simulates a peer typing / recording / stopping.
    pub fn simulate_chat_state(&self, chat: &str, participant: Option<&str>, state: ChatState) {
        let _ = self.tx.send(Event::ChatState {
            chat: chat.into(),
            participant: participant.map(Into::into),
            state,
        });
    }

    /// Simulates a presence update.
    pub fn simulate_presence(&self, jid: &str, online: bool, last_seen: Option<i64>) {
        let _ = self.tx.send(Event::Presence {
            jid: jid.into(),
            online,
            last_seen,
        });
    }

    /// Simulates an incoming reaction.
    pub fn simulate_reaction(&self, chat: &str, target: &str, reactor: &str, emoji: &str) {
        let _ = self.tx.send(Event::Reaction {
            chat: chat.into(),
            target: target.into(),
            reactor: reactor.into(),
            emoji: emoji.into(),
            ts: chrono::Utc::now().timestamp(),
        });
    }

    /// Simulates a contact name arriving from app-state sync / push names.
    pub fn simulate_contact_name(&self, jid: &str, name: &str, source: NameSource) {
        let _ = self.tx.send(Event::ContactName {
            jid: jid.into(),
            name: name.into(),
            source,
        });
    }

    /// Simulates archive / pin / mute changing on the phone.
    pub fn simulate_chat_flag(&self, chat: &str, flag: ChatFlag) {
        let _ = self.tx.send(Event::ChatFlag {
            chat: chat.into(),
            flag,
        });
    }

    /// Simulates a list being created / renamed / deleted on the phone.
    pub fn simulate_list_edit(&self, def: ListDef, deleted: bool) {
        let _ = self.tx.send(Event::ListEdit { def, deleted });
    }

    /// Simulates a chat joining or leaving a list on the phone.
    pub fn simulate_list_chat(&self, list: &str, chat: &str, member: bool) {
        let _ = self.tx.send(Event::ListChat {
            list: list.into(),
            chat: chat.into(),
            member,
        });
    }

    /// Simulates new statuses arriving.
    pub fn simulate_status(&self, items: Vec<StatusItem>) {
        let _ = self.tx.send(Event::Status(items));
    }

    /// Simulates an author deleting a status.
    pub fn simulate_status_revoked(&self, author: &str, id: &str) {
        let _ = self.tx.send(Event::StatusRevoked {
            author: author.into(),
            id: id.into(),
        });
    }

    /// Simulates a star change made on the phone.
    pub fn simulate_star(&self, key: MsgKey, starred: bool) {
        let _ = self.tx.send(Event::Star(super::social::StarChange {
            key,
            starred,
            ts: chrono::Utc::now().timestamp(),
        }));
    }

    /// Simulates a participant's receipt in a group.
    pub fn simulate_group_receipt(
        &self,
        chat: &str,
        ids: &[&str],
        status: ReceiptStatus,
        participant: &str,
    ) {
        let _ = self.tx.send(Event::Receipt {
            chat: chat.into(),
            ids: ids.iter().map(|s| s.to_string()).collect(),
            status,
            participant: Some(participant.into()),
            timestamp: 0,
        });
    }

    fn schedule_receipts(&self, chat: &str, id: &str) {
        let tx = self.tx.clone();
        let chat = chat.to_string();
        let ids = vec![id.to_string()];
        let delay = self.opts.receipt_delay;
        let auto = self.opts.auto_receipts;
        let _ = tx.send(Event::Receipt {
            chat: chat.clone(),
            ids: ids.clone(),
            status: ReceiptStatus::Sent,
            participant: None,
            timestamp: 0,
        });
        if !auto || crate::net::is_group_jid(&chat) {
            // Group receipts come per participant; tests drive them.
            return;
        }
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            let _ = tx.send(Event::Receipt {
                chat: chat.clone(),
                ids: ids.clone(),
                status: ReceiptStatus::Delivered,
                participant: None,
                timestamp: 0,
            });
            tokio::time::sleep(delay).await;
            let _ = tx.send(Event::Receipt {
                chat,
                ids,
                status: ReceiptStatus::Read,
                participant: None,
                timestamp: 0,
            });
        });
    }
}

impl WaClient for FakeClient {
    fn forward<'a>(
        &'a self,
        chat: &'a str,
        id: &'a str,
        source: &'a Message,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            if self.opts.fail_sends {
                return Err(Error::Net("fake send failure".into()));
            }
            if !source.can_forward() {
                return Err(Error::Protocol("this message can't be forwarded".into()));
            }
            self.record(Sent::Forward {
                chat: chat.into(),
                id: id.into(),
                source: source.id.clone(),
                score: source.forward_score(),
            });
            self.schedule_receipts(chat, id);
            Ok(())
        })
    }

    fn send_text<'a>(
        &'a self,
        chat: &'a str,
        id: &'a str,
        text: &'a str,
        reply: Option<&'a Quote>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            if self.opts.fail_sends {
                return Err(Error::Net("fake send failure".into()));
            }
            self.record(Sent::Text {
                chat: chat.into(),
                id: id.into(),
                text: text.into(),
                reply_to: reply.map(|q| q.id.clone()),
            });
            self.schedule_receipts(chat, id);
            if self.opts.auto_reply {
                let tx = self.tx.clone();
                let chat = chat.to_string();
                let delay = self.opts.receipt_delay * 3;
                tokio::spawn(async move {
                    tokio::time::sleep(delay / 2).await;
                    let _ = tx.send(Event::ChatState {
                        chat: chat.clone(),
                        participant: None,
                        state: ChatState::Composing,
                    });
                    tokio::time::sleep(delay / 2).await;
                    let _ = tx.send(Event::ChatState {
                        chat: chat.clone(),
                        participant: None,
                        state: ChatState::Paused,
                    });
                    let _ = tx.send(Event::Messages {
                        messages: vec![Message {
                            id: super::new_message_id(),
                            chat: chat.clone(),
                            sender: chat,
                            sender_name: None,
                            from_me: false,
                            timestamp: chrono::Utc::now().timestamp(),
                            content: Content::Text("👍".into()),
                            status: ReceiptStatus::Read,
                            expires_at: None,
                            reply: None,
                            edited: false,
                            forwarded: None,
                            mentions: Vec::new(),
                            reactions: Vec::new(),
                        }],
                        origin: Origin::Live,
                    });
                });
            }
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
            if self.opts.fail_sends {
                return Err(Error::Net("fake upload failure".into()));
            }
            let path = format!("/fake/{id}");
            let kind = match media.kind {
                super::OutgoingKind::Image { .. } => super::MediaKind::Image,
                super::OutgoingKind::Document => super::MediaKind::Document,
                super::OutgoingKind::Video { .. } => super::MediaKind::Video,
                super::OutgoingKind::Voice { .. } => super::MediaKind::Audio,
            };
            let mref = MediaRef {
                direct_path: path.clone(),
                media_key: vec![1; 32],
                file_sha256: id.as_bytes().to_vec(),
                file_enc_sha256: vec![2; 32],
                file_length: media.bytes.len() as u64,
                kind,
                mimetype: media.mimetype.clone(),
            };
            if let Ok(mut m) = self.media.lock() {
                m.insert(path, media.bytes.clone());
            }
            let content = media.local_content(Some(mref));
            self.record(Sent::Media {
                chat: chat.into(),
                id: id.into(),
                content: content.clone(),
            });
            self.schedule_receipts(chat, id);
            Ok(content)
        })
    }

    fn download<'a>(&'a self, media: &'a MediaRef) -> BoxFuture<'a, Result<Vec<u8>>> {
        Box::pin(async move {
            if self
                .expired
                .lock()
                .is_ok_and(|e| e.contains_key(&media.direct_path))
            {
                return Err(Error::MediaExpired);
            }
            self.media
                .lock()
                .ok()
                .and_then(|m| m.get(&media.direct_path).cloned())
                .ok_or_else(|| Error::Net("media not found (404)".into()))
        })
    }

    fn reupload<'a>(
        &'a self,
        key: &'a MsgKey,
        media: &'a MediaRef,
    ) -> BoxFuture<'a, Result<MediaRef>> {
        Box::pin(async move {
            self.record(Sent::Reupload { id: key.id.clone() });
            let bytes = self
                .expired
                .lock()
                .ok()
                .and_then(|mut e| e.remove(&media.direct_path))
                .filter(|b| !b.is_empty())
                .ok_or_else(|| Error::MediaGone("not found".into()))?;
            let direct_path = format!("{}-re", media.direct_path);
            if let Ok(mut m) = self.media.lock() {
                m.insert(direct_path.clone(), bytes);
            }
            Ok(MediaRef {
                direct_path,
                ..media.clone()
            })
        })
    }

    fn mark_read<'a>(
        &'a self,
        chat: &'a str,
        _sender: Option<&'a str>,
        ids: &'a [String],
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.record(Sent::Read {
                chat: chat.into(),
                ids: ids.to_vec(),
            });
            Ok(())
        })
    }

    fn mark_played<'a>(
        &'a self,
        chat: &'a str,
        _sender: Option<&'a str>,
        ids: &'a [String],
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.record(Sent::Played {
                chat: chat.into(),
                ids: ids.to_vec(),
            });
            Ok(())
        })
    }

    fn profile_picture<'a>(&'a self, jid: &'a str) -> BoxFuture<'a, Result<Option<Vec<u8>>>> {
        Box::pin(async move { Ok(self.avatars.get(jid).cloned()) })
    }

    fn send_reaction<'a>(&'a self, key: &'a MsgKey, emoji: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            if self.opts.fail_sends {
                return Err(Error::Net("fake send failure".into()));
            }
            self.record(Sent::Reaction {
                key: key.clone(),
                emoji: emoji.into(),
            });
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
            if self.opts.fail_sends {
                return Err(Error::Net("fake send failure".into()));
            }
            self.record(Sent::Edit {
                chat: chat.into(),
                id: id.into(),
                text: text.into(),
            });
            Ok(())
        })
    }

    fn revoke<'a>(&'a self, chat: &'a str, id: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            if self.opts.fail_sends {
                return Err(Error::Net("fake send failure".into()));
            }
            self.record(Sent::Revoke {
                chat: chat.into(),
                id: id.into(),
            });
            Ok(())
        })
    }

    fn delete_for_me<'a>(&'a self, key: &'a MsgKey, _timestamp: i64) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.record(Sent::DeleteForMe { key: key.clone() });
            Ok(())
        })
    }

    fn send_chat_state<'a>(&'a self, chat: &'a str, state: ChatState) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.record(Sent::ChatState {
                chat: chat.into(),
                state,
            });
            Ok(())
        })
    }

    fn subscribe_presence<'a>(&'a self, jid: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.record(Sent::Subscribed { jid: jid.into() });
            Ok(())
        })
    }

    fn group_size<'a>(&'a self, _chat: &'a str) -> BoxFuture<'a, Result<usize>> {
        Box::pin(async move { Ok(self.opts.group_size) })
    }

    fn set_archived<'a>(&'a self, chat: &'a str, archived: bool) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            if self.opts.fail_sends {
                return Err(Error::Net("fake send failure".into()));
            }
            self.record(Sent::Archive {
                chat: chat.into(),
                archived,
            });
            Ok(())
        })
    }

    fn set_pinned<'a>(&'a self, chat: &'a str, pinned: bool) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.record(Sent::Pin {
                chat: chat.into(),
                pinned,
            });
            Ok(())
        })
    }

    fn save_list<'a>(&'a self, list: &'a ListDef) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            if self.opts.fail_sends {
                return Err(Error::Net("fake send failure".into()));
            }
            self.record(Sent::SaveList(list.clone()));
            Ok(())
        })
    }

    fn delete_list<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.record(Sent::DeleteList(id.into()));
            Ok(())
        })
    }

    fn set_list_member<'a>(
        &'a self,
        list: &'a str,
        chat: &'a str,
        member: bool,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.record(Sent::ListMember {
                list: list.into(),
                chat: chat.into(),
                member,
            });
            Ok(())
        })
    }

    fn resync_app_state(&self) -> BoxFuture<'_, Result<ResyncReport>> {
        Box::pin(async move {
            self.record(Sent::Resync);
            let scripted = self
                .resync_reports
                .lock()
                .ok()
                .and_then(|mut q| q.pop_front());
            Ok(scripted.unwrap_or_else(|| ResyncReport {
                collections: vec![CollectionResync {
                    name: "critical_unblock_low",
                    before: 3,
                    after: 4,
                }],
                had_sync_key: true,
                contacts: 1,
            }))
        })
    }

    fn known_lid_pairs(&self) -> BoxFuture<'_, Result<Vec<(String, String)>>> {
        Box::pin(async move { Ok(self.lid_pairs.lock().map(|p| p.clone()).unwrap_or_default()) })
    }

    fn send_status_view<'a>(
        &'a self,
        author: &'a str,
        ids: &'a [String],
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async move {
            if self.opts.fail_sends {
                return Err(Error::Net("fake send failure".into()));
            }
            if !self.opts.read_receipts {
                return Ok(false);
            }
            self.record(Sent::StatusView {
                author: author.into(),
                ids: ids.to_vec(),
            });
            Ok(true)
        })
    }

    fn set_starred<'a>(&'a self, key: &'a MsgKey, starred: bool) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            if self.opts.fail_sends {
                return Err(Error::Net("fake send failure".into()));
            }
            self.record(Sent::Star {
                key: key.clone(),
                starred,
            });
            Ok(())
        })
    }

    fn create_group<'a>(
        &'a self,
        subject: &'a str,
        participants: &'a [String],
        icon: Option<Vec<u8>>,
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            if self.opts.fail_sends {
                return Err(Error::Net("fake send failure".into()));
            }
            self.record(Sent::CreateGroup {
                subject: subject.into(),
                participants: participants.to_vec(),
                icon: icon.is_some(),
            });
            let n = match self.next_group.lock() {
                Ok(mut g) => {
                    *g += 1;
                    *g
                }
                Err(_) => 1,
            };
            Ok(format!("1203639999{n:08}@g.us"))
        })
    }

    fn create_community<'a>(
        &'a self,
        name: &'a str,
        _description: &'a str,
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            if self.opts.fail_sends {
                return Err(Error::Net("fake send failure".into()));
            }
            self.record(Sent::CreateCommunity { name: name.into() });
            Ok("1203638888000000001@g.us".into())
        })
    }

    fn communities(&self) -> BoxFuture<'_, Result<Vec<CommunityInfo>>> {
        Box::pin(async move { Ok(self.communities.clone()) })
    }

    fn check_numbers<'a>(
        &'a self,
        digits: &'a [String],
    ) -> BoxFuture<'a, Result<Vec<PhoneLookup>>> {
        Box::pin(async move {
            self.record(Sent::CheckNumbers(digits.to_vec()));
            Ok(digits
                .iter()
                .map(|d| {
                    if self.registered.iter().any(|r| r == d) {
                        PhoneLookup::OnWhatsApp {
                            jid: super::social::phone_jid(d),
                            business: false,
                        }
                    } else {
                        PhoneLookup::NotOnWhatsApp
                    }
                })
                .collect())
        })
    }

    fn post_status_text<'a>(
        &'a self,
        text: &'a str,
        _background: u32,
        _font: i32,
        recipients: &'a [String],
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            if self.opts.fail_sends {
                return Err(Error::Net("fake send failure".into()));
            }
            self.record(Sent::PostStatus {
                text: text.into(),
                recipients: recipients.len(),
            });
            Ok(crate::net::new_message_id())
        })
    }

    fn simulate_scan(&self) {
        self.scan();
    }

    fn reject_call<'a>(&'a self, call_id: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.record(Sent::RejectCall(call_id.into()));
            Ok(())
        })
    }

    fn accept_call<'a>(
        &'a self,
        call_id: &'a str,
        _audio: super::CallAudio,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.record(Sent::AcceptCall(call_id.into()));
            let _ = self.tx.send(Event::Call(super::CallSignal::Connected {
                call_id: call_id.into(),
            }));
            Ok(())
        })
    }

    fn start_call<'a>(
        &'a self,
        peer: &'a str,
        _audio: super::CallAudio,
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            if self.opts.fail_sends {
                return Err(Error::Protocol("could not call".into()));
            }
            let call_id = format!("00{:030X}", self.log().len());
            self.record(Sent::StartCall {
                peer: peer.into(),
                call_id: call_id.clone(),
            });
            if self.opts.auto_reply {
                // The mock peer picks up after a moment.
                let tx = self.tx.clone();
                let id = call_id.clone();
                let delay = self.opts.receipt_delay;
                tokio::spawn(async move {
                    tokio::time::sleep(delay * 3).await;
                    let _ = tx.send(Event::Call(super::CallSignal::PeerAccepted {
                        call_id: id.clone(),
                    }));
                    let _ = tx.send(Event::Call(super::CallSignal::Connected { call_id: id }));
                });
            }
            Ok(call_id)
        })
    }

    fn end_call<'a>(&'a self, call_id: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.record(Sent::EndCall(call_id.into()));
            let _ = self.tx.send(Event::Call(super::CallSignal::MediaEnded {
                call_id: call_id.into(),
                error: None,
            }));
            Ok(())
        })
    }

    fn set_call_muted(&self, call_id: &str, muted: bool) {
        self.record(Sent::MuteCall {
            call_id: call_id.into(),
            muted,
        });
    }

    fn contact_info<'a>(&'a self, jid: &'a str) -> BoxFuture<'a, Result<ContactInfo>> {
        Box::pin(async move {
            self.record(Sent::InfoQuery(jid.into()));
            Ok(ContactInfo {
                about: Some(crate::mock::about_for(jid).to_string()),
                business_name: None,
            })
        })
    }

    fn group_info<'a>(&'a self, chat: &'a str) -> BoxFuture<'a, Result<GroupInfo>> {
        Box::pin(async move {
            self.record(Sent::InfoQuery(chat.into()));
            self.groups
                .get(chat)
                .cloned()
                .ok_or_else(|| Error::Net("not a participant".into()))
        })
    }

    fn common_groups<'a>(
        &'a self,
        ids: &'a [String],
    ) -> BoxFuture<'a, Result<Vec<(String, String)>>> {
        Box::pin(async move {
            let mut out: Vec<(String, String)> = self
                .groups
                .iter()
                .filter(|(_, g)| {
                    g.participants.iter().any(|p| {
                        ids.contains(&p.jid) || p.alias.as_ref().is_some_and(|a| ids.contains(a))
                    })
                })
                .map(|(j, g)| (j.clone(), g.subject.clone()))
                .collect();
            out.sort_by(|a, b| a.1.cmp(&b.1));
            Ok(out)
        })
    }

    fn is_blocked<'a>(&'a self, jid: &'a str) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async move {
            Ok(self
                .blocked
                .lock()
                .map(|b| b.contains(jid))
                .unwrap_or(false))
        })
    }

    fn set_blocked<'a>(&'a self, jid: &'a str, blocked: bool) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            if self.opts.fail_sends {
                return Err(Error::Net("fake send failure".into()));
            }
            if let Ok(mut b) = self.blocked.lock() {
                if blocked {
                    b.insert(jid.to_string());
                } else {
                    b.remove(jid);
                }
            }
            self.record(Sent::Block {
                jid: jid.into(),
                blocked,
            });
            Ok(())
        })
    }

    fn set_mute<'a>(&'a self, chat: &'a str, end: i64) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            if self.opts.fail_sends {
                return Err(Error::Net("fake send failure".into()));
            }
            self.record(Sent::Mute {
                chat: chat.into(),
                end,
            });
            Ok(())
        })
    }

    fn mark_chat_read<'a>(
        &'a self,
        chat: &'a str,
        read: bool,
        last: Option<&'a (MsgKey, i64)>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            if self.opts.fail_sends {
                return Err(Error::Net("fake send failure".into()));
            }
            self.record(Sent::MarkChat {
                chat: chat.into(),
                read,
                last: last.map(|(k, _)| k.id.clone()),
            });
            Ok(())
        })
    }

    fn clear_chat<'a>(
        &'a self,
        chat: &'a str,
        keep_starred: bool,
        _last: Option<&'a (MsgKey, i64)>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            if self.opts.fail_sends {
                return Err(Error::Net("fake send failure".into()));
            }
            self.record(Sent::ClearChat {
                chat: chat.into(),
                keep_starred,
            });
            Ok(())
        })
    }

    fn delete_chat<'a>(
        &'a self,
        chat: &'a str,
        _last: Option<&'a (MsgKey, i64)>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            if self.opts.fail_sends {
                return Err(Error::Net("fake send failure".into()));
            }
            self.record(Sent::DeleteChat { chat: chat.into() });
            Ok(())
        })
    }

    fn logout(&self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            self.record(Sent::Logout);
            let _ = self.tx.send(Event::LoggedOut {
                reason: "You logged out.".into(),
            });
            Ok(())
        })
    }
}

/// Starts the fake. Returns the concrete handle (for test hooks) and the
/// event stream the app subscribes to.
pub fn start(
    opts: FakeOptions,
    data: MockData,
) -> (Arc<FakeClient>, impl Stream<Item = Event> + Send + 'static) {
    let (tx, rx) = mpsc::unbounded_channel();
    let mut history = vec![Event::History {
        chats: data.chats.clone(),
        names: data.names.clone(),
        progress: Some(100),
    }];
    // App-state sync: address-book names, business names and lists.
    for (jid, name) in &data.saved {
        history.push(Event::ContactName {
            jid: jid.clone(),
            name: name.clone(),
            source: NameSource::Saved,
        });
    }
    for (jid, name) in &data.business {
        history.push(Event::ContactName {
            jid: jid.clone(),
            name: name.clone(),
            source: NameSource::Business,
        });
    }
    for def in &data.lists {
        history.push(Event::ListEdit {
            def: def.clone(),
            deleted: false,
        });
    }
    for (list, chat) in &data.list_members {
        history.push(Event::ListChat {
            list: list.clone(),
            chat: chat.clone(),
            member: true,
        });
    }
    let client = Arc::new(FakeClient {
        tx: tx.clone(),
        opts: opts.clone(),
        own_jid: data.own_jid.clone(),
        log: Mutex::new(Vec::new()),
        media: Mutex::new(data.media),
        expired: Mutex::new(HashMap::new()),
        avatars: data.avatars,
        pending_pair: Mutex::new(None),
        resync_reports: Mutex::new(std::collections::VecDeque::new()),
        lid_pairs: Mutex::new(Vec::new()),
        communities: data.communities.clone(),
        registered: data.registered.clone(),
        next_group: Mutex::new(0),
        groups: mock_groups(&data.own_jid, &data.chats, &data.messages),
        blocked: Mutex::new(std::collections::HashSet::new()),
    });
    let _ = tx.send(if opts.paired {
        Event::SessionFound
    } else {
        Event::NoSession
    });
    let _ = tx.send(Event::Ready(client.clone()));
    let _ = tx.send(Event::Connection(Connection::Connected));
    if opts.paired {
        let _ = tx.send(Event::Connected {
            jid: Some(data.own_jid.clone()),
        });
        for e in history {
            let _ = tx.send(e);
        }
        let _ = tx.send(Event::Messages {
            messages: data.messages,
            origin: Origin::History,
        });
        for (i, key) in data.starred.iter().enumerate() {
            let _ = tx.send(Event::Star(super::social::StarChange {
                key: key.clone(),
                starred: true,
                ts: 1_700_000_000 + i as i64,
            }));
        }
        if !data.statuses.is_empty() {
            let _ = tx.send(Event::Status(data.statuses.clone()));
        }
        for (chat, participant, state) in data.typing {
            let _ = tx.send(Event::ChatState {
                chat,
                participant,
                state,
            });
        }
        for (jid, online, last_seen) in data.presence {
            let _ = tx.send(Event::Presence {
                jid,
                online,
                last_seen,
            });
        }
    } else {
        if let Ok(mut p) = client.pending_pair.lock() {
            *p = Some((history, data.messages));
        }
        let txq = tx.clone();
        let (n, interval) = (opts.qr_codes, opts.qr_interval);
        tokio::spawn(async move {
            for i in 0..n {
                let code = format!(
                    "2@FERROCHATMOCK{i:02}QRPAYLOAD,bW9ja19ub2lzZV9rZXk=,bW9ja19pZGVudGl0eQ==,bW9ja19hZHY="
                );
                if txq
                    .send(Event::Qr {
                        code,
                        valid_for_secs: interval.as_secs(),
                    })
                    .is_err()
                {
                    return;
                }
                tokio::time::sleep(interval).await;
            }
            let _ = txq.send(Event::QrExhausted);
        });
    }
    let stream =
        futures::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|e| (e, rx)) });
    (client, stream)
}

/// Group metadata for the mock groups: the senders seen in each group plus
/// ourselves (owner and admin), with a description.
fn mock_groups(own: &str, chats: &[ChatMeta], messages: &[Message]) -> HashMap<String, GroupInfo> {
    let mut out = HashMap::new();
    for c in chats.iter().filter(|c| c.id.ends_with("@g.us")) {
        let mut participants = vec![Participant {
            jid: own.to_string(),
            alias: None,
            admin: true,
            owner: true,
        }];
        for m in messages.iter().filter(|m| m.chat == c.id && !m.from_me) {
            if !participants.iter().any(|p| p.jid == m.sender) {
                participants.push(Participant {
                    jid: m.sender.clone(),
                    alias: None,
                    admin: participants.len() == 1,
                    owner: false,
                });
            }
        }
        out.insert(
            c.id.clone(),
            GroupInfo {
                subject: c.name.clone().unwrap_or_default(),
                description: Some(crate::mock::group_description(&c.id).to_string()),
                created: Some(1_690_000_000),
                creator: Some(own.to_string()),
                participants,
            },
        );
    }
    out
}
