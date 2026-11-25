//! Domain types shared by `net`, `store` and `app`.
//!
//! These are deliberately independent of the protocol library: the
//! conversion from `whatsapp-rust` / protobuf types lives in
//! [`crate::net::convert`], so the rest of the app (and the fake client) only
//! ever sees these.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A chat JID as a string (`<user>@s.whatsapp.net`, `<id>@g.us`, `<id>@lid`).
pub type ChatId = String;

/// What kind of media a [`MediaRef`] points to (selects the HKDF info string
/// used to decrypt it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MediaKind {
    /// JPEG/PNG/WebP image.
    Image,
    /// Any file sent as a document.
    Document,
    /// Audio and voice notes (PTT).
    Audio,
    /// Video (and GIF-as-video) messages.
    Video,
    /// WebP stickers.
    Sticker,
}

/// Everything needed to download and decrypt a media blob from the CDN.
/// Not a credential: it is per-message content, like the message body.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MediaRef {
    /// CDN path.
    pub direct_path: String,
    /// Per-file AES key material.
    pub media_key: Vec<u8>,
    /// SHA-256 of the plaintext.
    pub file_sha256: Vec<u8>,
    /// SHA-256 of the ciphertext.
    pub file_enc_sha256: Vec<u8>,
    /// Plaintext size in bytes.
    pub file_length: u64,
    /// Media kind.
    pub kind: MediaKind,
    /// MIME type as announced by the sender.
    pub mimetype: String,
}

impl MediaRef {
    /// Stable cache key (hex of the plaintext hash, or the path).
    pub fn cache_key(&self) -> String {
        if self.file_sha256.is_empty() {
            self.direct_path.clone()
        } else {
            hex::encode(&self.file_sha256)
        }
    }
}

impl fmt::Debug for MediaRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MediaRef")
            .field("kind", &self.kind)
            .field("mimetype", &self.mimetype)
            .field("file_length", &self.file_length)
            .field("media_key", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

/// A link preview as attached by the sender (WhatsApp clients never fetch
/// previews on the receiving side, so neither do we).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkPreview {
    /// The URL the preview is for.
    pub url: String,
    /// Page title.
    pub title: String,
    /// Short description.
    pub description: String,
    /// Inline JPEG thumbnail (stored separately in the cache).
    #[serde(skip)]
    pub thumbnail: Option<Vec<u8>>,
}

impl fmt::Debug for LinkPreview {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LinkPreview")
    }
}

/// The body of a message.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub enum Content {
    /// Plain or extended text.
    Text(String),
    /// Text that carries a link preview (title, description, thumbnail)
    /// built by the sender's phone.
    Link {
        /// The message text.
        text: String,
        /// The preview card.
        preview: LinkPreview,
    },
    /// A photo.
    Image {
        /// Optional caption.
        caption: Option<String>,
        /// Pixel width, if announced.
        width: u32,
        /// Pixel height, if announced.
        height: u32,
        /// Inline JPEG thumbnail (stored separately in the cache).
        #[serde(skip)]
        thumbnail: Option<Vec<u8>>,
        /// Where to fetch the full image.
        media: Option<MediaRef>,
    },
    /// A file.
    Document {
        /// File name shown in the bubble.
        file_name: String,
        /// MIME type.
        mimetype: String,
        /// Size in bytes.
        size: u64,
        /// Page count for PDFs.
        pages: Option<u32>,
        /// Optional caption.
        caption: Option<String>,
        /// Where to fetch the file.
        media: Option<MediaRef>,
    },
    /// Audio; `ptt` marks a recorded voice note.
    Voice {
        /// Duration in seconds.
        seconds: u32,
        /// Push-to-talk (voice note) vs. audio file.
        ptt: bool,
        /// 64 amplitude samples (0..=100) for the waveform.
        waveform: Vec<u8>,
        /// Where to fetch the audio.
        media: Option<MediaRef>,
    },
    /// A video message: thumbnail and duration are shown in the bubble, the
    /// file itself is downloaded on click and opened in the system player.
    Video {
        /// Optional caption.
        caption: Option<String>,
        /// Duration in seconds.
        seconds: u32,
        /// Pixel width, if announced.
        width: u32,
        /// Pixel height, if announced.
        height: u32,
        /// Inline JPEG thumbnail (stored separately in the cache).
        #[serde(skip)]
        thumbnail: Option<Vec<u8>>,
        /// Where to fetch the video.
        media: Option<MediaRef>,
        /// A GIF (WhatsApp sends GIFs as silent MP4s with `gifPlayback`):
        /// labelled "GIF" and looped. Rows cached before this field read as
        /// plain videos.
        #[serde(default)]
        gif: bool,
    },
    /// A sticker (WebP). Rendered statically: animated ones show frame 1.
    Sticker {
        /// Pixel width, if announced.
        width: u32,
        /// Pixel height, if announced.
        height: u32,
        /// Animated sticker (only the first frame is shown).
        animated: bool,
        /// Where to fetch the WebP.
        media: Option<MediaRef>,
    },
    /// Something we do not render yet (poll, location, ...).
    Unsupported(String),
    /// A view-once photo / video / voice note. Never downloaded or stored;
    /// the user opens it on the phone.
    ViewOnce,
    /// "This message was deleted".
    Deleted,
    /// A call (missed, declined, answered ...), shown as a call bubble.
    Call(super::call::CallLog),
}

