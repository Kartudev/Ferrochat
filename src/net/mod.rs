//! Protocol client. Owns sockets, never touches UI types.
//!
//! The app talks to WhatsApp only through the [`WaClient`] trait and the
//! [`Event`] stream. Two implementations exist:
//!
//! * [`whatsapp::RealClient`]: `whatsapp-rust` (pure Rust multi-device
//!   protocol), with its Signal store in a SQLCipher database.
//! * [`fake::FakeClient`]: an in-process scripted fake used by `--mock`,
//!   screenshot mode and the integration tests.

pub mod call;
pub mod convert;
pub mod fake;
pub mod social;
pub mod types;
pub mod whatsapp;

use std::fmt;
use std::sync::Arc;

use futures::future::BoxFuture;

use crate::error::Result;
pub use call::{CallAudio, CallLog, CallOutcome, CallSignal, IncomingCallInfo};
pub use social::{
    CommunityInfo, PhoneLookup, StarChange, StarredMessage, StatusBody, StatusGroup, StatusItem,
    SubGroup,
};
pub use types::*;

/// Shared handle to the active protocol client.
pub type ClientHandle = Arc<dyn WaClient>;

/// Operations the app can ask of the protocol layer.
pub trait WaClient: Send + Sync {
    /// Sends a text message with a caller-chosen id, optionally as a reply.
    fn send_text<'a>(
        &'a self,
        chat: &'a str,
        id: &'a str,
        text: &'a str,
        reply: Option<&'a Quote>,
    ) -> BoxFuture<'a, Result<()>>;
    /// Uploads media and sends it. Returns the content as sent (with the CDN
    /// reference filled in).
    fn send_media<'a>(
        &'a self,
        chat: &'a str,
        id: &'a str,
        media: OutgoingMedia,
    ) -> BoxFuture<'a, Result<Content>>;
    /// Forwards a message (`source`, as cached) to `chat` with a
    /// caller-chosen id. Media is relayed from the same CDN blob.
    fn forward<'a>(
        &'a self,
        chat: &'a str,
        id: &'a str,
        source: &'a Message,
    ) -> BoxFuture<'a, Result<()>>;
    /// Downloads and decrypts a media blob. `Error::MediaExpired` when the
    /// CDN no longer has it (see [`reupload`](Self::reupload)).
    fn download<'a>(&'a self, media: &'a MediaRef) -> BoxFuture<'a, Result<Vec<u8>>>;
    /// Asks for expired media of message `key` to be uploaded again (the
    /// "media retry" receipt WhatsApp sends: the phone that has the file
    /// re-uploads it). `key` carries the JIDs the message came from (LIDs
    /// included). Returns `media` with the new path; `Error::MediaGone` when
    /// nobody has it any more.
    fn reupload<'a>(
        &'a self,
        key: &'a MsgKey,
        media: &'a MediaRef,
    ) -> BoxFuture<'a, Result<MediaRef>>;
    /// Sends read receipts. `sender` is required for group messages.
    fn mark_read<'a>(
        &'a self,
        chat: &'a str,
        sender: Option<&'a str>,
        ids: &'a [String],
    ) -> BoxFuture<'a, Result<()>>;
    /// Sends a "played" receipt for a voice note.
    fn mark_played<'a>(
        &'a self,
        chat: &'a str,
        sender: Option<&'a str>,
        ids: &'a [String],
    ) -> BoxFuture<'a, Result<()>>;
    /// Fetches a profile picture preview (JPEG bytes). `Ok(None)` when the
    /// contact or group has none or hides it.
    fn profile_picture<'a>(&'a self, jid: &'a str) -> BoxFuture<'a, Result<Option<Vec<u8>>>>;
    /// Reacts to a message; an empty `emoji` removes our reaction.
    fn send_reaction<'a>(&'a self, key: &'a MsgKey, emoji: &'a str) -> BoxFuture<'a, Result<()>>;
    /// Edits the text of one of our messages.
    fn edit_text<'a>(
        &'a self,
        chat: &'a str,
        id: &'a str,
        text: &'a str,
    ) -> BoxFuture<'a, Result<()>>;
    /// "Delete for everyone" for one of our messages.
    fn revoke<'a>(&'a self, chat: &'a str, id: &'a str) -> BoxFuture<'a, Result<()>>;
    /// "Delete for me": syncs the deletion to our other devices.
    fn delete_for_me<'a>(&'a self, key: &'a MsgKey, timestamp: i64) -> BoxFuture<'a, Result<()>>;
    /// Tells `chat` that we are typing / recording / stopped.
    fn send_chat_state<'a>(&'a self, chat: &'a str, state: ChatState) -> BoxFuture<'a, Result<()>>;
    /// Subscribes to a contact's online / last-seen updates.
    fn subscribe_presence<'a>(&'a self, jid: &'a str) -> BoxFuture<'a, Result<()>>;
    /// Number of other participants of a group (everyone but us).
    fn group_size<'a>(&'a self, chat: &'a str) -> BoxFuture<'a, Result<usize>>;
    /// Archives or unarchives a chat on every device (app-state `archive`).
    fn set_archived<'a>(&'a self, chat: &'a str, archived: bool) -> BoxFuture<'a, Result<()>>;
    /// Pins or unpins a chat on every device (app-state `pin_v1`).
    fn set_pinned<'a>(&'a self, chat: &'a str, pinned: bool) -> BoxFuture<'a, Result<()>>;
    /// Creates or edits a custom list (app-state `label_edit`).
    fn save_list<'a>(&'a self, list: &'a ListDef) -> BoxFuture<'a, Result<()>>;
    /// Deletes a custom list.
    fn delete_list<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<()>>;
    /// Adds a chat to / removes it from a list (app-state `label_jid`).
    fn set_list_member<'a>(
        &'a self,
        list: &'a str,
        chat: &'a str,
        member: bool,
    ) -> BoxFuture<'a, Result<()>>;
    /// Forces a full app-state snapshot (contacts, archive, pins, lists) so
    /// names saved on the phone arrive even when the collections were synced
    /// before this client used them. Reports what was applied.
    fn resync_app_state(&self) -> BoxFuture<'_, Result<ResyncReport>>;
    /// LID ↔ phone-number pairs the protocol library already knows
    /// (`(lid, pn)` JIDs), so names keyed by either id meet.
    fn known_lid_pairs(&self) -> BoxFuture<'_, Result<Vec<(String, String)>>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    /// Tells `author` that we viewed these statuses. Returns `Ok(false)` when
    /// nothing was sent because our "read receipts" privacy setting is off
    /// (or could not be read): statuses are then only marked seen locally.
    fn send_status_view<'a>(
        &'a self,
        author: &'a str,
        ids: &'a [String],
    ) -> BoxFuture<'a, Result<bool>>;
    /// Stars or unstars a message on every device (app-state `star`).
    fn set_starred<'a>(&'a self, key: &'a MsgKey, starred: bool) -> BoxFuture<'a, Result<()>>;
    /// Creates a group and returns its JID. `icon` is a JPEG.
    fn create_group<'a>(
        &'a self,
        subject: &'a str,
        participants: &'a [String],
        icon: Option<Vec<u8>>,
    ) -> BoxFuture<'a, Result<String>>;
    /// Creates a community and returns its JID.
    fn create_community<'a>(
        &'a self,
        name: &'a str,
        description: &'a str,
    ) -> BoxFuture<'a, Result<String>>;
    /// The communities we belong to, with their subgroups.
    fn communities(&self) -> BoxFuture<'_, Result<Vec<CommunityInfo>>>;
    /// Asks the server whether these phone numbers (digits only) have a
    /// WhatsApp account. The answer has one entry per input, in order.
    fn check_numbers<'a>(&'a self, digits: &'a [String])
    -> BoxFuture<'a, Result<Vec<PhoneLookup>>>;
    /// Posts a text status to `recipients` (our saved contacts) and returns
    /// its id.
    fn post_status_text<'a>(
        &'a self,
        text: &'a str,
        background: u32,
        font: i32,
        recipients: &'a [String],
    ) -> BoxFuture<'a, Result<String>>;
    /// About text and business name of a contact (the usync query the
    /// official clients use for the contact info drawer).
    fn contact_info<'a>(&'a self, jid: &'a str) -> BoxFuture<'a, Result<ContactInfo>>;
    /// Group subject, description and members (group metadata query).
    fn group_info<'a>(&'a self, chat: &'a str) -> BoxFuture<'a, Result<GroupInfo>>;
    /// `(group JID, subject)` of the groups we share with the person whose
    /// ids (phone number and / or LID) are `ids`.
    fn common_groups<'a>(
        &'a self,
        ids: &'a [String],
    ) -> BoxFuture<'a, Result<Vec<(String, String)>>>;
    /// Whether a contact is on our block list.
    fn is_blocked<'a>(&'a self, jid: &'a str) -> BoxFuture<'a, Result<bool>>;
    /// Blocks / unblocks a contact.
    fn set_blocked<'a>(&'a self, jid: &'a str, blocked: bool) -> BoxFuture<'a, Result<()>>;
    /// Mutes a chat until `end` (Unix seconds, or [`MUTE_FOREVER`]) or
    /// unmutes it ([`MUTE_OFF`]); synced to the phone.
    fn set_mute<'a>(&'a self, chat: &'a str, end: i64) -> BoxFuture<'a, Result<()>>;
    /// Marks a chat read / unread on every device (app-state
    /// `markChatAsRead`). `last` is the newest message and its time, which
    /// lets the phone resolve conflicts.
    fn mark_chat_read<'a>(
        &'a self,
        chat: &'a str,
        read: bool,
        last: Option<&'a (MsgKey, i64)>,
    ) -> BoxFuture<'a, Result<()>>;
    /// Clears a chat's messages but keeps the chat (app-state `clearChat`).
    /// Starred messages survive when `keep_starred`.
    fn clear_chat<'a>(
        &'a self,
        chat: &'a str,
        keep_starred: bool,
        last: Option<&'a (MsgKey, i64)>,
    ) -> BoxFuture<'a, Result<()>>;
    /// Deletes a chat and its messages (app-state `deleteChat`).
    fn delete_chat<'a>(
        &'a self,
        chat: &'a str,
        last: Option<&'a (MsgKey, i64)>,
    ) -> BoxFuture<'a, Result<()>>;
    /// Declines a call ringing here (the library's `<reject>`).
    fn reject_call<'a>(&'a self, call_id: &'a str) -> BoxFuture<'a, Result<()>>;
    /// Answers a call ringing here with these audio ends. Progress arrives
    /// as [`Event::Call`] (`Connected`, `MediaEnded`).
    fn accept_call<'a>(&'a self, call_id: &'a str, audio: CallAudio) -> BoxFuture<'a, Result<()>>;
    /// Places a 1:1 voice call; returns the call id.
    fn start_call<'a>(&'a self, peer: &'a str, audio: CallAudio) -> BoxFuture<'a, Result<String>>;
    /// Hangs up a call with media, or cancels one still ringing at the peer.
    fn end_call<'a>(&'a self, call_id: &'a str) -> BoxFuture<'a, Result<()>>;
    /// Mutes or unmutes our microphone in a call.
    fn set_call_muted(&self, call_id: &str, muted: bool);
    /// Unlinks this device, disconnects and deletes the session store.
    fn logout(&self) -> BoxFuture<'_, Result<()>>;
    /// `--mock` only: pretend the phone scanned the QR code. The live client
    /// ignores it.
    fn simulate_scan(&self) {}
}

