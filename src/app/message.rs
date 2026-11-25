//! The `Message` enum: UI events, protocol events and task completions.

use std::path::PathBuf;
use std::sync::Arc;

use crate::app::state::Filter;
use crate::media::audio::Pcm;
use crate::media::avatar::Tile;
use crate::net::{self, ChatId, Content, Message as ChatMessage, OutgoingMedia};
use crate::store::{SearchHit, Window};

/// Every state change goes through one of these.
#[derive(Debug, Clone)]
pub enum Message {
    /// User input.
    Ui(Ui),
    /// Pushed by the protocol client.
    Net(net::Event),
    /// A `Task` finished (boxed: some results are large).
    Done(Box<Done>),
    /// A frame or status of the video helper (boxed: frames are large).
    Video(Box<crate::app::video::VideoMsg>),
    /// Fast tick while a voice note plays / records.
    Tick,
    /// One-second tick while typing indicators, our own composing state or
    /// the jump highlight are live.
    LiveTick,
    /// Window focus changed.
    Focus(bool),
    /// Screenshot mode: capture now.
    CaptureScreenshot,
    /// First frame has been presented (startup trace).
    FirstFrame,
    /// Tray click / menu entry, a second launch or a clicked notification.
    External(crate::ext::External),
    /// The user asked to close a window (X button, Alt+F4).
    CloseRequested(iced::window::Id),
    /// A window is gone.
    WindowClosed(iced::window::Id),
    /// A window was resized (remembered for the next open).
    WindowResized(iced::window::Id, iced::Size),
    /// A window moved (not reported on Wayland).
    WindowMoved(iced::window::Id, iced::Point),
    /// Quit the app (tray menu).
    Quit,
    /// In-app updater.
    Update(crate::update::UpdateMsg),
}

impl Message {
    /// Wraps a task result.
    pub fn done(d: Done) -> Self {
        Message::Done(Box::new(d))
    }
}