impl Content {
    /// One-line preview for the chat list and notifications.
    pub fn preview(&self) -> String {
        match self {
            Content::Text(t) | Content::Link { text: t, .. } => {
                t.lines().next().unwrap_or_default().to_string()
            }
            Content::Image { caption, .. } => match caption {
                Some(c) if !c.is_empty() => format!("📷 {c}"),
                _ => "📷 Photo".into(),
            },
            Content::Document { file_name, .. } => format!("📄 {file_name}"),
            Content::Video {
                caption,
                seconds,
                gif,
                ..
            } => match caption {
                Some(c) if !c.is_empty() => format!("{} {c}", if *gif { "GIF" } else { "🎥" }),
                _ if *gif => "GIF".into(),
                _ => format!("🎥 {}", format_duration(*seconds)),
            },
            Content::Sticker { .. } => "Sticker".into(),
            Content::Voice { seconds, ptt, .. } => {
                let label = if *ptt { "🎤" } else { "🎵" };
                format!("{label} {}", format_duration(*seconds))
            }
            Content::Unsupported(kind) => format!("Unsupported message: {kind}"),
            Content::ViewOnce => "👁 View once message".into(),
            Content::Deleted => "🚫 This message was deleted".into(),
            // Direction-neutral wording; the chat list adds "You:" for ours.
            Content::Call(c) => c.preview(true),
        }
    }

    /// The text of a plain or link message.
    pub fn text(&self) -> Option<&str> {
        match self {
            Content::Text(t) | Content::Link { text: t, .. } => Some(t),
            _ => None,
        }
    }

    /// Mutable access to the media reference (a re-upload moves it).
    pub fn media_mut(&mut self) -> Option<&mut MediaRef> {
        match self {
            Content::Image { media, .. }
            | Content::Document { media, .. }
            | Content::Video { media, .. }
            | Content::Sticker { media, .. }
            | Content::Voice { media, .. } => media.as_mut(),
            _ => None,
        }
    }

    /// The downloadable media of this content, if any.
    pub fn media(&self) -> Option<&MediaRef> {
        match self {
            Content::Image { media, .. }
            | Content::Document { media, .. }
            | Content::Video { media, .. }
            | Content::Sticker { media, .. }
            | Content::Voice { media, .. } => media.as_ref(),
            _ => None,
        }
    }

    /// Short type tag stored in the cache.
    pub fn kind_tag(&self) -> &'static str {
        match self {
            Content::Text(_) => "text",
            Content::Link { .. } => "link",
            Content::Image { .. } => "image",
            Content::Document { .. } => "document",
            Content::Voice { .. } => "voice",
            Content::Video { .. } => "video",
            Content::Sticker { .. } => "sticker",
            Content::Unsupported(_) => "unsupported",
            Content::ViewOnce => "view_once",
            Content::Deleted => "deleted",
            Content::Call(_) => "call",
        }
    }
}

impl fmt::Debug for Content {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never print message bodies or file names.
        match self {
            Content::Text(t) => write!(f, "Text(len={})", t.chars().count()),
            Content::Link { text, .. } => write!(f, "Link(len={})", text.chars().count()),
            Content::Image { width, height, .. } => write!(f, "Image({width}x{height})"),
            Content::Document { size, .. } => write!(f, "Document(size={size})"),
            Content::Voice { seconds, ptt, .. } => write!(f, "Voice({seconds}s, ptt={ptt})"),
            Content::Video { seconds, .. } => write!(f, "Video({seconds}s)"),
            Content::Sticker { animated, .. } => write!(f, "Sticker(animated={animated})"),
            Content::Unsupported(k) => write!(f, "Unsupported({k})"),
            Content::ViewOnce => write!(f, "ViewOnce"),
            Content::Deleted => write!(f, "Deleted"),
            Content::Call(c) => write!(f, "{c:?}"),
        }
    }
}