/// Connection state shown in the banner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Connection {
    /// Not connected, not trying.
    Disconnected,
    /// First connection attempt.
    Connecting,
    /// Online.
    Connected,
    /// Lost the connection, retrying with backoff.
    Reconnecting {
        /// Attempt number, from 1.
        attempt: u32,
    },
    /// Another WhatsApp Web/Desktop session took over this one. The session
    /// is still valid; the user can reconnect.
    Replaced,
    /// The server refused the connection for a non-transient reason; the app
    /// retries with backoff and offers a manual retry.
    Offline {
        /// User-facing reason.
        reason: String,
    },
}

/// Events pushed by the protocol client.
#[derive(Clone)]
pub enum Event {
    /// The client is constructed; keep the handle to issue commands.
    Ready(ClientHandle),
    /// A stored session was found and is being restored.
    SessionFound,
    /// Not paired; QR codes will follow.
    NoSession,
    /// New QR payload to display.
    Qr {
        /// Payload.
        code: String,
        /// Validity in seconds.
        valid_for_secs: u64,
    },
    /// No more QR codes will come; the app restarts the client to get new ones.
    QrExhausted,
    /// The phone accepted the link.
    PairSuccess {
        /// Own JID.
        jid: String,
    },
    /// Pairing failed.
    PairError(String),
    /// Connection state changed.
    Connection(Connection),
    /// Connected with a valid session.
    Connected {
        /// Own JID.
        jid: Option<String>,
    },
    /// Session revoked / device unlinked.
    LoggedOut {
        /// Reason, user-facing.
        reason: String,
    },
    /// New or historical messages.
    Messages {
        /// Messages in arrival order.
        messages: Vec<Message>,
        /// Live, offline drain or history.
        origin: Origin,
    },
    /// History sync chunk: chat metadata and names.
    History {
        /// Chats.
        chats: Vec<ChatMeta>,
        /// Contact names (`jid`, `name`).
        names: Vec<(String, String)>,
        /// Sync progress 0..=100 if known.
        progress: Option<u32>,
    },
    /// Delivery / read receipt for outgoing messages. In groups every
    /// participant sends its own (`participant` set); the app aggregates them.
    Receipt {
        /// Chat.
        chat: ChatId,
        /// Affected message ids.
        ids: Vec<String>,
        /// New status.
        status: ReceiptStatus,
        /// Who the receipt is from (groups only).
        participant: Option<String>,
        /// When it was sent (Unix seconds); 0 = unknown (local states).
        timestamp: i64,
    },
    /// A reaction was added, changed or removed (`emoji` empty).
    Reaction {
        /// Chat.
        chat: ChatId,
        /// Reacted-to message id.
        target: String,
        /// Who reacted (bare JID).
        reactor: String,
        /// The emoji; empty removes the reaction.
        emoji: String,
        /// Unix seconds.
        ts: i64,
    },
    /// The sender edited a message.
    Edited {
        /// Chat.
        chat: ChatId,
        /// Edited message id.
        id: String,
        /// Who sent the edit (must be the original sender).
        sender: String,
        /// Sent by us (from another device)?
        from_me: bool,
        /// New content.
        content: Box<Content>,
    },
    /// A message was deleted for us on another device.
    DeletedForMe {
        /// Chat.
        chat: ChatId,
        /// Message id.
        id: String,
    },
    /// Typing / recording indicator.
    ChatState {
        /// Chat.
        chat: ChatId,
        /// The participant in a group.
        participant: Option<String>,
        /// State.
        state: ChatState,
    },
    /// Online / last seen of a contact.
    Presence {
        /// Contact.
        jid: String,
        /// Currently online.
        online: bool,
        /// Last seen, unix seconds, if the contact shares it.
        last_seen: Option<i64>,
    },
    /// A profile picture was set, changed or removed.
    PictureChanged {
        /// Contact or group.
        jid: String,
    },
    /// A contact's display name changed / became known. An empty `name`
    /// from the address book (`Saved`) means the contact lost its saved name.
    ContactName {
        /// JID.
        jid: String,
        /// Name.
        name: String,
        /// Where the name comes from (decides display priority).
        source: NameSource,
    },
    /// Archive / pin / mute changed on another device.
    ChatFlag {
        /// Chat.
        chat: ChatId,
        /// New value.
        flag: ChatFlag,
    },
    /// A custom list was created, renamed or deleted on another device.
    ListEdit {
        /// The list.
        def: ListDef,
        /// Removed.
        deleted: bool,
    },
    /// A chat joined or left a list on another device.
    ListChat {
        /// List id.
        list: String,
        /// Chat.
        chat: ChatId,
        /// Now a member.
        member: bool,
    },
    /// Chat marked unread on another device (app-state `markChatAsRead`
    /// with `read = false`).
    ChatMarkedUnread {
        /// Chat.
        chat: ChatId,
    },
    /// Chat marked read on another device.
    ChatRead {
        /// Chat.
        chat: ChatId,
    },
    /// A chat's messages were cleared on another device (the chat stays).
    ChatCleared {
        /// Chat.
        chat: ChatId,
        /// Starred messages were kept.
        keep_starred: bool,
    },
    /// A chat was deleted on another device.
    ChatDeleted {
        /// Chat.
        chat: ChatId,
    },
    /// New statuses (live or from the offline queue).
    Status(Vec<StatusItem>),
    /// A status was revoked by its author.
    StatusRevoked {
        /// Author (bare JID).
        author: String,
        /// Status id.
        id: String,
    },
    /// A message was starred / unstarred (here or on another device).
    Star(StarChange),
    /// Call signalling or call media progress.
    Call(CallSignal),
    /// LID ↔ phone-number pairs learned from envelopes or history.
    LidMappings(Vec<(String, String)>),
    /// The session store exists but cannot be decrypted with the keyring key
    /// (key lost or replaced). Nothing was deleted; the user decides.
    SessionUnreadable(String),
    /// Unrecoverable failure (keyring missing, database unreadable).
    Fatal(String),
}