/// User input.
#[derive(Debug, Clone)]
pub enum Ui {
    /// Search box edited.
    Search(String),
    /// Filter chip clicked.
    Filter(Filter),
    /// Chat row clicked.
    OpenChat(ChatId),
    /// Escape / back.
    CloseChat,
    /// Composer edited (typing, cursor moves, paste, Shift+Enter).
    ComposerAction(iced::widget::text_editor::Action),
    /// Enter / send button.
    Send,
    /// Paperclip.
    Attach,
    /// Ctrl+V / Shift+Insert: a copied image or files open the preview.
    Paste,
    /// Files dragged over or dropped on the window.
    Drop(crate::app::drop::DropMsg),
    /// Attachment preview caption edited.
    AttachCaption(String),
    /// Attachment preview "send".
    AttachSend,
    /// Attachment preview closed without sending.
    AttachCancel,
    /// Emoji panel toggle.
    ToggleEmoji,
    /// Emoji picked.
    Emoji(&'static str),
    /// Emoji picker search box edited.
    EmojiSearch(String),
    /// Emoji picker category tab: scroll to its section.
    EmojiCategory(crate::app::emoji::Category),
    /// Mic pressed: start recording.
    StartRecording,
    /// Stop recording; send or discard.
    StopRecording {
        /// Send the note (false = discard).
        send: bool,
    },
    /// Play / resume a voice note (message id).
    PlayVoice(String),
    /// Pause playback.
    PauseVoice,
    /// Open (download + launch) a document.
    OpenDocument(String),
    /// Show an image full size.
    OpenImage(String),
    /// Close the image viewer.
    CloseViewer,
    /// Zoom the viewer by `factor` around `at` (offset from the centre).
    ViewerZoom {
        /// Multiplier (> 1 zooms in).
        factor: f32,
        /// Point kept under the cursor, relative to the window centre.
        at: iced::Vector,
    },
    /// "0" / double click: back to the fitted picture.
    ViewerResetZoom,
    /// Drag of the zoomed picture.
    ViewerPan {
        /// Pointer movement.
        delta: iced::Vector,
        /// Fitted picture size at zoom 1 (for clamping).
        fitted: (f32, f32),
    },
    /// Previous (-1) / next (+1) photo of the chat.
    ViewerStep(i32),
    /// "Save as…".
    ViewerSave,
    /// "Open externally".
    ViewerOpenExternal,
    /// Left / Right arrow: the photo viewer or the status viewer steps.
    Arrow(i32),
    /// Conversation header clicked: open / close the info drawer.
    ToggleInfo,
    /// Close the info drawer.
    CloseInfo,
    /// "Message info" from the menu of one of our messages.
    MessageInfo(String),
    /// Close the "Message info" panel.
    CloseMessageInfo,
    /// Open a gallery tab of the info drawer (`None` = back).
    InfoGallery(Option<crate::app::info::Tab>),
    /// Mute / unmute a chat.
    ToggleMute(ChatId),
    /// "Block" pressed: ask first.
    AskBlock,
    /// Block confirmation dismissed.
    CancelBlock,
    /// Block (`true`) / unblock the contact of the drawer.
    SetBlocked(bool),
    /// Conversation scrolled; load older when near the top.
    Scrolled {
        /// Relative offset from the bottom anchor: 0 = newest, 1 = oldest.
        relative_y: f32,
        /// Absolute distance scrolled from the newest end, in pixels.
        offset_y: f32,
    },
    /// "Click to reload QR code".
    ReloadQr,
    /// Logout / unlink this device.
    Logout,
    /// Header menu toggle.
    ToggleMenu,
    /// Hide the toast.
    DismissToast,
    /// Mock mode: pretend the phone scanned the QR code.
    SimulateScan,
    /// Retry a failed message.
    Retry(String),
    /// Reconnect after "opened elsewhere" / offline.
    Reconnect,
    /// Backoff timer fired for this client generation.
    AutoReconnect {
        /// Generation the timer was armed for.
        generation: u32,
    },
    /// Discard an undecryptable session and pair again (user confirmed).
    ResetSession,
    /// The pointer entered a message row (hover actions).
    HoverMessage(String),
    /// The pointer left a message row.
    UnhoverMessage(String),
    /// Smiley button: open / close the quick-reaction picker of a message.
    ToggleReactionPicker(String),
    /// Click outside the floating reaction picker / menu.
    ClosePopup,
    /// Chevron / right click: open / close the message menu.
    ToggleMessageMenu(String),
    /// React with an emoji (same emoji again removes the reaction).
    React {
        /// Message id.
        id: String,
        /// Emoji.
        emoji: String,
    },
    /// Reply to a message: reply bar above the composer.
    StartReply(String),
    /// Close the reply bar.
    CancelReply,
    /// Scroll to (loading if needed) a message, e.g. a quoted original.
    JumpTo(String),
    /// Edit one of our messages.
    StartEdit(String),
    /// Leave edit mode.
    CancelEdit,
    /// Open the delete dialog.
    AskDelete(String),
    /// Delete, for everyone or just here.
    ConfirmDelete {
        /// Message id.
        id: String,
        /// "Delete for everyone".
        everyone: bool,
    },
    /// Close the delete dialog / reaction list.
    CloseDialog,
    /// Copy the text of a message to the clipboard.
    CopyText(String),
    /// Show who reacted to a message.
    ShowReactions(String),
    /// Play a video message in the in-app viewer.
    OpenVideo(String),
    /// "Open externally": download the video and hand it to the system player.
    OpenVideoExternal(String),
    /// Close the video viewer (stops playback).
    CloseVideo,
    /// Play / pause (also a click on the picture or Space).
    VideoToggle,
    /// Seek slider moved to this fraction (0..=1).
    VideoSeek(f32),
    /// Seek slider released: seek.
    VideoSeekRelease,
    /// Volume slider (0..=100).
    VideoVolume(u8),
    /// Mute / unmute.
    VideoMute,
    /// Toggle full screen.
    VideoFullscreen,
    /// The chat list scrolled (avatars load for what is visible).
    SidebarScrolled {
        /// Pixels from the top.
        offset_y: f32,
        /// Viewport height.
        height: f32,
    },
    /// Rail sections, panels, starred messages, communities, statuses.
    Nav(crate::app::nav::NavMsg),
    /// Call buttons (see `app::call`).
    Call(crate::app::call::CallUi),
    /// Search icon in the conversation header.
    ToggleChatSearch,
    /// In-chat search box edited.
    ChatSearch(String),
    /// Next (older) / previous (newer) in-chat hit.
    ChatSearchStep {
        /// +1 older, -1 newer.
        delta: i32,
    },
    /// "Jump to latest" after browsing old history.
    JumpLatest,
    /// Settings page from the rail or the header menu.
    OpenSettings,
    /// Back from the settings page.
    CloseSettings,
    /// Flip a background / notification setting.
    Toggle(Setting),
    /// "Archived" row: show the archived chats.
    OpenArchived,
    /// Back from the archived chats.
    CloseArchived,
    /// Right click on a chat row.
    ChatMenu(ChatId),
    /// Right click on a custom list chip (list id).
    ChipMenu(String),
    /// Dismiss the chat list context menu.
    CloseSidebarMenu,
    /// Archive / unarchive a chat.
    ToggleArchive(ChatId),
    /// Pin / unpin a chat.
    TogglePin(ChatId),
    /// Add to / remove from Favourites.
    ToggleFavourite(ChatId),
    /// Add to / remove from a custom list.
    ToggleInList {
        /// Chat.
        chat: ChatId,
        /// List id.
        list: String,
    },
    /// "+" chip: create a list.
    NewList,
    /// Rename a list (id).
    RenameList(String),
    /// List name edited in the dialog.
    ListName(String),
    /// Save the list dialog.
    SaveList,
    /// Ask before deleting a list (id).
    AskDeleteList(String),
    /// Delete a list (id).
    ConfirmDeleteList(String),
    /// Close the list dialog.
    CloseListDialog,
    /// Forward messages / selection mode.
    Forward(crate::app::forward::Fwd),
    /// Mark read / unread, mute, clear or delete a chat.
    ChatAct(crate::app::chat_actions::ChatAct),
    /// Open a link from a message in the default browser.
    OpenLink(String),
    /// Open a message found by the chat list search.
    OpenHit {
        /// Chat.
        chat: ChatId,
        /// Message id.
        id: String,
    },
}

/// A boolean preference that can be flipped from the settings page (and,
/// for some, the tray menu).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setting {
    /// Closing the window keeps the app in the tray.
    CloseToTray,
    /// Start without a window.
    StartMinimized,
    /// Write / remove the autostart entry.
    Autostart,
    /// No desktop notifications.
    Muted,
}