/// `m:ss` like the official client.
pub fn format_duration(seconds: u32) -> String {
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// Delivery state of an outgoing message (the tick marks).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
pub enum ReceiptStatus {
    /// Queued locally, clock icon.
    Pending,
    /// Sending failed, red alert icon.
    Failed,
    /// Server accepted, one grey tick.
    Sent,
    /// Delivered to a device of the recipient, two grey ticks.
    Delivered,
    /// Read, two blue ticks.
    Read,
    /// Voice note played, two blue ticks (blue mic in the bubble).
    Played,
}

impl ReceiptStatus {
    fn rank(self) -> u8 {
        match self {
            ReceiptStatus::Pending => 0,
            ReceiptStatus::Failed => 0,
            ReceiptStatus::Sent => 1,
            ReceiptStatus::Delivered => 2,
            ReceiptStatus::Read => 3,
            ReceiptStatus::Played => 4,
        }
    }

    /// Receipt state machine. Receipts can arrive out of order (a `read` can
    /// overtake a `delivered`), so the state only ever moves forward:
    /// `Pending → Sent → Delivered → Read → Played`. `Failed` is only reachable
    /// from `Pending`, and any server confirmation overrides `Failed` (a retry
    /// succeeded).
    pub fn advance(self, incoming: ReceiptStatus) -> ReceiptStatus {
        match (self, incoming) {
            (ReceiptStatus::Pending, ReceiptStatus::Failed) => ReceiptStatus::Failed,
            (s, ReceiptStatus::Failed) => s,
            (ReceiptStatus::Failed, ReceiptStatus::Pending) => ReceiptStatus::Failed,
            (cur, new) if new.rank() > cur.rank() => new,
            (cur, _) => cur,
        }
    }

    /// Integer stored in SQLite.
    pub fn to_i64(self) -> i64 {
        match self {
            ReceiptStatus::Pending => 0,
            ReceiptStatus::Failed => -1,
            ReceiptStatus::Sent => 1,
            ReceiptStatus::Delivered => 2,
            ReceiptStatus::Read => 3,
            ReceiptStatus::Played => 4,
        }
    }

    /// Inverse of [`Self::to_i64`]; unknown values map to `Sent`.
    pub fn from_i64(v: i64) -> Self {
        match v {
            0 => ReceiptStatus::Pending,
            -1 => ReceiptStatus::Failed,
            2 => ReceiptStatus::Delivered,
            3 => ReceiptStatus::Read,
            4 => ReceiptStatus::Played,
            _ => ReceiptStatus::Sent,
        }
    }
}

/// A message in a chat.
#[derive(Clone, PartialEq)]
pub struct Message {
    /// Message id (unique per chat).
    pub id: String,
    /// Chat JID.
    pub chat: ChatId,
    /// Sender JID (own JID for outgoing messages).
    pub sender: String,
    /// Push name of the sender, if the server sent one.
    pub sender_name: Option<String>,
    /// Outgoing?
    pub from_me: bool,
    /// Unix seconds.
    pub timestamp: i64,
    /// Body.
    pub content: Content,
    /// Delivery state (only meaningful when `from_me`).
    pub status: ReceiptStatus,
    /// Disappearing messages: unix seconds after which the cache drops it.
    pub expires_at: Option<i64>,
    /// The message this one replies to.
    pub reply: Option<Quote>,
    /// The sender edited the message after sending it.
    pub edited: bool,
    /// Reactions, one per reactor, oldest first.
    pub reactions: Vec<Reaction>,
    /// JIDs mentioned in the text (`contextInfo.mentionedJid`): PN or LID,
    /// the text then carries "@<user part>".
    pub mentions: Vec<String>,
    /// Forwarded: `Some(contextInfo.forwardingScore)`, `None` when the
    /// message was not forwarded.
    pub forwarded: Option<u32>,
}

impl fmt::Debug for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // JIDs are phone numbers and push names are personal data: masked.
        f.debug_struct("Message")
            .field("id", &self.id)
            .field(
                "chat",
                &format_args!("{}", crate::error::MaskedJid(&self.chat)),
            )
            .field(
                "sender",
                &format_args!("{}", crate::error::MaskedJid(&self.sender)),
            )
            .field("from_me", &self.from_me)
            .field("timestamp", &self.timestamp)
            .field("content", &self.content)
            .field("status", &self.status)
            .field("reply", &self.reply)
            .field("edited", &self.edited)
            .field("reactions", &self.reactions.len())
            .field("mentions", &self.mentions.len())
            .field("forwarded", &self.forwarded)
            .finish_non_exhaustive()
    }
}

/// The quoted message of a reply (WhatsApp's `contextInfo`).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Quote {
    /// Id of the quoted message.
    pub id: String,
    /// Author of the quoted message (bare JID).
    pub sender: String,
    /// One-line preview ("📷 Photo", or the first line of the text).
    pub preview: String,
}

impl fmt::Debug for Quote {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Previews are message bodies: never printed.
        write!(f, "Quote(len={})", self.preview.chars().count())
    }
}

/// Longest quote preview kept (characters).
pub const QUOTE_PREVIEW_MAX: usize = 160;

/// Most mentioned JIDs kept per message (a group has at most ~1024
/// members; anything longer is garbage).
pub const MAX_MENTIONS: usize = 1024;

impl Quote {
    /// Quote of `msg` as shown in the reply bubble.
    pub fn of(msg: &Message) -> Self {
        Self {
            id: msg.id.clone(),
            sender: msg.sender.clone(),
            preview: msg
                .content
                .preview()
                .chars()
                .take(QUOTE_PREVIEW_MAX)
                .collect(),
        }
    }
}

/// One person's reaction to a message.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reaction {
    /// Who reacted (bare JID; our own JID for our reactions).
    pub reactor: String,
    /// The emoji.
    pub emoji: String,
    /// Unix seconds.
    pub ts: i64,
}

impl fmt::Debug for Reaction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Reaction({})", self.emoji)
    }
}

/// Typing indicator state of a peer (chat state).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChatState {
    /// Typing a text message.
    Composing,
    /// Recording a voice note.
    Recording,
    /// Stopped.
    Paused,
}

/// Identifies one message for actions that need its protocol key.
#[derive(Clone, PartialEq, Eq)]
pub struct MsgKey {
    /// Chat JID.
    pub chat: ChatId,
    /// Message id.
    pub id: String,
    /// Sent by us.
    pub from_me: bool,
    /// Author (bare JID); the participant for group messages.
    pub sender: String,
}

impl fmt::Debug for MsgKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MsgKey(from_me={})", self.from_me)
    }
}