impl fmt::Debug for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Redacting Debug: no QR payloads, bodies or names.
        match self {
            Event::Ready(_) => write!(f, "Ready"),
            Event::SessionFound => write!(f, "SessionFound"),
            Event::NoSession => write!(f, "NoSession"),
            Event::Qr { valid_for_secs, .. } => write!(f, "Qr(valid={valid_for_secs}s)"),
            Event::QrExhausted => write!(f, "QrExhausted"),
            Event::PairSuccess { .. } => write!(f, "PairSuccess"),
            Event::PairError(_) => write!(f, "PairError"),
            Event::Connection(c) => write!(f, "Connection({c:?})"),
            Event::Connected { .. } => write!(f, "Connected"),
            Event::LoggedOut { .. } => write!(f, "LoggedOut"),
            Event::Messages { messages, origin } => {
                write!(f, "Messages(n={}, {origin:?})", messages.len())
            }
            Event::History {
                chats,
                names,
                progress,
            } => write!(
                f,
                "History(chats={}, names={}, progress={progress:?})",
                chats.len(),
                names.len()
            ),
            Event::Receipt { ids, status, .. } => {
                write!(f, "Receipt(n={}, {status:?})", ids.len())
            }
            Event::Reaction { .. } => write!(f, "Reaction"),
            Event::Edited { .. } => write!(f, "Edited"),
            Event::DeletedForMe { .. } => write!(f, "DeletedForMe"),
            Event::ChatState { state, .. } => write!(f, "ChatState({state:?})"),
            Event::Presence { online, .. } => write!(f, "Presence(online={online})"),
            Event::PictureChanged { .. } => write!(f, "PictureChanged"),
            Event::ContactName { source, .. } => write!(f, "ContactName({source:?})"),
            Event::ChatFlag { flag, .. } => write!(f, "ChatFlag({flag:?})"),
            Event::ListEdit { deleted, .. } => write!(f, "ListEdit(deleted={deleted})"),
            Event::ListChat { member, .. } => write!(f, "ListChat(member={member})"),
            Event::ChatRead { .. } => write!(f, "ChatRead"),
            Event::ChatMarkedUnread { .. } => write!(f, "ChatMarkedUnread"),
            Event::ChatCleared { .. } => write!(f, "ChatCleared"),
            Event::ChatDeleted { .. } => write!(f, "ChatDeleted"),
            Event::Status(v) => write!(f, "Status(n={})", v.len()),
            Event::StatusRevoked { .. } => write!(f, "StatusRevoked"),
            Event::Star(c) => write!(f, "Star({})", c.starred),
            Event::Call(c) => write!(f, "Call({c:?})"),
            Event::LidMappings(p) => write!(f, "LidMappings(n={})", p.len()),
            Event::SessionUnreadable(_) => write!(f, "SessionUnreadable"),
            Event::Fatal(_) => write!(f, "Fatal"),
        }
    }
}

/// Generates a message id in the same shape WhatsApp Web uses
/// (`3EB0` + 18 upper-case hex chars).
pub fn new_message_id() -> String {
    let mut raw = [0u8; 9];
    if getrandom::fill(&mut raw).is_err() {
        // Extremely unlikely; fall back to the clock so ids stay unique.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        raw.copy_from_slice(&nanos.to_be_bytes()[7..16]);
    }
    format!("3EB0{}", hex::encode_upper(raw))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_ids_look_like_web_ids() {
        let a = new_message_id();
        let b = new_message_id();
        assert_ne!(a, b);
        assert_eq!(a.len(), 22);
        assert!(a.starts_with("3EB0"));
    }

    #[test]
    fn event_debug_is_redacted() {
        let e = Event::Qr {
            code: "2@secretpayload".into(),
            valid_for_secs: 20,
        };
        assert!(!format!("{e:?}").contains("secret"));
    }
}