/// A downloaded blob, already prepared for display where needed.
#[derive(Clone)]
pub enum Fetched {
    /// Raw bytes (images, documents).
    Bytes(Arc<Vec<u8>>),
    /// A decoded sticker frame.
    Rgba {
        /// Width.
        width: u32,
        /// Height.
        height: u32,
        /// RGBA pixels.
        pixels: Arc<Vec<u8>>,
    },
}

impl std::fmt::Debug for Fetched {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Fetched::Bytes(b) => write!(f, "Bytes({})", b.len()),
            Fetched::Rgba { width, height, .. } => write!(f, "Rgba({width}x{height})"),
        }
    }
}

/// Geometry needed to scroll a message into view.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Located {
    /// Top of the message relative to the top of the scrollable content.
    pub top: f32,
    /// Height of the message.
    pub height: f32,
    /// Height of the viewport.
    pub viewport: f32,
    /// Height of the whole content.
    pub content: f32,
    /// Top of the message relative to the top of the viewport right now.
    pub screen_top: f32,
}

/// Task completions.
#[derive(Debug, Clone)]
pub enum Done {
    /// A forced app-state resync finished.
    Resync {
        /// Attempt number in this run, from 1.
        attempt: u32,
        /// What was applied.
        result: Result<net::ResyncReport, String>,
    },
    /// Time to retry the resync.
    ResyncDue,
    /// The autostart entry was written (`true`) or removed (`false`).
    Autostart(Result<bool, String>),
    /// Messages loaded from the cache.
    Loaded {
        /// Chat.
        chat: ChatId,
        /// Messages, oldest first.
        msgs: Vec<ChatMessage>,
        /// Older page (prepend) vs. initial load.
        older: bool,
        /// More history in the cache.
        has_more: bool,
    },
    /// Text send finished.
    Sent {
        /// Chat.
        chat: ChatId,
        /// Message id.
        id: String,
        /// Error text on failure.
        result: Result<(), String>,
    },
    /// Media send finished.
    MediaSent {
        /// Chat.
        chat: ChatId,
        /// Message id.
        id: String,
        /// Final content.
        result: Result<Content, String>,
    },
    /// Media downloaded.
    Downloaded {
        /// Cache key.
        key: String,
        /// Bytes or error.
        result: Result<Fetched, String>,
    },
    /// A profile picture was fetched (`None` = the contact has none).
    Avatar {
        /// Contact or group.
        jid: String,
        /// Decoded tile.
        result: Result<Option<Tile>, String>,
    },
    /// A reaction we sent finished; on failure the previous one is restored.
    ReactionSent {
        /// Chat.
        chat: ChatId,
        /// Message id.
        id: String,
        /// Reaction before ours (to restore on failure).
        previous: Option<String>,
        /// Error text on failure.
        result: Result<(), String>,
    },
    /// An edit / revoke / delete-for-me request finished.
    ActionDone {
        /// What was attempted.
        what: &'static str,
        /// Error text on failure.
        result: Result<(), String>,
    },
    /// Group receipts folded into aggregate ticks.
    GroupStatus {
        /// Chat.
        chat: ChatId,
        /// Other participants in the group, if known.
        size: Option<usize>,
        /// Messages whose tick changed.
        changed: Vec<(String, net::ReceiptStatus)>,
    },
    /// Result of the chat list message search.
    ListSearch {
        /// Generation the query was issued for.
        generation: u32,
        /// Hits, newest first.
        hits: Vec<SearchHit>,
    },
    /// Result of the in-chat search.
    ChatSearch {
        /// Generation the query was issued for.
        generation: u32,
        /// Chat searched.
        chat: ChatId,
        /// Hits, newest first.
        hits: Vec<SearchHit>,
    },
    /// A window of history around a message was loaded (jump).
    Window {
        /// Chat.
        chat: ChatId,
        /// The message to bring into view.
        anchor: String,
        /// Loaded messages.
        window: Window,
    },
    /// Newer messages after a jump.
    Newer {
        /// Chat.
        chat: ChatId,
        /// Messages, oldest first.
        msgs: Vec<ChatMessage>,
        /// Still more.
        has_newer: bool,
    },
    /// Where a message sits in the conversation scrollable.
    Located(Option<Located>),
    /// Where the row of a message with an open popup is on screen.
    PopupAt {
        /// Message id the popup belongs to.
        id: String,
        /// Row geometry.
        loc: Option<Located>,
    },
    /// The newest remaining message of a chat after a local delete.
    LastMessage {
        /// Chat.
        chat: ChatId,
        /// Newest message, if any.
        last: Option<Box<ChatMessage>>,
    },
    /// A video was saved and handed to the system player.
    VideoOpened(Result<PathBuf, String>),
    /// The video file for the in-app viewer is on disk (or failed).
    VideoFile {
        /// Message id.
        id: String,
        /// Path or error text.
        result: Result<PathBuf, String>,
    },
    /// A poster frame was extracted for a video without an inline thumbnail.
    Poster {
        /// Chat.
        chat: ChatId,
        /// Message id.
        id: String,
        /// JPEG or error text.
        result: Result<Vec<u8>, String>,
    },
    /// The image viewer's full-resolution download finished.
    ViewerImage {
        /// Message id.
        id: String,
        /// Bytes or error.
        result: Result<Arc<Vec<u8>>, String>,
    },
    /// An info drawer query finished.
    Info(Box<crate::app::info::InfoDone>),
    /// The receipts of the "Message info" panel were read.
    MsgInfo {
        chat: String,
        id: String,
        rows: Vec<crate::store::ReceiptDetail>,
    },
    /// A photo was saved / handed to the system viewer (`None` = cancelled).
    ViewerSaved(Result<Option<PathBuf>, String>),
    /// Voice note downloaded and decoded.
    VoiceReady {
        /// Message id.
        id: String,
        /// PCM or error.
        result: Result<Arc<Pcm>, String>,
    },
    /// Voice note encoded and ready to send.
    Prepared(Option<Result<Box<OutgoingMedia>, String>>),
    /// Picked or pasted media prepared for the attachment preview
    /// (`None` = dialog cancelled or a plain text paste).
    Staged {
        /// Chat that was open when the paste / pick started.
        chat: ChatId,
        /// The prepared media.
        staged: Option<Box<crate::app::paste::Staged>>,
    },
    /// Document saved to disk (and opened unless it is an executable type).
    DocumentOpened(Result<(PathBuf, bool), String>),
    /// Messages persisted; `new_ids` were not in the cache before.
    MessagesStored {
        /// Messages (addressing normalised).
        messages: Vec<ChatMessage>,
        /// Ids that are new.
        new_ids: Vec<String>,
        /// Where they came from.
        origin: crate::net::Origin,
    },
    /// The audio output thread is ready (or failed); then play `then`.
    PlayerReady {
        /// Player or error.
        result: Result<crate::media::audio::Player, String>,
        /// Message id to play.
        then: String,
    },
    /// Background store write finished.
    Stored(Result<(), String>),
    /// Logout / unlink cleanup finished, with any problems encountered.
    CleanedUp(Vec<String>),
    /// Screenshot captured.
    Screenshot(Box<iced::window::Screenshot>),
    /// Screenshot written.
    ScreenshotSaved(Result<PathBuf, String>),
    /// Syncing an archive / pin change failed: restore `undo`.
    FlagFailed {
        /// Chat.
        chat: ChatId,
        /// The previous value.
        undo: net::ChatFlag,
        /// Error text.
        error: String,
    },
    /// Syncing a list membership failed: restore `undo`.
    MemberFailed {
        /// List id.
        list: String,
        /// Chat.
        chat: ChatId,
        /// Previous membership.
        undo: bool,
        /// Error text.
        error: String,
    },
    /// A task started by a navigation / status action finished.
    Nav(Box<crate::app::nav::NavDone>),
    /// A chat action task finished.
    ChatAct(crate::app::chat_actions::ChatActDone),
    /// Nothing to do.
    Nothing,
    /// A call task finished.
    Call(crate::app::call::CallDone),
}