/// Result of applying an edit to `old`: text stays text; for media only the
/// caption changes (the media reference is kept). `None` when the edit does
/// not fit the original (so it is ignored).
pub fn merge_edit(old: &Content, new: &Content) -> Option<Content> {
    let new_text = |c: &Content| -> Option<Option<String>> {
        match c {
            Content::Text(t) | Content::Link { text: t, .. } => {
                Some(Some(t.clone()).filter(|t| !t.is_empty()))
            }
            Content::Image { caption, .. }
            | Content::Video { caption, .. }
            | Content::Document { caption, .. } => Some(caption.clone()),
            _ => None,
        }
    };
    let text = new_text(new)?;
    match old {
        Content::Text(_) => Some(Content::Text(text?)),
        // An edit replaces the text; the sender's preview stays attached.
        Content::Link { preview, .. } => Some(Content::Link {
            text: text?,
            preview: preview.clone(),
        }),
        Content::Image { .. } | Content::Video { .. } | Content::Document { .. } => {
            let mut c = old.clone();
            match &mut c {
                Content::Image { caption, .. }
                | Content::Video { caption, .. }
                | Content::Document { caption, .. } => *caption = text,
                _ => return None,
            }
            Some(c)
        }
        _ => None,
    }
}

/// Seconds a message can be edited after sending (WhatsApp: 15 minutes).
pub const EDIT_WINDOW_SECS: i64 = 15 * 60;
/// Seconds "delete for everyone" stays available (WhatsApp: 2 days 12 hours).
pub const REVOKE_WINDOW_SECS: i64 = 60 * 60 * 60;

impl Message {
    /// Our text message is still inside WhatsApp's edit window.
    pub fn can_edit(&self, now: i64) -> bool {
        self.from_me
            && matches!(self.content, Content::Text(_) | Content::Link { .. })
            && self.status != ReceiptStatus::Failed
            && self.status != ReceiptStatus::Pending
            && now - self.timestamp <= EDIT_WINDOW_SECS
    }

    /// Our message can still be deleted for everyone.
    pub fn can_revoke(&self, now: i64) -> bool {
        self.from_me
            && !matches!(self.content, Content::Deleted)
            && self.status != ReceiptStatus::Failed
            && self.status != ReceiptStatus::Pending
            && now - self.timestamp <= REVOKE_WINDOW_SECS
    }

    /// Shown as "Forwarded many times" (WhatsApp's frequently forwarded
    /// mark, double arrow). Such messages go to one chat at a time.
    pub fn frequently_forwarded(&self) -> bool {
        self.forwarded
            .is_some_and(|s| s >= FREQUENTLY_FORWARDED_SCORE)
    }

    /// The message can be forwarded: something the recipient can open
    /// (text or media still on the CDN), and ours only once it was sent.
    pub fn can_forward(&self) -> bool {
        let content = match &self.content {
            Content::Text(_) | Content::Link { .. } => true,
            Content::Image { media, .. }
            | Content::Document { media, .. }
            | Content::Voice { media, .. }
            | Content::Video { media, .. }
            | Content::Sticker { media, .. } => {
                media.as_ref().is_some_and(|m| !m.media_key.is_empty())
            }
            Content::Unsupported(_) | Content::ViewOnce | Content::Deleted | Content::Call(_) => {
                false
            }
        };
        content
            && !(self.from_me
                && matches!(self.status, ReceiptStatus::Pending | ReceiptStatus::Failed))
    }

    /// The forwarding score a forward of this message carries
    /// (`prepare_for_forward` in the library: the source score, plus one if
    /// the source was already a forward, jumping to 127 from 5 on).
    pub fn forward_score(&self) -> u32 {
        match self.forwarded {
            None => 0,
            Some(s) if s.saturating_add(1) >= FREQUENTLY_FORWARDED_SCORE => 127,
            Some(s) => s + 1,
        }
    }

    /// Protocol key of this message.
    pub fn key(&self) -> MsgKey {
        MsgKey {
            chat: self.chat.clone(),
            id: self.id.clone(),
            from_me: self.from_me,
            sender: self.sender.clone(),
        }
    }
}

/// Chat metadata (from history sync or derived from messages).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ChatMeta {
    /// JID.
    pub id: ChatId,
    /// Display name (contact or group subject).
    pub name: Option<String>,
    /// Unread messages count.
    pub unread: u32,
    /// Unix seconds of the last activity.
    pub last_ts: i64,
    /// Pinned to the top.
    pub pinned: bool,
    /// Archived.
    pub archived: bool,
    /// When the mute ends: [`MUTE_OFF`], [`MUTE_FOREVER`] or Unix seconds.
    pub mute_end: i64,
}

/// Forwarding score from which a message is "forwarded many times".
pub const FREQUENTLY_FORWARDED_SCORE: u32 = 5;

/// [`ChatMeta::mute_end`] of a chat that is not muted.
pub const MUTE_OFF: i64 = 0;
/// [`ChatMeta::mute_end`] of a chat muted until unmuted ("Always").
pub const MUTE_FOREVER: i64 = -1;

/// Whether a mute ending at `end` (see [`ChatMeta::mute_end`]) still holds
/// at `now` (Unix seconds).
pub fn mute_active(end: i64, now: i64) -> bool {
    end < 0 || end > now
}

impl ChatMeta {
    /// Muted at `now` (Unix seconds): timed mutes end on their own.
    pub fn is_muted(&self, now: i64) -> bool {
        mute_active(self.mute_end, now)
    }
}

/// Where a batch of messages comes from. Decides unread counting and
/// notifications.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Delivered live.
    Live,
    /// Drained from the server's offline queue after (re)connecting: new to
    /// us, but delivered in bulk.
    Offline,
    /// History sync / local echo: never counts as unread.
    History,
}

/// Status updates (`status@broadcast`), broadcast lists and channels: not
/// chats of the chat list, never counted as unread.
pub fn is_pseudo_chat(jid: &str) -> bool {
    jid.ends_with("@broadcast") || jid.ends_with("@newsletter")
}

/// True for a `(…@lid, …@s.whatsapp.net)` pair.
pub fn is_lid_pair(lid: &str, pn: &str) -> bool {
    lid.ends_with("@lid") && pn.ends_with("@s.whatsapp.net")
}

/// Maps `@lid` JIDs to phone-number JIDs so one contact is one chat.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LidMap {
    lid_to_pn: std::collections::HashMap<String, String>,
}

impl LidMap {
    /// Learns a pair; ignores anything that is not `(…@lid, …@s.whatsapp.net)`.
    /// Returns true if the map changed.
    pub fn learn(&mut self, lid: &str, pn: &str) -> bool {
        if !lid.ends_with("@lid") || !pn.ends_with("@s.whatsapp.net") {
            return false;
        }
        self.lid_to_pn
            .insert(lid.to_string(), pn.to_string())
            .as_deref()
            != Some(pn)
    }

    /// PN JID for a LID, or the input unchanged.
    pub fn normalize(&self, jid: &str) -> String {
        self.lid_to_pn
            .get(jid)
            .cloned()
            .unwrap_or_else(|| jid.to_string())
    }

    /// Rewrites chat and sender of a message.
    pub fn apply(&self, m: &mut Message) {
        m.chat = self.normalize(&m.chat);
        m.sender = self.normalize(&m.sender);
    }

    /// All pairs (for persisting).
    pub fn pairs(&self) -> Vec<(String, String)> {
        self.lid_to_pn
            .iter()
            .map(|(a, b)| (a.clone(), b.clone()))
            .collect()
    }

    /// Number of known pairs.
    pub fn len(&self) -> usize {
        self.lid_to_pn.len()
    }

    /// True when empty.
    pub fn is_empty(&self) -> bool {
        self.lid_to_pn.is_empty()
    }
}

/// What the contact info drawer shows about a person (usync query).
#[derive(Clone, Default, PartialEq, Eq)]
pub struct ContactInfo {
    /// "About" text, if the contact's privacy settings let us see it.
    pub about: Option<String>,
    /// Verified business name, for business accounts.
    pub business_name: Option<String>,
}

impl fmt::Debug for ContactInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ContactInfo")
            .field("about", &self.about.as_ref().map(|_| "<redacted>"))
            .field("business", &self.business_name.is_some())
            .finish()
    }
}

/// One member of a group.
#[derive(Clone, PartialEq, Eq)]
pub struct Participant {
    /// JID (phone number when the group gives it, else the LID).
    pub jid: String,
    /// The other id of the same person, if the group gave both.
    pub alias: Option<String>,
    /// Group admin.
    pub admin: bool,
    /// Group creator / owner ("super admin").
    pub owner: bool,
}

impl fmt::Debug for Participant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Participant({}, admin={})",
            crate::error::MaskedJid(&self.jid),
            self.admin
        )
    }
}

/// Group metadata for the group info drawer (group metadata IQ).
#[derive(Clone, Default, PartialEq, Eq)]
pub struct GroupInfo {
    /// Subject.
    pub subject: String,
    /// Description.
    pub description: Option<String>,
    /// Creation time, unix seconds.
    pub created: Option<i64>,
    /// Creator, if known.
    pub creator: Option<String>,
    /// Members: admins first, then by JID.
    pub participants: Vec<Participant>,
}

impl fmt::Debug for GroupInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GroupInfo")
            .field("participants", &self.participants.len())
            .field("has_description", &self.description.is_some())
            .finish_non_exhaustive()
    }
}

/// True for group JIDs.
pub fn is_group_jid(jid: &str) -> bool {
    jid.ends_with("@g.us")
}

/// Fallback display name for a JID: `+<number>` for phone JIDs.
pub fn fallback_name(jid: &str) -> String {
    let user = jid.split('@').next().unwrap_or(jid);
    let user = user.split([':', '.']).next().unwrap_or(user);
    if jid.ends_with("@s.whatsapp.net")
        && !user.is_empty()
        && user.chars().all(|c| c.is_ascii_digit())
    {
        format_phone(user)
    } else if is_group_jid(jid) {
        "Group".into()
    } else {
        user.to_string()
    }
}

/// Number of digits of the country calling code at the start of `digits`
/// (ITU-T E.164: the codes are prefix-free, so the first digits decide).
fn country_code_len(digits: &str) -> usize {
    let b = digits.as_bytes();
    match b.first() {
        Some(b'1' | b'7') => 1,
        Some(_) => {
            const TWO: &[&str] = &[
                "20", "27", "30", "31", "32", "33", "34", "36", "39", "40", "41", "43", "44", "45",
                "46", "47", "48", "49", "51", "52", "53", "54", "55", "56", "57", "58", "60", "61",
                "62", "63", "64", "65", "66", "81", "82", "84", "86", "90", "91", "92", "93", "94",
                "95", "98",
            ];
            if digits.get(..2).is_some_and(|c| TWO.contains(&c)) {
                2
            } else {
                3
            }
        }
        None => 0,
    }
}

/// `34600111222` -> `+34 600 111 222`: country code, then the national number
/// in groups of three (a trailing single digit joins the previous group).
/// Not a libphonenumber replacement, only a readable fallback title.
pub fn format_phone(digits: &str) -> String {
    let cc = country_code_len(digits).min(digits.len());
    let (code, national) = digits.split_at(cc);
    let mut groups: Vec<&str> = national
        .as_bytes()
        .chunks(3)
        .map(|c| std::str::from_utf8(c).unwrap_or_default())
        .collect();
    if groups.len() > 1 && groups.last().is_some_and(|g| g.len() == 1) {
        let last = groups.pop().unwrap_or_default();
        if let Some(prev) = groups.pop() {
            let start = national.len() - last.len() - prev.len();
            groups.push(&national[start..]);
        }
    }
    if groups.is_empty() {
        format!("+{code}")
    } else {
        format!("+{code} {}", groups.join(" "))
    }
}

/// Push names from history sync as `(jid, source, name)` rows.
pub fn push_names(names: &[(String, String)]) -> Vec<(String, NameSource, String)> {
    names
        .iter()
        .map(|(j, n)| (j.clone(), NameSource::Push, n.clone()))
        .collect()
}

/// Where a contact name comes from, ordered by display priority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum NameSource {
    /// The name the contact chose for itself (shown as "~ Name").
    Push = 1,
    /// A business account's verified name.
    Business = 2,
    /// The name saved in the user's phone address book (app-state sync).
    Saved = 3,
}

impl NameSource {
    /// Database code.
    pub fn code(self) -> i64 {
        self as i64
    }

    /// From a database code.
    pub fn from_code(c: i64) -> Option<Self> {
        match c {
            1 => Some(Self::Push),
            2 => Some(Self::Business),
            3 => Some(Self::Saved),
            _ => None,
        }
    }
}

/// What one app-state collection did during a forced resync.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionResync {
    /// Collection name (`critical_unblock_low`, `regular`, ...).
    pub name: &'static str,
    /// Persisted version before the resync.
    pub before: u64,
    /// Persisted version after it (0: the snapshot was not applied).
    pub after: u64,
}

impl CollectionResync {
    /// The snapshot was applied (the version only leaves 0 through a
    /// snapshot or the genesis patch).
    pub fn applied(&self) -> bool {
        self.after > 0
    }
}

/// Outcome of a forced app-state resync. Counts only: never names or ids.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResyncReport {
    /// Per collection.
    pub collections: Vec<CollectionResync>,
    /// An app-state sync key was stored before the resync (keys come from
    /// the phone in `AppStateSyncKeyShare`; without one nothing decrypts).
    pub had_sync_key: bool,
    /// `contact` mutations (address-book entries) received while it ran.
    pub contacts: usize,
}

impl ResyncReport {
    /// Every collection applied its snapshot.
    pub fn complete(&self) -> bool {
        !self.collections.is_empty() && self.collections.iter().all(CollectionResync::applied)
    }

    /// The resync achieved its purpose: the snapshots were applied and the
    /// address book arrived. An account with no saved contact at all only
    /// counts as done after `attempt` reaches `max_empty`.
    pub fn succeeded(&self, attempt: u32, max_empty: u32) -> bool {
        self.complete() && (self.contacts > 0 || attempt >= max_empty)
    }
}

/// A chat property changed on another device (app-state sync).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatFlag {
    /// Archived / unarchived.
    Archived(bool),
    /// Pinned / unpinned.
    Pinned(bool),
    /// Muted until this time (see [`ChatMeta::mute_end`]); [`MUTE_OFF`]
    /// unmutes.
    Muted(i64),
}

/// What a chat list "label" is (WhatsApp's `LabelEditAction.ListType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ListKind {
    /// A list the user created ("Lists" in the official app).
    Custom,
    /// The Favourites list.
    Favourites,
    /// Anything else (business labels, predefined server lists): kept so
    /// associations are not lost, not shown as a chip.
    #[default]
    Other,
}

impl ListKind {
    /// Database code.
    pub fn code(self) -> i64 {
        match self {
            Self::Custom => 1,
            Self::Favourites => 2,
            Self::Other => 0,
        }
    }

    /// From a database code.
    pub fn from_code(c: i64) -> Self {
        match c {
            1 => Self::Custom,
            2 => Self::Favourites,
            _ => Self::Other,
        }
    }
}

/// A chat list definition synced through app state (`label_edit`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ListDef {
    /// Label id (opaque, chosen by the creating client).
    pub id: String,
    /// Name shown on the chip.
    pub name: String,
    /// WhatsApp colour index.
    pub color: i32,
    /// Kind.
    pub kind: ListKind,
    /// Position among the lists.
    pub order: i32,
}

/// Media the user wants to send.
#[derive(Clone)]
pub struct OutgoingMedia {
    /// What to send it as.
    pub kind: OutgoingKind,
    /// Plaintext bytes.
    pub bytes: Vec<u8>,
    /// File name (documents).
    pub file_name: String,
    /// MIME type.
    pub mimetype: String,
    /// Optional caption.
    pub caption: Option<String>,
}

impl fmt::Debug for OutgoingMedia {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OutgoingMedia")
            .field("kind", &self.kind)
            .field("len", &self.bytes.len())
            .field("mimetype", &self.mimetype)
            .finish_non_exhaustive()
    }
}

/// Kind-specific data of an [`OutgoingMedia`].
#[derive(Debug, Clone, PartialEq)]
pub enum OutgoingKind {
    /// Photo with dimensions and a JPEG thumbnail.
    Image {
        /// Width in pixels.
        width: u32,
        /// Height in pixels.
        height: u32,
        /// Small JPEG thumbnail.
        thumbnail: Vec<u8>,
    },
    /// Any file.
    Document,
    /// MP4 video (H.264) with what the helper read from it. GIFs are sent
    /// as silent MP4s like WhatsApp does (`gif`).
    Video {
        /// Duration in seconds.
        seconds: u32,
        /// Pixel width.
        width: u32,
        /// Pixel height.
        height: u32,
        /// Small JPEG thumbnail of the first frame.
        thumbnail: Vec<u8>,
        /// Played as a looping, silent GIF.
        gif: bool,
    },
    /// Voice note (Ogg/Opus).
    Voice {
        /// Duration in seconds.
        seconds: u32,
        /// 64 samples, 0..=100.
        waveform: Vec<u8>,
    },
}

impl OutgoingMedia {
    /// The [`Content`] shown locally for this media before/after upload.
    pub fn local_content(&self, media: Option<MediaRef>) -> Content {
        match &self.kind {
            OutgoingKind::Image {
                width,
                height,
                thumbnail,
            } => Content::Image {
                caption: self.caption.clone(),
                width: *width,
                height: *height,
                thumbnail: Some(thumbnail.clone()),
                media,
            },
            OutgoingKind::Document => Content::Document {
                file_name: self.file_name.clone(),
                mimetype: self.mimetype.clone(),
                size: self.bytes.len() as u64,
                pages: None,
                caption: self.caption.clone(),
                media,
            },
            OutgoingKind::Video {
                seconds,
                width,
                height,
                thumbnail,
                gif,
            } => Content::Video {
                caption: self.caption.clone(),
                seconds: *seconds,
                width: *width,
                height: *height,
                thumbnail: Some(thumbnail.clone()),
                media,
                gif: *gif,
            },
            OutgoingKind::Voice { seconds, waveform } => Content::Voice {
                seconds: *seconds,
                ptt: true,
                waveform: waveform.clone(),
                media,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ReceiptStatus::*;

    #[test]
    fn receipts_only_move_forward() {
        assert_eq!(Pending.advance(Sent), Sent);
        assert_eq!(Sent.advance(Delivered), Delivered);
        assert_eq!(Delivered.advance(Read), Read);
        // Out-of-order: read arrives before delivered.
        assert_eq!(Sent.advance(Read).advance(Delivered), Read);
        assert_eq!(Read.advance(Sent), Read);
        assert_eq!(Read.advance(Played), Played);
        assert_eq!(Played.advance(Read), Played);
    }

    #[test]
    fn failure_rules() {
        assert_eq!(Pending.advance(Failed), Failed);
        assert_eq!(Sent.advance(Failed), Sent);
        assert_eq!(Delivered.advance(Failed), Delivered);
        assert_eq!(Failed.advance(Pending), Failed);
        assert_eq!(Failed.advance(Sent), Sent);
        assert_eq!(Failed.advance(Read), Read);
    }

    #[test]
    fn status_roundtrips_through_i64() {
        for s in [Pending, Failed, Sent, Delivered, Read, Played] {
            assert_eq!(ReceiptStatus::from_i64(s.to_i64()), s);
        }
    }

    #[test]
    fn previews() {
        assert_eq!(Content::Text("hi\nthere".into()).preview(), "hi");
        assert_eq!(
            Content::Voice {
                seconds: 75,
                ptt: true,
                waveform: vec![],
                media: None
            }
            .preview(),
            "🎤 1:15"
        );
        assert!(Content::Deleted.preview().contains("deleted"));
    }

    #[test]
    fn debug_never_prints_bodies() {
        let c = Content::Text("my secret body".into());
        assert!(!format!("{c:?}").contains("secret"));
        let d = Content::Document {
            file_name: "tax-return.pdf".into(),
            mimetype: "application/pdf".into(),
            size: 10,
            pages: None,
            caption: None,
            media: None,
        };
        assert!(!format!("{d:?}").contains("tax"));
    }

    #[test]
    fn lid_map_normalizes_only_known_pairs() {
        let mut m = LidMap::default();
        assert!(m.learn("123@lid", "34600111222@s.whatsapp.net"));
        assert!(!m.learn("123@lid", "34600111222@s.whatsapp.net"));
        assert!(!m.learn("34600111222@s.whatsapp.net", "123@lid"));
        assert_eq!(m.normalize("123@lid"), "34600111222@s.whatsapp.net");
        assert_eq!(m.normalize("999@lid"), "999@lid");
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn fallback_names() {
        assert_eq!(
            fallback_name("34600111222@s.whatsapp.net"),
            "+34 600 111 222"
        );
        assert_eq!(fallback_name("1203630@g.us"), "Group");
        assert!(is_group_jid("1203630@g.us"));
        assert_eq!(fallback_name("abc@lid"), "abc");
    }

    #[test]
    fn phone_numbers_are_grouped() {
        assert_eq!(format_phone("34600111222"), "+34 600 111 222");
        assert_eq!(format_phone("14155550123"), "+1 415 555 0123");
        assert_eq!(format_phone("447911123456"), "+44 791 112 3456");
        assert_eq!(format_phone("5491155551234"), "+54 911 555 512 34");
        assert_eq!(format_phone("351"), "+351");
        assert_eq!(format_phone(""), "+");
    }

    #[test]
    fn name_sources_rank_and_round_trip() {
        assert!(NameSource::Saved > NameSource::Business);
        assert!(NameSource::Business > NameSource::Push);
        for s in [NameSource::Push, NameSource::Business, NameSource::Saved] {
            assert_eq!(NameSource::from_code(s.code()), Some(s));
        }
        assert_eq!(NameSource::from_code(9), None);
    }

    fn text_msg(from_me: bool, ts: i64, status: ReceiptStatus) -> Message {
        Message {
            id: "1".into(),
            chat: "c".into(),
            sender: "s".into(),
            sender_name: None,
            from_me,
            timestamp: ts,
            content: Content::Text("hi".into()),
            status,
            expires_at: None,
            reply: None,
            edited: false,
            forwarded: None,
            mentions: Vec::new(),
            reactions: Vec::new(),
        }
    }

    #[test]
    fn edit_and_revoke_windows() {
        let now = 1_000_000;
        let m = text_msg(true, now - 60, Sent);
        assert!(m.can_edit(now));
        assert!(m.can_revoke(now));
        // 15 minutes to edit, then the option disappears (delete stays).
        let old = text_msg(true, now - EDIT_WINDOW_SECS - 1, Delivered);
        assert!(!old.can_edit(now));
        assert!(old.can_revoke(now));
        let edge = text_msg(true, now - EDIT_WINDOW_SECS, Read);
        assert!(edge.can_edit(now));
        // About two and a half days to delete for everyone.
        let ancient = text_msg(true, now - REVOKE_WINDOW_SECS - 1, Read);
        assert!(!ancient.can_revoke(now));
        // Never for other people's messages, pending or failed sends.
        assert!(!text_msg(false, now - 1, Read).can_edit(now));
        assert!(!text_msg(false, now - 1, Read).can_revoke(now));
        assert!(!text_msg(true, now - 1, Pending).can_edit(now));
        assert!(!text_msg(true, now - 1, Failed).can_revoke(now));
        // Only text can be edited here; deleted messages cannot be deleted again.
        let mut img = text_msg(true, now - 1, Sent);
        img.content = Content::Deleted;
        assert!(!img.can_edit(now) && !img.can_revoke(now));
    }

    #[test]
    fn edits_merge_into_the_original_content() {
        let t = |s: &str| Content::Text(s.into());
        assert_eq!(merge_edit(&t("a"), &t("b")), Some(t("b")));
        assert_eq!(
            merge_edit(&t("a"), &t("")),
            None,
            "empty text is not an edit"
        );
        let img = Content::Image {
            caption: Some("old".into()),
            width: 2,
            height: 2,
            thumbnail: None,
            media: None,
        };
        match merge_edit(&img, &t("new")) {
            Some(Content::Image { caption, width, .. }) => {
                assert_eq!(caption.as_deref(), Some("new"));
                assert_eq!(width, 2);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(merge_edit(&Content::Deleted, &t("x")), None);
        assert_eq!(merge_edit(&t("a"), &Content::Deleted), None);
    }

    #[test]
    fn quotes_are_capped_and_never_logged() {
        let mut m = text_msg(false, 1, Read);
        m.content = Content::Text("y".repeat(1000));
        let q = Quote::of(&m);
        assert_eq!(q.preview.chars().count(), QUOTE_PREVIEW_MAX);
        let mut secret = text_msg(false, 1, Read);
        secret.content = Content::Text("my secret".into());
        assert!(!format!("{:?}", Quote::of(&secret)).contains("secret"));
        assert!(
            !format!(
                "{:?}",
                Reaction {
                    reactor: "34600111222@s.whatsapp.net".into(),
                    emoji: "👍".into(),
                    ts: 1
                }
            )
            .contains("346")
        );
        assert_eq!(format!("{:?}", secret.key()), "MsgKey(from_me=false)");
    }

    #[test]
    fn video_and_sticker_previews() {
        let v = Content::Video {
            caption: None,
            seconds: 75,
            width: 1,
            height: 1,
            thumbnail: None,
            media: None,
            gif: false,
        };
        assert_eq!(v.preview(), "🎥 1:15");
        let g = Content::Video {
            caption: None,
            seconds: 3,
            width: 1,
            height: 1,
            thumbnail: None,
            media: None,
            gif: true,
        };
        assert_eq!(g.preview(), "GIF");
        assert_eq!(v.kind_tag(), "video");
        let s = Content::Sticker {
            width: 1,
            height: 1,
            animated: false,
            media: None,
        };
        assert_eq!(s.preview(), "Sticker");
        assert!(s.media().is_none());
    }
}
