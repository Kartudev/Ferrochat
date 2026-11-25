//! App state and the single `update` mutation point.

mod actions;
pub mod call;
pub mod composer;
pub mod drop;
pub mod emoji;
pub mod info;
pub mod input;
pub mod linkify;
pub mod lists;
pub mod media_fetch;
pub mod msg_info;
pub mod video;
pub mod viewer;
pub use actions::{PopupPos, popup_top, row_id};
mod background;
pub mod chat_actions;
pub mod forward;
pub use background::WindowGeom;
#[cfg(test)]
mod call_flows;
#[cfg(test)]
mod chat_action_flows;
#[cfg(test)]
mod flows;
#[cfg(test)]
mod forward_flows;
pub mod live;
#[cfg(test)]
mod media_flows;
pub mod message;
pub mod nav;
pub mod paste;
#[cfg(test)]
mod perf;
#[cfg(test)]
mod reupload_flows;
#[cfg(test)]
mod social_flows;
pub mod state;
pub mod status;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod update_flows;
mod updates;
pub mod window;

use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use iced::widget::{image, operation};
use iced::{Subscription, Task};

pub use message::{Done, Fetched, Located, Message, Setting, Ui};
use state::{ChatList, Conversation, Filter, NameBook};

use crate::auth::secrets::{KeyringStore, MemoryStore, SIGNAL_STORE_KEY, SecretStore};
use crate::auth::{AuthEvent, AuthState};
use crate::config::{Config, ScreenshotTarget};
use crate::media::ByteLru;
use crate::media::audio::{self, Pcm, Player, Recorder};
use crate::net::{
    self, ChatState, ClientHandle, Connection, Content, Event, LidMap, Message as ChatMessage,
    Origin, OutgoingKind, OutgoingMedia, Quote, ReceiptStatus,
};
use crate::store::{SearchHit, Store};
use crate::ui::theme::FerroTheme;

/// Id of the conversation scrollable (for snapping to the newest message).
pub const CONVERSATION_SCROLL: &str = "conversation";
/// Messages loaded per page from the cache.
pub const PAGE: usize = 60;
/// Image cache budget (architecture §3).
pub const IMAGE_BUDGET: usize = 48 * 1024 * 1024;
/// Id of the composer text input (focus after "reply" / "edit").
pub const COMPOSER_ID: &str = "composer";
/// Id of the caption input of the attachment preview.
pub const ATTACH_CAPTION_ID: &str = "attach-caption";
/// Id of the in-chat search input.
pub const CHAT_SEARCH_ID: &str = "chat-search";
/// Decoded profile pictures kept in memory (about 36 kB each).
pub const AVATAR_BUDGET: usize = 12 * 1024 * 1024;
/// Profile picture lookups in flight at once.
pub const MAX_AVATAR_FETCHES: usize = 3;
/// Profile pictures waiting for a slot (oldest dropped beyond this).
pub const MAX_AVATAR_QUEUE: usize = 200;
/// How long a jumped-to message stays highlighted.
pub const HIGHLIGHT_FOR: Duration = Duration::from_millis(1800);
/// Largest sticker downloaded (bytes); real ones are ~100 kB.
pub const MAX_STICKER_BYTES: u64 = 2 * 1024 * 1024;
/// Videos kept in the cache directory.
pub const MAX_VIDEO_FILES: usize = 4;
/// Cache key / value recording that the full app-state resync was requested.
const RESYNC_KEY: &str = "appstate_resync";
// "3": the first resync that really replays the snapshots (contacts and
// starred messages). "1" (v0.8.0) and "2" (development before the reset
// fix) ran into wacore's stale-snapshot check and replayed nothing.
const RESYNC_VERSION: &str = "3";
/// Resync attempts per run before waiting for the next start.
const RESYNC_MAX_ATTEMPTS: u32 = 4;
/// An account whose snapshot applied with no contact at all is done after
/// this many attempts (it may really have an empty address book).
const RESYNC_MAX_EMPTY: u32 = 3;
/// Delay before retrying a resync that did not deliver.
#[cfg(not(test))]
const RESYNC_RETRY: Duration = Duration::from_secs(45);
#[cfg(test)]
const RESYNC_RETRY: Duration = Duration::from_millis(1);
/// Unprocessed reactions / edits buffered behind a message insert.
const MAX_DEFERRED: usize = 5000;

/// Progress of the forced app-state resync in this run.
#[derive(Debug, Default)]
struct Resync {
    /// Delivered (or already recorded in the cache).
    done: bool,
    /// A request is in flight.
    running: bool,
    /// Requests made in this run.
    attempts: u32,
}

/// What a message's floating popup shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Popup {
    /// Quick-reaction picker.
    Reactions,
    /// Reply / copy / edit / delete menu.
    Menu,
}

/// A modal dialog over the conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dialog {
    /// "Delete message?" for this message id.
    Delete(String),
    /// "Reactions" list for this message id.
    Reactions(String),
}

/// What a right-click menu in the chat list belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuTarget {
    /// A chat row.
    Chat(String),
    /// A custom list chip (list id).
    Chip(String),
}

/// A right-click menu floating at the pointer.
#[derive(Debug, Clone, PartialEq)]
pub struct SidebarMenu {
    /// What it acts on.
    pub target: MenuTarget,
    /// Window position of the click.
    pub at: iced::Point,
}

/// The dialog to create, rename or delete a list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListDialog {
    /// New list (`id` empty) or rename.
    Edit {
        /// Existing list id, `None` for a new list.
        id: Option<String>,
        /// Name typed.
        name: String,
    },
    /// "Delete list?" for this id.
    Delete(String),
}

/// The in-chat search bar.
#[derive(Debug, Clone, Default)]
pub struct ChatSearch {
    /// Text typed.
    pub query: String,
    /// Hits, newest first.
    pub hits: Vec<SearchHit>,
    /// Current hit (index into `hits`).
    pub index: usize,
    /// Generation of the query the hits belong to.
    pub generation: u32,
}

/// Voice note playback state.
#[derive(Debug, Clone)]
pub struct VoiceState {
    /// Message id.
    pub id: String,
    /// Still downloading / decoding.
    pub loading: bool,
    /// Paused by the user.
    pub paused: bool,
    /// Position.
    pub position_ms: u64,
    /// Total length.
    pub duration_ms: u64,
}

/// Root state.
pub struct App {
    pub(crate) cfg: Config,
    pub(crate) theme: FerroTheme,
    pub(crate) store: Store,
    secrets: Arc<dyn SecretStore>,
    pub(crate) auth: AuthState,
    pub(crate) connection: Connection,
    client: Option<ClientHandle>,
    pub(crate) own_jid: Option<String>,
    net_generation: u32,
    pub(crate) chats: ChatList,
    pub(crate) search: String,
    pub(crate) filter: Filter,
    /// The chat list shows archived chats.
    pub(crate) archived_view: bool,
    /// Right-click menu in the chat list.
    pub(crate) sidebar_menu: Option<SidebarMenu>,
    /// Create / rename / delete list dialog.
    pub(crate) list_dialog: Option<ListDialog>,
    /// Mute / clear / delete chat dialog.
    pub(crate) chat_dialog: Option<chat_actions::ChatDialog>,
    /// "Select messages" mode of the open conversation.
    pub(crate) selection: Option<forward::Selection>,
    /// "Forward message to" dialog.
    pub(crate) forward: Option<forward::ForwardDialog>,
    pub(crate) open: Option<Conversation>,
    pending_read: usize,
    /// Display JID -> the JID messages came from (LID-addressed chats and
    /// group members), so read receipts go where the message came from.
    wire: HashMap<String, String>,
    pub(crate) composer: composer::Composer,
    /// Drafts of the chats that are not open.
    pub(crate) drafts: composer::Drafts,
    pub(crate) emoji_open: bool,
    /// Emoji picker search and recent emoji.
    pub(crate) emoji: emoji::Picker,
    /// Attachment preview (picked or pasted media waiting for "send").
    pub(crate) attach: Option<paste::AttachPreview>,
    /// Files dragged over / dropped on the window.
    pub(crate) drop: drop::DropState,
    /// The "Message info" panel.
    pub(crate) msg_info: Option<msg_info::MsgInfo>,
    /// Re-uploaded media waiting to be applied to the open conversation.
    pub(crate) fresh_media: media_fetch::FreshMedia,
    pub(crate) menu_open: bool,
    pub(crate) qr: Option<image::Handle>,
    pub(crate) images: ByteLru<String, image::Handle>,
    pub(crate) thumbs: HashMap<String, image::Handle>,
    /// Videos a poster frame was tried for (each at most once per run).
    pub(crate) posters_tried: HashSet<String>,
    /// A poster extraction is running (one at a time).
    poster_busy: bool,
    downloading: HashSet<String>,
    player: Option<Player>,
    pub(crate) voice: Option<VoiceState>,
    voice_cache: ByteLru<String, Arc<Pcm>>,
    recorder: Option<Recorder>,
    pub(crate) recording_since: Option<Instant>,
    pub(crate) toast: Option<String>,
    pub(crate) focused: bool,
    pub(crate) viewer: Option<viewer::ImageViewer>,
    /// Contact / group info drawer.
    pub(crate) info: Option<info::InfoPanel>,
    pub(crate) sync_progress: Option<u32>,
    started: Instant,
    first_frame_logged: bool,
    lid_map: LidMap,
    /// The session store exists but cannot be decrypted; the login screen
    /// offers to discard it.
    pub(crate) session_unreadable: bool,
    /// Message ids waiting for a download slot (images, auto-download).
    download_queue: VecDeque<String>,
    /// App-level reconnect backoff after a non-transient refusal.
    offline_attempt: u32,
    /// Calls: the ringing / current call and the logged call ids.
    pub(crate) calls: crate::call::Calls,
    /// Audio devices and ringtone of the current call.
    call_media: call::CallMedia,
    /// The video viewer (at most one: one decoder at a time).
    pub(crate) video: Option<video::VideoView>,
    video_session: u32,
    /// The window is full screen because of the video viewer.
    pub(crate) video_fullscreen: bool,
    /// The one-time full app-state resync (names, archive, lists).
    resync: Resync,
    /// Message under the pointer (hover actions).
    pub(crate) hovered: Option<String>,
    /// Open reaction picker / menu.
    pub(crate) popup: Option<(String, Popup)>,
    /// Where the popup's message row is on screen.
    pub(crate) popup_pos: Option<PopupPos>,
    /// Reply bar contents.
    pub(crate) reply_to: Option<Quote>,
    /// Message being edited.
    pub(crate) editing: Option<String>,
    /// Modal dialog.
    pub(crate) dialog: Option<Dialog>,
    /// Message highlighted after a jump.
    pub(crate) highlight: Option<(String, Instant)>,
    /// Who is typing where.
    pub(crate) typing: live::TypingTracker,
    compose: live::ComposeThrottle,
    /// Online / last seen.
    pub(crate) presence: live::PresenceBook,
    subscribed: HashSet<String>,
    /// Profile pictures by JID.
    pub(crate) avatars: ByteLru<String, image::Handle>,
    avatar_none: HashSet<String>,
    avatar_inflight: HashSet<String>,
    avatar_queue: VecDeque<String>,
    /// Disk cache for profile pictures (`None` in mock mode: no disk writes).
    avatar_dir: Option<PathBuf>,
    /// Chat list rows currently on screen (first, last).
    sidebar_rows: (usize, usize),
    group_sizes: HashMap<String, usize>,
    /// Message batches being written to the cache.
    pending_inserts: usize,
    /// Reactions and edits waiting for those writes.
    deferred: Vec<Event>,
    /// Message search hits for the chat list query.
    pub(crate) message_hits: Vec<SearchHit>,
    search_generation: u32,
    /// In-chat search bar.
    pub(crate) chat_search: Option<ChatSearch>,
    /// Screenshot mode: the scripted interaction already ran.
    scene_started: bool,
    /// Screenshot mode: the navigation scene already ran.
    nav_scene_done: bool,
    /// Last known scroll offset of the conversation.
    scroll_y: f32,
    /// Rail section, panels, communities, stars, log out confirmation.
    pub(crate) nav: nav::NavState,
    /// Statuses and their viewer.
    pub(crate) status: status::StatusState,
    /// Persisted preferences (background mode, mute).
    pub(crate) settings: crate::settings::Settings,
    /// The tray service reported at least once (up or not).
    pub(crate) tray_checked: bool,
    /// The autostart entry exists.
    pub(crate) autostart: bool,
    /// The settings page replaces the conversation pane.
    pub(crate) settings_open: bool,
    /// Whether the main window exists and whether a tray backs it.
    window: window::WindowMode,
    /// iced id of the main window while it exists.
    window_id: Option<iced::window::Id>,
    /// Size / position to reopen the window with.
    geom: WindowGeom,
    /// Last tray model handed to the tray service.
    tray_sent: Option<crate::tray::TrayModel>,
    /// In-app updater (`app/updates.rs`).
    pub(crate) updater: crate::update::Updater,
}

/// Subscription key for the protocol stream. Changing `generation`
/// restarts the client (reload QR, after logout).
#[derive(Clone)]
struct NetKey {
    cfg: Config,
    generation: u32,
    secrets: Arc<dyn SecretStore>,
}

impl Hash for NetKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.cfg.hash(state);
        self.generation.hash(state);
    }
}

fn now_secs() -> i64 {
    chrono::Utc::now().timestamp()
}

fn net_stream(key: &NetKey) -> futures::stream::BoxStream<'static, Message> {
    let key = key.clone();
    futures::stream::once(async move {
        if key.cfg.mock {
            let paired = !matches!(key.cfg.screenshot, Some((ScreenshotTarget::Login, _)))
                && key.generation == 0;
            let opts = net::fake::FakeOptions {
                paired,
                auto_reply: true,
                ..Default::default()
            };
            let (_client, stream) = net::fake::start(opts, crate::mock::mock_data(now_secs()));
            stream.boxed()
        } else {
            net::whatsapp::start(key.cfg.data_dir.clone(), key.cfg.signal_db(), key.secrets).boxed()
        }
    })
    .flatten()
    .map(Message::Net)
    .boxed()
}

fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
    map: impl FnOnce(T) -> Message + Send + 'static,
) -> Task<Message> {
    Task::perform(
        async move { tokio::task::spawn_blocking(f).await.ok() },
        move |r| match r {
            Some(v) => map(v),
            None => Message::done(Done::Nothing),
        },
    )
}

fn store_task(
    store: &Store,
    f: impl FnOnce(&Store) -> crate::error::Result<()> + Send + 'static,
) -> Task<Message> {
    let s = store.clone();
    blocking(
        move || f(&s).map_err(|e| e.to_string()),
        |r| Message::done(Done::Stored(r)),
    )
}

/// Encodes a QR payload into an image handle (white quiet zone, black modules).
pub fn qr_handle(payload: &str) -> Option<image::Handle> {
    let code = qrcode::QrCode::new(payload.as_bytes()).ok()?;
    let w = code.width();
    let scale = 8usize;
    let quiet = 4usize;
    let side = (w + quiet * 2) * scale;
    let colors = code.to_colors();
    let mut rgba = vec![255u8; side * side * 4];
    for y in 0..w {
        for x in 0..w {
            if colors[y * w + x] == qrcode::Color::Dark {
                for dy in 0..scale {
                    for dx in 0..scale {
                        let px = (x + quiet) * scale + dx;
                        let py = (y + quiet) * scale + dy;
                        let i = (py * side + px) * 4;
                        rgba[i] = 0x11;
                        rgba[i + 1] = 0x1b;
                        rgba[i + 2] = 0x21;
                    }
                }
            }
        }
    }
    Some(image::Handle::from_rgba(side as u32, side as u32, rgba))
}

impl App {
    /// Boot: open the cache synchronously, render from it, start the client.
    pub fn boot(cfg: Config) -> (Self, Task<Message>) {
        let started = Instant::now();
        let mut cache_fallback = false;
        let store = if cfg.mock {
            Store::open_in_memory()
        } else {
            Store::open(&cfg.cache_db())
        }
        .or_else(|e| {
            tracing::error!(error = %e, "cache unreadable; using an in-memory cache");
            cache_fallback = true;
            Store::open_in_memory()
        });
        let (store, chats) = match store {
            Ok(s) => {
                let rows = s.chat_rows().unwrap_or_default();
                let names = NameBook::from_rows(s.contact_names().unwrap_or_default());
                let mut list = ChatList::from_rows(rows, names);
                let (defs, members) = s.load_lists().unwrap_or_default();
                list.load_lists(defs, members);
                (s, list)
            }
            Err(e) => {
                // In-memory SQLite only fails on allocation failure.
                eprintln!("ferrochat: cannot open any cache: {e}");
                std::process::exit(1);
            }
        };
        let own_jid = store.kv("own_jid").ok().flatten();
        let recent_emoji = store
            .kv(emoji::RECENT_KEY)
            .ok()
            .flatten()
            .map(|j| emoji::parse_recent(&j))
            .unwrap_or_default();
        let _ = store.prune_expired(now_secs());
        let mut lid_map = LidMap::default();
        let mut chats = chats;
        for (lid, pn) in store.lid_pairs().unwrap_or_default() {
            lid_map.learn(&lid, &pn);
            chats.names.link(&lid, &pn);
        }
        let secrets: Arc<dyn SecretStore> = if cfg.mock {
            Arc::new(MemoryStore::new())
        } else {
            Arc::new(KeyringStore)
        };
        let auth = if !cfg.mock && cfg.signal_db().exists() {
            AuthState::Restoring
        } else {
            AuthState::WaitingForQr
        };
        let avatar_dir = (!cfg.mock).then(|| cfg.cache_dir.join("avatars"));
        let settings = if cfg.mock {
            crate::settings::Settings::default()
        } else {
            crate::settings::Settings::load(&cfg.config_dir)
        };
        // `--minimized` or the saved "start minimized" preference: no window.
        let mut cfg = cfg;
        if settings.start_minimized && cfg.screenshot.is_none() && !cfg.mock {
            cfg.minimized = true;
        }
        let autostart = !cfg.mock && crate::autostart::is_enabled();
        let window = App::initial_mode(&cfg, settings.close_to_tray);
        let geom = WindowGeom::initial(cfg.screenshot.is_some());
        let updater = App::new_updater(&cfg);
        let app = Self {
            theme: FerroTheme::new(!cfg.light_theme),
            cfg,
            store,
            secrets,
            auth,
            connection: Connection::Connecting,
            client: None,
            own_jid,
            net_generation: 0,
            chats,
            search: String::new(),
            filter: Filter::All,
            archived_view: false,
            calls: crate::call::Calls::default(),
            call_media: call::CallMedia::default(),
            video: None,
            video_session: 0,
            video_fullscreen: false,
            resync: Resync::default(),
            sidebar_menu: None,
            list_dialog: None,
            chat_dialog: None,
            selection: None,
            forward: None,
            open: None,
            pending_read: 0,
            wire: HashMap::new(),
            composer: composer::Composer::new(),
            drafts: composer::Drafts::default(),
            emoji_open: false,
            emoji: emoji::Picker {
                recent: recent_emoji,
                ..Default::default()
            },
            attach: None,
            drop: drop::DropState::default(),
            fresh_media: Default::default(),
            msg_info: None,
            menu_open: false,
            qr: None,
            images: ByteLru::new(IMAGE_BUDGET),
            thumbs: HashMap::new(),
            posters_tried: HashSet::new(),
            poster_busy: false,
            downloading: HashSet::new(),
            player: None,
            voice: None,
            voice_cache: ByteLru::new(16 * 1024 * 1024),
            recorder: None,
            recording_since: None,
            toast: None,
            focused: true,
            viewer: None,
            info: None,
            sync_progress: None,
            started,
            first_frame_logged: false,
            lid_map,
            session_unreadable: false,
            download_queue: VecDeque::new(),
            offline_attempt: 0,
            hovered: None,
            popup: None,
            popup_pos: None,
            reply_to: None,
            editing: None,
            dialog: None,
            highlight: None,
            typing: live::TypingTracker::default(),
            compose: live::ComposeThrottle::default(),
            presence: live::PresenceBook::default(),
            subscribed: HashSet::new(),
            avatars: ByteLru::new(AVATAR_BUDGET),
            avatar_none: HashSet::new(),
            avatar_inflight: HashSet::new(),
            avatar_queue: VecDeque::new(),
            avatar_dir,
            sidebar_rows: (0, 14),
            group_sizes: HashMap::new(),
            pending_inserts: 0,
            deferred: Vec::new(),
            message_hits: Vec::new(),
            search_generation: 0,
            chat_search: None,
            scene_started: false,
            nav_scene_done: false,
            scroll_y: 0.0,
            nav: nav::NavState::default(),
            status: status::StatusState::default(),
            settings,
            autostart,
            tray_checked: false,
            settings_open: false,
            window,
            window_id: None,
            geom,
            tray_sent: None,
            updater,
        };
        let mut app = app;
        app.focused = !app.cfg.minimized;
        if cache_fallback {
            app.toast(
                "The message cache could not be opened; this session keeps history in memory only.",
            );
        }
        let mut tasks = vec![Task::done(Message::FirstFrame)];
        tasks.push(app.load_statuses_task());
        tasks.push(app.load_star_keys_task());
        if app.window.open {
            tasks.push(app.open_window());
        }
        if app.cfg.screenshot.is_none() {
            // A tray icon, if the desktop has a panel for it. The answer
            // decides whether closing the window may hide it.
            let model = app.tray_model();
            tasks.push(Task::perform(crate::tray::start(model), |up| {
                Message::External(crate::ext::External::TrayUp(up))
            }));
        }
        if matches!(
            app.cfg.screenshot,
            Some((
                ScreenshotTarget::Settings | ScreenshotTarget::UpdateSettings,
                _
            ))
        ) {
            app.settings_open = true;
        }
        tasks.push(app.updates_boot());
        if app.cfg.screenshot.is_some() {
            tasks.push(Task::perform(
                tokio::time::sleep(Duration::from_millis(match app.cfg.screenshot {
                    Some((ScreenshotTarget::Video, _)) => 9000,
                    // Encodes a photo after the history loads.
                    Some((ScreenshotTarget::Paste, _)) => 6000,
                    // The helper probes / converts the file.
                    Some((ScreenshotTarget::AttachVideo | ScreenshotTarget::AttachGif, _)) => 9000,
                    Some((
                        ScreenshotTarget::Emoji
                        | ScreenshotTarget::Viewer
                        | ScreenshotTarget::Info
                        | ScreenshotTarget::GroupInfo
                        | ScreenshotTarget::MessageInfo
                        | ScreenshotTarget::GroupMessageInfo,
                        _,
                    )) => 5000,
                    // Call bubbles are stored, then the call rings.
                    Some((ScreenshotTarget::Call | ScreenshotTarget::CallActive, _)) => 6000,
                    // Scenes that wait for statuses, stars or communities.
                    Some((t, _)) if t.is_nav() => 6000,
                    _ => 2500,
                })),
                |_| Message::CaptureScreenshot,
            ));
        }
        (app, Task::batch(tasks))
    }

    /// Wrapper for `iced::application`'s title.
    pub fn title(&self, _window: iced::window::Id) -> String {
        let unread: u32 = self.chats.entries.iter().map(|e| e.meta.unread).sum();
        if unread > 0 {
            format!("({unread}) Ferrochat")
        } else {
            "Ferrochat".into()
        }
    }

    /// iced theme.
    pub fn iced_theme(&self, _window: iced::window::Id) -> iced::Theme {
        self.theme.iced()
    }

    /// Long-lived streams.
    pub fn subscription(&self) -> Subscription<Message> {
        let key = NetKey {
            cfg: self.cfg.clone(),
            generation: self.net_generation,
            secrets: self.secrets.clone(),
        };
        let mut subs = vec![
            Subscription::run_with(key, net_stream),
            iced::window::close_requests().map(Message::CloseRequested),
            iced::window::close_events().map(Message::WindowClosed),
            Subscription::run(crate::ext::stream).map(Message::External),
            iced::event::listen_with(|event, status, id| match event {
                iced::Event::Mouse(iced::mouse::Event::CursorMoved { position }) => {
                    input::set(position);
                    None
                }
                iced::Event::Window(iced::window::Event::Focused) => Some(Message::Focus(true)),
                iced::Event::Window(iced::window::Event::Unfocused) => Some(Message::Focus(false)),
                iced::Event::Window(iced::window::Event::Resized(size)) => {
                    Some(Message::WindowResized(id, size))
                }
                iced::Event::Window(iced::window::Event::Moved(p)) => {
                    Some(Message::WindowMoved(id, p))
                }
                // Drag and drop (X11 / XWayland, Windows, macOS).
                iced::Event::Window(iced::window::Event::FileHovered(_)) => {
                    Some(Message::Ui(Ui::Drop(drop::DropMsg::Hovered)))
                }
                iced::Event::Window(iced::window::Event::FilesHoveredLeft) => {
                    Some(Message::Ui(Ui::Drop(drop::DropMsg::Left)))
                }
                iced::Event::Window(iced::window::Event::FileDropped(path)) => {
                    Some(Message::Ui(Ui::Drop(drop::DropMsg::Dropped(path))))
                }
                iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
                    key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
                    ..
                }) => Some(Message::Ui(Ui::CloseChat)),
                // Paste: the composer's text input pastes text itself; this
                // only looks for a copied image or files (see `app::paste`).
                iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
                    key,
                    physical_key,
                    modifiers,
                    ..
                }) if paste::is_paste_shortcut(&key, physical_key, modifiers) => {
                    Some(Message::Ui(Ui::Paste))
                }
                // Space pauses the video viewer, unless a text field took it.
                iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
                    key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Space),
                    ..
                }) if status == iced::event::Status::Ignored => Some(Message::Ui(Ui::VideoToggle)),
                // Status viewer: arrows step through the updates.
                iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
                    key: iced::keyboard::Key::Named(iced::keyboard::key::Named::ArrowRight),
                    ..
                }) if status == iced::event::Status::Ignored => Some(Message::Ui(Ui::Arrow(1))),
                iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
                    key: iced::keyboard::Key::Named(iced::keyboard::key::Named::ArrowLeft),
                    ..
                }) if status == iced::event::Status::Ignored => Some(Message::Ui(Ui::Arrow(-1))),
                // Image viewer zoom keys (no text field has them then).
                iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
                    key: iced::keyboard::Key::Character(c),
                    modifiers,
                    ..
                }) if status == iced::event::Status::Ignored && !modifiers.control() => {
                    match c.as_str() {
                        "+" | "=" => Some(Message::Ui(Ui::ViewerZoom {
                            factor: viewer::ZOOM_STEP,
                            at: iced::Vector::ZERO,
                        })),
                        "-" => Some(Message::Ui(Ui::ViewerZoom {
                            factor: 1.0 / viewer::ZOOM_STEP,
                            at: iced::Vector::ZERO,
                        })),
                        "0" => Some(Message::Ui(Ui::ViewerResetZoom)),
                        _ => None,
                    }
                }
                _ => None,
            }),
        ];
        // Only once the helper runs: the stream takes the receiver on start.
        if let Some(v) = self
            .video
            .as_ref()
            .filter(|v| v.phase != video::VideoPhase::Downloading)
        {
            subs.push(Subscription::run_with(
                video::VideoKey {
                    session: v.session,
                    rx: v.rx.clone(),
                },
                video::video_stream,
            ));
        }
        if self.calls.busy() {
            subs.push(
                iced::time::every(Duration::from_secs(1))
                    .map(|_| Message::Ui(Ui::Call(call::CallUi::Tick))),
            );
        }
        if self.status.viewer.is_some() {
            subs.push(
                iced::time::every(Duration::from_millis(status::TICK_MS))
                    .map(|_| Message::Ui(Ui::Nav(nav::NavMsg::StatusTick))),
            );
        }
        let playing = self.voice.as_ref().is_some_and(|v| !v.paused);
        if playing || self.recording_since.is_some() {
            subs.push(iced::time::every(Duration::from_millis(100)).map(|_| Message::Tick));
        }
        if self.typing.is_active() || self.compose.is_active() || self.highlight.is_some() {
            subs.push(iced::time::every(Duration::from_secs(1)).map(|_| Message::LiveTick));
        }
        Subscription::batch(subs)
    }

    /// The only mutation point. Pushes the tray model afterwards.
    pub fn update(&mut self, message: Message) -> Task<Message> {
        self.apply_fresh_media();
        let task = self.update_inner(message);
        self.sync_tray();
        task
    }

    fn update_inner(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::External(ev) => self.on_external(ev),
            Message::CloseRequested(id) => self.on_close_requested(id),
            Message::WindowClosed(id) => self.on_window_closed(id),
            Message::WindowResized(id, size) => {
                self.on_window_resized(id, size);
                Task::none()
            }
            Message::WindowMoved(id, p) => {
                self.on_window_moved(id, p);
                Task::none()
            }
            Message::Quit => self.quit(),
            Message::Update(m) => self.on_update(m),
            Message::Video(m) => {
                self.on_video_event(*m);
                self.status_after_video()
            }
            Message::Ui(ui) => self.on_ui(ui),
            Message::Net(ev) => self.on_net(ev),
            Message::Done(done) => self.on_done(*done),
            Message::Tick => self.on_tick(),
            Message::LiveTick => self.on_live_tick(),
            Message::Focus(f) => {
                self.focused = f;
                if f {
                    // Unread messages of the open chat become read when the
                    // window comes back (if the newest ones are on screen).
                    self.send_pending_reads()
                } else {
                    Task::none()
                }
            }
            Message::FirstFrame => {
                if !self.first_frame_logged {
                    self.first_frame_logged = true;
                    let ms = self.started.elapsed().as_secs_f64() * 1000.0;
                    if self.cfg.trace_startup {
                        tracing::info!(first_frame_ms = format!("{ms:.1}"), "startup");
                        eprintln!("first_frame_ms={ms:.1}");
                    }
                }
                Task::none()
            }
            Message::CaptureScreenshot => iced::window::latest()
                .and_then(iced::window::screenshot)
                .map(|s| Message::done(Done::Screenshot(Box::new(s)))),
        }
    }

    fn toast(&mut self, text: impl Into<String>) {
        self.toast = Some(text.into());
    }

    // ---------------------------------------------------------------- UI

    fn on_ui(&mut self, ui: Ui) -> Task<Message> {
        if let Ui::Nav(m) = ui {
            return self.on_nav(m);
        }
        if let Ui::Call(c) = ui {
            return self.on_call_ui(c);
        }
        let ui = match self.on_ui_forward(ui) {
            Ok(task) => return task,
            Err(ui) => ui,
        };
        let ui = match self.on_ui_msg_info(ui) {
            Ok(task) => return task,
            Err(ui) => ui,
        };
        let ui = match self.on_ui_drop(ui) {
            Ok(task) => return task,
            Err(ui) => ui,
        };
        let ui = match self.on_ui_extra(ui) {
            Ok(task) => return task,
            Err(ui) => ui,
        };
        match ui {
            Ui::Search(q) => {
                self.search = q;
                return Task::batch([self.run_list_search(), self.want_visible_avatars()]);
            }
            Ui::Filter(f) => {
                self.filter = f;
                return self.want_visible_avatars();
            }
            Ui::OpenChat(chat) => return self.open_chat(chat),
            Ui::Arrow(d) => {
                if self.viewer.is_some() {
                    return self.update_inner(Message::Ui(Ui::ViewerStep(d)));
                }
                return self.on_nav(if d > 0 {
                    nav::NavMsg::StatusNext
                } else {
                    nav::NavMsg::StatusPrev
                });
            }
            Ui::CloseChat => {
                // Esc closes the topmost layer first, the conversation last.
                let mut close_video = None;
                let consumed = self.nav_escape()
                    || std::mem::take(&mut self.settings_open)
                    || self.list_dialog.take().is_some()
                    || self.chat_dialog.take().is_some()
                    || self.forward.take().is_some()
                    || self.sidebar_menu.take().is_some()
                    || self.dialog.take().is_some()
                    || self.attach.take().is_some()
                    || self.popup.take().is_some()
                    || self.video.is_some() && {
                        close_video = Some(self.close_video());
                        true
                    }
                    || self.viewer.take().is_some()
                    || self.msg_info.take().is_some()
                    || self.info.take().is_some()
                    || self.chat_search.take().is_some()
                    || self.selection.take().is_some()
                    || self.reply_to.take().is_some()
                    || (self.editing.is_some() && {
                        self.cancel_edit();
                        true
                    });
                let mut task = Task::none();
                if !consumed && !self.emoji_open && !self.menu_open {
                    task = self.compose_stop();
                    self.stash_draft();
                    self.composer.clear();
                    self.open = None;
                    self.hovered = None;
                }
                self.emoji_open = false;
                self.menu_open = false;
                if let Some(t) = close_video {
                    return Task::batch([task, t]);
                }
                return task;
            }
            Ui::ComposerAction(action) => {
                if self.composer.perform(action) {
                    return self.compose_input();
                }
            }
            Ui::Send => {
                if self.editing.is_some() {
                    return self.send_edit();
                }
                return self.send_text();
            }
            Ui::Attach => {
                self.emoji_open = false;
                let Some(chat) = self.open.as_ref().map(|c| c.chat.clone()) else {
                    return Task::none();
                };
                return Task::perform(
                    async {
                        let files = rfd::AsyncFileDialog::new()
                            .set_title("Send photos, videos or documents")
                            .pick_files()
                            .await?;
                        let paths: Vec<PathBuf> =
                            files.iter().map(|f| f.path().to_path_buf()).collect();
                        paste::stage_paths(paths, false).await.map(Box::new)
                    },
                    move |staged| Message::done(Done::Staged { chat, staged }),
                );
            }
            Ui::Paste => return self.paste(),
            Ui::AttachCaption(c) => {
                if let Some(a) = &mut self.attach {
                    a.caption = c;
                }
            }
            Ui::AttachSend => return self.send_attachment(),
            Ui::AttachCancel => self.attach = None,
            Ui::ToggleEmoji => {
                self.emoji_open = !self.emoji_open;
                self.emoji.opened();
                if !self.emoji_open {
                    return operation::focus(COMPOSER_ID);
                }
            }
            Ui::Emoji(e) => {
                return self.pick_emoji(e);
            }
            Ui::EmojiSearch(q) => self.emoji.query = q,
            Ui::EmojiCategory(c) => return self.scroll_picker_to(c),
            Ui::StartRecording => {
                // Device I/O happens on the recorder's own thread.
                self.recorder = Some(Recorder::start());
                self.recording_since = Some(Instant::now());
                let stop = self.compose_stop();
                let state = match self.open.as_ref().map(|c| c.chat.clone()) {
                    Some(chat) => self.emit_chat_state(live::Emit {
                        chat,
                        state: ChatState::Recording,
                    }),
                    None => Task::none(),
                };
                return Task::batch([stop, state]);
            }
            Ui::StopRecording { send } => {
                self.recording_since = None;
                let paused = match self.open.as_ref().map(|c| c.chat.clone()) {
                    Some(chat) => self.emit_chat_state(live::Emit {
                        chat,
                        state: ChatState::Paused,
                    }),
                    None => Task::none(),
                };
                if let Some(rec) = self.recorder.take()
                    && send
                {
                    return Task::batch([
                        paused,
                        blocking(
                            move || {
                                let pcm = rec.finish();
                                if pcm.len() < audio::OPUS_RATE as usize / 2 {
                                    return Err("Voice note too short.".to_string());
                                }
                                let ogg =
                                    audio::encode_ogg_opus(&pcm).map_err(|e| e.to_string())?;
                                Ok(Box::new(OutgoingMedia {
                                    kind: OutgoingKind::Voice {
                                        seconds: (pcm.len() as u32 / audio::OPUS_RATE).max(1),
                                        waveform: audio::waveform(&pcm),
                                    },
                                    bytes: ogg,
                                    file_name: "voice-note.ogg".into(),
                                    mimetype: "audio/ogg; codecs=opus".into(),
                                    caption: None,
                                }))
                            },
                            |r| Message::done(Done::Prepared(Some(r))),
                        ),
                    ]);
                }
                return paused;
            }
            Ui::PlayVoice(id) => return self.play_voice(id),
            Ui::PauseVoice => {
                if let (Some(v), Some(p)) = (&mut self.voice, &self.player) {
                    p.pause();
                    v.paused = true;
                }
            }
            Ui::OpenDocument(id) => return self.open_document(&id),
            Ui::Scrolled {
                relative_y,
                offset_y,
            } => {
                // The card is placed in screen coordinates: scrolling closes
                // it (a layout change that keeps the offset does not).
                if (offset_y - self.scroll_y).abs() > 1.0 {
                    self.popup = None;
                }
                self.scroll_y = offset_y;
                if self.pending_read > 0 && self.reading_chat().is_some() {
                    // Scrolled back down to unread messages: they are read now.
                    return self.send_pending_reads();
                }
                if relative_y < 0.02
                    && self
                        .open
                        .as_ref()
                        .is_some_and(|c| c.has_newer && !c.loading)
                {
                    return self.load_newer_page();
                }
                if relative_y > 0.98
                    && let Some(c) = &mut self.open
                    && c.has_more
                    && !c.loading
                    && let Some((ts, id)) = c.oldest_cursor()
                {
                    c.loading = true;
                    let chat = c.chat.clone();
                    let s = self.store.clone();
                    return blocking(
                        move || {
                            s.load_messages(&chat, Some((ts, &id)), PAGE)
                                .map(|m| (chat, m))
                        },
                        |r| match r {
                            Ok((chat, msgs)) => Message::done(Done::Loaded {
                                has_more: msgs.len() == PAGE,
                                chat,
                                msgs,
                                older: true,
                            }),
                            Err(e) => Message::done(Done::Stored(Err(e.to_string()))),
                        },
                    );
                }
            }
            Ui::ReloadQr => {
                self.auth = std::mem::take(&mut self.auth).next(AuthEvent::ReloadQr);
                self.qr = None;
                self.session_unreadable = false;
                self.restart_client();
            }
            Ui::Logout => {
                self.menu_open = false;
                if let Some(c) = self.client.clone() {
                    return Task::perform(async move { c.logout().await }, |_| {
                        Message::done(Done::Nothing)
                    })
                    .chain(self.cleanup_session());
                }
                return self.cleanup_session();
            }
            Ui::ToggleMenu => self.menu_open = !self.menu_open,
            Ui::OpenSettings => {
                self.menu_open = false;
                self.settings_open = true;
            }
            Ui::CloseSettings => self.settings_open = false,
            Ui::Toggle(setting) => return self.toggle_setting(setting),
            Ui::DismissToast => self.toast = None,
            Ui::SimulateScan => {
                if let Some(c) = &self.client {
                    c.simulate_scan();
                }
            }
            Ui::Retry(id) => return self.retry(&id),
            Ui::Reconnect => {
                self.offline_attempt = 0;
                self.restart_client();
            }
            Ui::AutoReconnect { generation } => {
                // Ignore stale timers (the user already retried / relinked).
                if generation == self.net_generation
                    && matches!(self.connection, Connection::Offline { .. })
                {
                    self.restart_client();
                }
            }
            Ui::ResetSession => {
                // The user explicitly chose to discard an undecryptable session.
                self.session_unreadable = false;
                self.advance_auth(AuthEvent::ReloadQr);
                return self.cleanup_session();
            }
            // Everything else is handled by `on_ui_extra`.
            _ => {}
        }
        Task::none()
    }

    fn open_chat(&mut self, chat: String) -> Task<Message> {
        self.emoji_open = false;
        self.menu_open = false;
        self.sidebar_menu = None;
        if self.open.as_ref().is_some_and(|c| c.chat == chat) {
            return Task::none();
        }
        self.selection = None;
        let unread = self
            .chats
            .get(&chat)
            .map(|e| e.meta.unread as usize)
            .unwrap_or(0);
        // Read once it is on screen (focused, at the bottom): until then the
        // badge stays, like WhatsApp Desktop.
        self.pending_read = unread;
        let paused = self.compose_stop();
        self.stash_draft();
        self.info = None;
        self.msg_info = None;
        self.scroll_y = 0.0;
        let mut conv = Conversation::new(chat.clone(), Vec::new(), false);
        conv.unread = (unread > 0).then_some(state::UnreadDivider {
            first: None,
            count: unread,
        });
        self.open = Some(conv);
        match self.drafts.take(&chat) {
            Some(d) => self.composer.set_text(&d),
            None => self.composer.clear(),
        }
        self.attach = None;
        self.reply_to = None;
        self.editing = None;
        self.popup = None;
        self.dialog = None;
        self.hovered = None;
        self.chat_search = None;
        self.highlight = None;
        let presence = self.subscribe_presence(&chat);
        let avatars = self.want_visible_avatars();
        let s = self.store.clone();
        let c2 = chat.clone();
        let load = blocking(
            move || s.load_messages(&c2, None, PAGE).map(|m| (c2, m)),
            |r| match r {
                Ok((chat, msgs)) => Message::done(Done::Loaded {
                    has_more: msgs.len() == PAGE,
                    chat,
                    msgs,
                    older: false,
                }),
                Err(e) => Message::done(Done::Stored(Err(e.to_string()))),
            },
        );
        // The cached counter is cleared with the badge, once the messages are
        // on screen (`send_pending_reads`).
        let persist = Task::none();
        Task::batch([load, persist, paused, presence, avatars])
    }

    fn local_message(&self, chat: &str, id: String, content: Content) -> ChatMessage {
        ChatMessage {
            id,
            chat: chat.to_string(),
            sender: self.own_jid.clone().unwrap_or_default(),
            sender_name: None,
            from_me: true,
            timestamp: now_secs(),
            content,
            status: ReceiptStatus::Pending,
            expires_at: None,
            reply: None,
            edited: false,
            forwarded: None,
            mentions: Vec::new(),
            reactions: Vec::new(),
        }
    }

    fn add_local(&mut self, msg: ChatMessage) -> Task<Message> {
        self.chats
            .apply_messages(std::slice::from_ref(&msg), Some(&msg.chat), true, None);
        self.index_thumbs(std::slice::from_ref(&msg));
        if let Some(c) = &mut self.open {
            if c.has_newer {
                // Sending from the middle of old history: continue from the
                // newest end; older pages load from the cache on scroll.
                c.replace_window(Vec::new(), true, false);
            }
            c.merge(vec![msg.clone()]);
        }
        Task::batch([
            store_task(&self.store, move |s| s.insert_messages(&[msg]).map(|_| ())),
            operation::snap_to(CONVERSATION_SCROLL, operation::RelativeOffset::START),
        ])
    }

    fn send_text(&mut self) -> Task<Message> {
        let text = self.composer.text().trim().to_string();
        let Some(chat) = self.open.as_ref().map(|c| c.chat.clone()) else {
            return Task::none();
        };
        if text.is_empty() {
            return Task::none();
        }
        self.composer.clear();
        self.emoji_open = false;
        let reply = self.reply_to.take();
        let id = net::new_message_id();
        let mut local = self.local_message(&chat, id.clone(), Content::Text(text.clone()));
        local.reply = reply.clone();
        let t1 = self.add_local(local);
        let t2 = self.dispatch_text(chat, id, text, reply);
        let t3 = self.compose_stop();
        Task::batch([t1, t2, t3])
    }

    fn dispatch_text(
        &self,
        chat: String,
        id: String,
        text: String,
        reply: Option<Quote>,
    ) -> Task<Message> {
        let Some(client) = self.client.clone() else {
            return Task::done(Message::done(Done::Sent {
                chat,
                id,
                result: Err("not connected".into()),
            }));
        };
        Task::perform(
            async move {
                let r = client
                    .send_text(&chat, &id, &text, reply.as_ref())
                    .await
                    .map_err(|e| e.to_string());
                (chat, id, r)
            },
            |(chat, id, result)| Message::done(Done::Sent { chat, id, result }),
        )
    }

    fn send_media(&mut self, media: OutgoingMedia) -> Task<Message> {
        let Some(chat) = self.open.as_ref().map(|c| c.chat.clone()) else {
            return Task::none();
        };
        let id = net::new_message_id();
        let local = self.local_message(&chat, id.clone(), media.local_content(None));
        let t1 = self.add_local(local);
        let Some(client) = self.client.clone() else {
            return t1;
        };
        let t2 = Task::perform(
            async move {
                let r = client
                    .send_media(&chat, &id, media)
                    .await
                    .map_err(|e| e.to_string());
                (chat, id, r)
            },
            |(chat, id, result)| Message::done(Done::MediaSent { chat, id, result }),
        );
        Task::batch([t1, t2])
    }

    /// Ctrl+V: if the composer has focus and the clipboard holds an image or
    /// copied files (and no text), open the attachment preview. The text
    /// input pastes text by itself; the clipboard is read off the UI thread.
    fn paste(&mut self) -> Task<Message> {
        if self.attach.is_some() || self.editing.is_some() || self.recording_since.is_some() {
            return Task::none();
        }
        let Some(chat) = self.open.as_ref().map(|c| c.chat.clone()) else {
            return Task::none();
        };
        operation::is_focused(COMPOSER_ID).then(move |focused| {
            if !focused {
                return Task::none();
            }
            let chat = chat.clone();
            Task::perform(
                async {
                    let staged =
                        tokio::task::spawn_blocking(|| paste::stage(paste::read_clipboard()))
                            .await
                            .ok()
                            .flatten()?;
                    Some(Box::new(paste::finish_videos(staged).await))
                },
                move |staged| Message::done(Done::Staged { chat, staged }),
            )
        })
    }

    /// Picked or pasted media is ready: show the preview (the preview image
    /// handle is made here, never in `view`).
    fn on_staged(&mut self, chat: String, staged: Option<Box<paste::Staged>>) -> Task<Message> {
        let Some(staged) = staged else {
            return Task::none();
        };
        if self.open.as_ref().is_none_or(|c| c.chat != chat) {
            return Task::none();
        }
        let staged = *staged;
        if let Some(text) = paste::strip_pasted_paths(&self.composer.text(), &staged.pasted_paths) {
            self.composer.set_text(&text);
        }
        if let Some(first) = staged.problems.first() {
            let more = staged.problems.len() - 1;
            self.toast(if more > 0 {
                format!("{first} (and {more} more)")
            } else {
                first.clone()
            });
        }
        let Some(preview) = paste::AttachPreview::new(chat, staged) else {
            return Task::none();
        };
        self.emoji_open = false;
        self.popup = None;
        self.attach = Some(preview);
        operation::focus(ATTACH_CAPTION_ID)
    }

    /// Sends the previewed media through the normal upload path.
    fn send_attachment(&mut self) -> Task<Message> {
        let Some(preview) = self.attach.take() else {
            return Task::none();
        };
        if self.open.as_ref().is_none_or(|c| c.chat != preview.chat) {
            return Task::none();
        }
        let tasks: Vec<_> = preview
            .into_outgoing()
            .into_iter()
            .map(|m| self.send_media(m))
            .collect();
        Task::batch(tasks).chain(operation::focus(COMPOSER_ID))
    }

    fn retry(&mut self, id: &str) -> Task<Message> {
        let Some(m) = self.open.as_ref().and_then(|c| c.get(id)).cloned() else {
            return Task::none();
        };
        if m.status != ReceiptStatus::Failed {
            return Task::none();
        }
        match m.content {
            Content::Text(t) => {
                if let Some(c) = &mut self.open
                    && let Some(x) = c.messages.iter_mut().find(|x| x.id == m.id)
                {
                    x.status = ReceiptStatus::Pending;
                }
                self.dispatch_text(m.chat, m.id, t, m.reply)
            }
            _ => {
                self.toast("Media can't be retried; attach the file again.");
                Task::none()
            }
        }
    }

    pub fn find_message(&self, id: &str) -> Option<&ChatMessage> {
        self.open.as_ref().and_then(|c| c.get(id))
    }

    /// Queues or starts a download, at most [`crate::media::MAX_CONCURRENT_DOWNLOADS`]
    /// at a time.
    fn request_media(&mut self, id: &str, max_pixels: u64, urgent: bool) -> Task<Message> {
        let Some((mref, origin)) = self
            .find_message(id)
            .and_then(|m| m.content.media().map(|r| (r.clone(), self.media_origin(m))))
        else {
            return Task::none();
        };
        let key = mref.cache_key();
        if self.images.contains(&key) || self.downloading.contains(&key) {
            return Task::none();
        }
        if self.downloading.len() >= crate::media::MAX_CONCURRENT_DOWNLOADS {
            if !self.download_queue.iter().any(|q| q == id) {
                if urgent {
                    self.download_queue.push_front(id.to_string());
                } else {
                    self.download_queue.push_back(id.to_string());
                }
            }
            return Task::none();
        }
        let Some(client) = self.client.clone() else {
            return Task::none();
        };
        if mref.kind == net::MediaKind::Sticker && mref.file_length > MAX_STICKER_BYTES {
            // Not a sticker anyone sends: show the placeholder.
            return Task::none();
        }
        self.downloading.insert(key.clone());
        Task::perform(
            async move {
                let r = async {
                    let bytes = media_fetch::download(&client, &mref, Some(&origin)).await?;
                    if mref.kind == net::MediaKind::Sticker {
                        // WebP: decode the first frame off the UI thread.
                        let (w, h, px) = tokio::task::spawn_blocking(move || {
                            crate::media::image::decode_sticker(&bytes).map_err(|e| e.to_string())
                        })
                        .await
                        .map_err(|e| e.to_string())??;
                        return Ok(Fetched::Rgba {
                            width: w,
                            height: h,
                            pixels: Arc::new(px),
                        });
                    }
                    let bytes = Arc::new(bytes);
                    if mref.kind == net::MediaKind::Image {
                        // Header-only probe: refuse oversized images and
                        // decompression bombs before iced decodes them.
                        let probe = bytes.clone();
                        tokio::task::spawn_blocking(move || {
                            crate::media::image::check_image_bounds_max(&probe, max_pixels)
                                .map(|_| ())
                        })
                        .await
                        .map_err(|e| e.to_string())?
                        .map_err(|e| e.to_string())?;
                    }
                    Ok::<_, String>(Fetched::Bytes(bytes))
                }
                .await;
                (key, r)
            },
            |(key, result)| Message::done(Done::Downloaded { key, result }),
        )
    }

    /// Starts queued downloads while slots are free.
    fn pump_downloads(&mut self) -> Task<Message> {
        let mut tasks = Vec::new();
        while self.downloading.len() < crate::media::MAX_CONCURRENT_DOWNLOADS {
            let Some(id) = self.download_queue.pop_front() else {
                break;
            };
            let cap = if self.viewer.as_ref().is_some_and(|v| v.id == id) {
                crate::media::MAX_IMAGE_PIXELS
            } else {
                crate::media::MAX_AUTO_IMAGE_PIXELS
            };
            tasks.push(self.request_media(&id, cap, false));
        }
        Task::batch(tasks)
    }

    fn play_voice(&mut self, id: String) -> Task<Message> {
        if self.player.is_none() {
            // Opening the audio device blocks: do it off the UI thread and
            // come back here when it is ready.
            return blocking(
                || Player::spawn().map_err(|e| e.user_message()),
                move |result| Message::done(Done::PlayerReady { result, then: id }),
            );
        }
        if let (Some(v), Some(p)) = (&mut self.voice, &self.player)
            && v.id == id
            && v.paused
            && !v.loading
        {
            p.resume();
            v.paused = false;
            return Task::none();
        }
        if let Some(p) = &self.player {
            p.stop();
        }
        let Some(msg) = self.find_message(&id).cloned() else {
            return Task::none();
        };
        let Some(mref) = msg.content.media().cloned() else {
            return Task::none();
        };
        let origin = self.media_origin(&msg);
        let declared = match &msg.content {
            Content::Voice { seconds, .. } => Some(*seconds),
            _ => None,
        };
        let mut tasks = Vec::new();
        if !msg.from_me
            && let Some(client) = self.client.clone()
        {
            let chat = msg.chat.clone();
            let sender = net::is_group_jid(&chat).then(|| msg.sender.clone());
            let ids = vec![msg.id.clone()];
            tasks.push(Task::perform(
                async move { client.mark_played(&chat, sender.as_deref(), &ids).await },
                |_| Message::done(Done::Nothing),
            ));
        }
        let key = mref.cache_key();
        if let Some(pcm) = self.voice_cache.get(&key).cloned() {
            self.start_playback(id, pcm);
            return Task::batch(tasks);
        }
        self.voice = Some(VoiceState {
            id: id.clone(),
            loading: true,
            paused: false,
            position_ms: 0,
            duration_ms: 0,
        });
        let Some(client) = self.client.clone() else {
            return Task::batch(tasks);
        };
        tasks.push(Task::perform(
            async move {
                let r = async {
                    let bytes = media_fetch::download(&client, &mref, Some(&origin)).await?;
                    tokio::task::spawn_blocking(move || {
                        audio::decode_ogg_opus_bounded(&bytes, declared)
                    })
                    .await
                    .map_err(|e| e.to_string())?
                    .map_err(|e| e.to_string())
                }
                .await;
                (id, r.map(Arc::new))
            },
            |(id, result)| Message::done(Done::VoiceReady { id, result }),
        ));
        Task::batch(tasks)
    }

    fn start_playback(&mut self, id: String, pcm: Arc<Pcm>) {
        let duration_ms = pcm.duration_ms();
        if let Some(p) = &self.player {
            p.play((*pcm).clone(), Duration::ZERO);
        }
        self.voice = Some(VoiceState {
            id,
            loading: false,
            paused: false,
            position_ms: 0,
            duration_ms,
        });
    }

    fn on_tick(&mut self) -> Task<Message> {
        if let Some(rec) = &self.recorder {
            if let Some(err) = rec.error() {
                self.recorder = None;
                self.recording_since = None;
                self.toast(format!("Microphone: {err}"));
            } else if rec.is_full() {
                self.toast("Voice notes are limited to 30 minutes; sending.");
                return Task::done(Message::Ui(Ui::StopRecording { send: true }));
            }
        }
        if let (Some(v), Some(p)) = (&mut self.voice, &self.player)
            && !v.loading
        {
            v.position_ms = p.state().position_ms().min(v.duration_ms);
            if p.state().is_finished() {
                self.voice = None;
            }
        }
        Task::none()
    }

    fn open_document(&mut self, id: &str) -> Task<Message> {
        let Some(m) = self.find_message(id).cloned() else {
            return Task::none();
        };
        let origin = self.media_origin(&m);
        let Content::Document {
            file_name,
            media: Some(mref),
            ..
        } = m.content
        else {
            return Task::none();
        };
        let Some(client) = self.client.clone() else {
            return Task::none();
        };
        let dir = self.cfg.downloads_dir.clone();
        let safe_name = crate::notify::sanitize_file_name(&file_name);
        let dangerous = crate::notify::is_dangerous(&safe_name);
        // Executables, scripts and active content are saved, never launched.
        let open_after = self.cfg.screenshot.is_none() && !dangerous;
        self.toast("Downloading document…");
        Task::perform(
            async move {
                let bytes = media_fetch::download(&client, &mref, Some(&origin)).await?;
                tokio::task::spawn_blocking(move || {
                    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                    let path = unique_path(&dir, &safe_name);
                    std::fs::write(&path, &bytes).map_err(|e| e.to_string())?;
                    if open_after {
                        crate::notify::open_path(&path).map_err(|e| e.to_string())?;
                    }
                    Ok((path, dangerous))
                })
                .await
                .map_err(|e| e.to_string())?
            },
            |r| Message::done(Done::DocumentOpened(r)),
        )
    }

    fn cleanup_session(&mut self) -> Task<Message> {
        self.client = None;
        self.open = None;
        self.composer.clear();
        self.drafts = composer::Drafts::default();
        self.chats = ChatList::default();
        self.images.clear();
        self.thumbs.clear();
        self.avatars.clear();
        self.avatar_none.clear();
        self.reset_live_state();
        self.message_hits.clear();
        self.chat_search = None;
        self.reply_to = None;
        self.editing = None;
        self.dialog = None;
        self.popup = None;
        self.own_jid = None;
        self.reset_nav();
        if let Some(p) = &self.player {
            p.stop();
        }
        self.voice = None;
        let store = self.store.clone();
        let secrets = self.secrets.clone();
        let signal_db = self.cfg.signal_db();
        let mock = self.cfg.mock;
        let avatar_dir = self.avatar_dir.clone();
        blocking(
            move || {
                let mut problems = Vec::new();
                if let Err(e) = store.clear() {
                    problems.push(format!("cache: {e}"));
                }
                if let Some(dir) = avatar_dir {
                    // Profile pictures belong to the unlinked account.
                    let _ = std::fs::remove_dir_all(dir);
                }
                if !mock {
                    if let Err(e) = net::whatsapp::remove_db(&signal_db) {
                        problems.push(format!("session file: {e}"));
                    }
                    if let Err(e) = secrets.delete(SIGNAL_STORE_KEY) {
                        problems.push(format!("keyring: {e}"));
                    }
                }
                problems
            },
            |problems| Message::done(Done::CleanedUp(problems)),
        )
    }

    /// The chat is muted now (timed mutes end on their own).
    pub fn is_muted(&self, chat: &str) -> bool {
        let now = now_secs();
        self.chats.get(chat).is_some_and(|e| e.meta.is_muted(now))
    }

    /// The open chat, if the user is reading its newest messages.
    pub fn reading_chat(&self) -> Option<&str> {
        let c = self.open.as_ref()?;
        state::is_reading(self.focused, self.scroll_y, c.has_newer).then_some(c.chat.as_str())
    }

    /// Unread messages of the open chat are on screen: clear the badge
    /// (and its cached counter) and send read receipts the way the library
    /// does, addressed to the JIDs the messages came from.
    fn send_pending_reads(&mut self) -> Task<Message> {
        if self.pending_read == 0 || self.reading_chat().is_none() {
            return Task::none();
        }
        let Some(conv) = &self.open else {
            return Task::none();
        };
        let had_badge = self
            .chats
            .get(&conv.chat)
            .is_some_and(|e| e.meta.unread > 0);
        let chat_id = conv.chat.clone();
        // A chat marked unread by hand is "read" on the other devices too.
        let sync = if self.chats.mark_read(&chat_id) {
            self.sync_read_mark(chat_id.clone(), true, 1)
        } else {
            Task::none()
        };
        let clear = if had_badge {
            Task::batch([
                store_task(&self.store, move |s| s.set_unread(&chat_id, 0)),
                sync,
            ])
        } else {
            sync
        };
        let Some(conv) = &self.open else {
            return clear;
        };
        let incoming: Vec<ChatMessage> = conv
            .messages
            .iter()
            .filter(|m| !m.from_me)
            .rev()
            .take(self.pending_read)
            .cloned()
            .collect();
        self.pending_read = 0;
        let Some(client) = self.client.clone() else {
            return clear;
        };
        let chat = conv.chat.clone();
        let batches = state::read_receipt_batches(&chat, &incoming);
        let wire_chat = state::wire_jid(&self.wire, &chat).to_string();
        let mut tasks = vec![clear];
        for (sender, ids) in batches {
            let client = client.clone();
            let chat = wire_chat.clone();
            let sender = sender.map(|s| state::wire_jid(&self.wire, &s).to_string());
            tasks.push(Task::perform(
                async move { client.mark_read(&chat, sender.as_deref(), &ids).await },
                |_| Message::done(Done::Nothing),
            ));
        }
        Task::batch(tasks)
    }

    fn index_thumbs(&mut self, msgs: &[ChatMessage]) {
        for m in msgs {
            let thumb = match &m.content {
                Content::Image { thumbnail, .. } | Content::Video { thumbnail, .. } => {
                    thumbnail.as_ref()
                }
                Content::Link { preview, .. } => preview.thumbnail.as_ref(),
                _ => None,
            };
            if let Some(t) = thumb
                && !self.thumbs.contains_key(&m.id)
            {
                self.thumbs
                    .insert(m.id.clone(), image::Handle::from_bytes(t.clone()));
            }
        }
        // Bounded: thumbnails only for what is in memory.
        if self.thumbs.len() > 2000 {
            let keep: HashSet<String> = self
                .open
                .iter()
                .flat_map(|c| c.messages.iter().map(|m| m.id.clone()))
                .collect();
            self.thumbs.retain(|k, _| keep.contains(k));
        }
    }

    fn auto_download_images(&mut self) -> Task<Message> {
        let ids: Vec<String> = self
            .open
            .iter()
            .flat_map(|c| c.messages.iter().rev().take(30))
            .filter(|m| {
                matches!(
                    m.content,
                    Content::Image { media: Some(_), .. } | Content::Sticker { media: Some(_), .. }
                )
            })
            .map(|m| m.id.clone())
            .collect();
        let mut tasks: Vec<_> = ids
            .iter()
            .map(|id| self.request_media(id, crate::media::MAX_AUTO_IMAGE_PIXELS, false))
            .collect();
        tasks.push(self.want_poster());
        Task::batch(tasks)
    }

    // ---------------------------------------------------------------- Net

    fn advance_auth(&mut self, ev: AuthEvent) {
        self.auth = std::mem::take(&mut self.auth).next(ev);
    }

    fn on_net(&mut self, ev: Event) -> Task<Message> {
        tracing::debug!(event = ?ev, "net event");
        let ev = if matches!(ev, Event::Reaction { .. } | Event::Edited { .. }) {
            match self.defer_if_inserting(ev) {
                Ok(()) => return Task::none(),
                Err(ev) => ev,
            }
        } else {
            ev
        };
        let ev = match self.on_nav_event(ev) {
            Ok(task) => return task,
            Err(ev) => ev,
        };
        match ev {
            Event::Ready(h) => {
                self.client = Some(h);
                return self.want_visible_avatars();
            }
            Event::SessionFound => self.advance_auth(AuthEvent::SessionFound),
            Event::NoSession => self.advance_auth(AuthEvent::NoSession),
            Event::Qr {
                code,
                valid_for_secs,
            } => {
                self.qr = qr_handle(&code);
                self.advance_auth(AuthEvent::Qr {
                    code,
                    valid_for_secs,
                });
            }
            Event::QrExhausted => self.advance_auth(AuthEvent::QrExhausted),
            Event::PairSuccess { jid } => {
                self.own_jid = Some(jid.clone());
                self.qr = None;
                self.advance_auth(AuthEvent::PairSuccess { jid: jid.clone() });
                return store_task(&self.store, move |s| s.set_kv("own_jid", &jid));
            }
            Event::PairError(e) => self.advance_auth(AuthEvent::PairError(e)),
            Event::Connection(c) => {
                let retry = matches!(c, Connection::Offline { .. });
                self.connection = c;
                if retry {
                    return self.schedule_reconnect();
                }
            }
            Event::Connected { jid } => {
                self.connection = Connection::Connected;
                self.offline_attempt = 0;
                if let Some(j) = &jid {
                    self.own_jid = Some(j.clone());
                }
                self.advance_auth(AuthEvent::Connected { jid: jid.clone() });
                let resync = Task::batch([self.import_lid_pairs(), self.maybe_resync()]);
                if let Some(j) = jid {
                    return Task::batch([
                        store_task(&self.store, move |s| s.set_kv("own_jid", &j)),
                        resync,
                    ]);
                }
                return resync;
            }
            Event::LoggedOut { reason } => {
                self.toast(reason.clone());
                self.advance_auth(AuthEvent::LoggedOut(reason));
                return self.cleanup_session();
            }
            Event::Messages { messages, origin } => return self.on_messages(messages, origin),
            Event::History {
                mut chats,
                names,
                progress,
            } => {
                self.sync_progress = progress.filter(|p| *p < 100);
                for c in &mut chats {
                    c.id = self.lid_map.normalize(&c.id);
                }
                self.chats.apply_history(&chats, &names);
                // A history chunk carries the phone's counter from when it was
                // built: it must not mark the chat being read as unread.
                if let Some(c) = self.reading_chat().map(str::to_string) {
                    self.chats.mark_read(&c);
                }
                let rows = crate::net::push_names(&names);
                let mut tasks = vec![
                    store_task(&self.store, move |s| {
                        s.upsert_chats(&chats)?;
                        s.set_contact_names(&rows)
                    }),
                    self.want_visible_avatars(),
                ];
                if let Some((t, _)) = self.cfg.screenshot
                    && t.list_only()
                    && !self.scene_started
                {
                    self.scene_started = true;
                    match t {
                        ScreenshotTarget::Archived => {
                            tasks.push(Task::done(Message::Ui(Ui::OpenArchived)));
                        }
                        ScreenshotTarget::Lists => {
                            tasks.push(Task::done(Message::Ui(Ui::Filter(Filter::List(
                                "1".into(),
                            )))));
                        }
                        ScreenshotTarget::ChatMenu => {
                            input::set(iced::Point::new(170.0, 250.0));
                            tasks.push(Task::done(Message::Ui(Ui::ChatMenu(
                                "34622222222@s.whatsapp.net".into(),
                            ))));
                        }
                        ScreenshotTarget::Mute => {
                            tasks.push(Task::done(Message::Ui(Ui::ChatAct(
                                chat_actions::ChatAct::AskMute("34622222222@s.whatsapp.net".into()),
                            ))));
                        }
                        _ => {}
                    }
                }
                if matches!(
                    self.cfg.screenshot,
                    Some((t, _)) if !t.list_only()
                ) && self.open.is_none()
                {
                    let chat = if matches!(
                        self.cfg.screenshot,
                        Some((
                            ScreenshotTarget::GroupInfo | ScreenshotTarget::GroupMessageInfo,
                            _
                        ))
                    ) {
                        "120363000000000001@g.us"
                    } else {
                        crate::mock::SHOWCASE_CHAT
                    };
                    tasks.push(Task::done(Message::Ui(Ui::OpenChat(chat.into()))));
                }
                tasks.extend(self.nav_scene(nav::SceneStage::History));
                return Task::batch(tasks);
            }
            Event::Receipt {
                chat,
                ids,
                status,
                participant,
                timestamp,
            } => {
                let chat = self.lid_map.normalize(&chat);
                let timed = self.record_receipt_times(
                    &chat,
                    &ids,
                    participant.as_deref(),
                    status,
                    timestamp,
                );
                if let Some(p) = participant
                    && net::is_group_jid(&chat)
                    && matches!(
                        status,
                        ReceiptStatus::Delivered | ReceiptStatus::Read | ReceiptStatus::Played
                    )
                {
                    return Task::batch([timed, self.group_receipt(chat, ids, status, p)]);
                }
                self.chats.apply_receipt(&chat, &ids, status);
                if let Some(c) = &mut self.open
                    && c.chat == chat
                {
                    c.apply_receipt(&ids, status);
                }
                return Task::batch([
                    timed,
                    store_task(&self.store, move |s| {
                        s.apply_receipt(&chat, &ids, status).map(|_| ())
                    }),
                ]);
            }
            Event::ContactName { jid, name, source } => {
                let jid = self.lid_map.normalize(&jid);
                if !self.chats.set_name(&jid, source, &name) {
                    return Task::none();
                }
                return store_task(&self.store, move |s| {
                    s.set_contact_names(&[(jid, source, name)])
                });
            }
            Event::ChatFlag { chat, flag } => {
                let chat = self.lid_map.normalize(&chat);
                self.chats.apply_flag(&chat, flag);
                return store_task(&self.store, move |s| s.set_chat_flag(&chat, flag));
            }
            Event::ListEdit { def, deleted } => {
                self.chats.apply_list_edit(def.clone(), deleted);
                if deleted {
                    if self.filter == Filter::List(def.id.clone()) {
                        self.filter = Filter::All;
                    }
                    return store_task(&self.store, move |s| s.delete_list(&def.id));
                }
                let saved = self.chats.list(&def.id).cloned().unwrap_or(def);
                return store_task(&self.store, move |s| s.save_list(&saved));
            }
            Event::ListChat { list, chat, member } => {
                let chat = self.lid_map.normalize(&chat);
                self.chats.apply_list_chat(&list, &chat, member);
                return store_task(&self.store, move |s| {
                    s.set_list_member(&list, &chat, member)
                });
            }
            Event::ChatMarkedUnread { chat } => {
                let chat = self.lid_map.normalize(&chat);
                self.chats.mark_unread(&chat);
                let n = self.chats.get(&chat).map(|e| e.meta.unread).unwrap_or(0);
                if n == 0 {
                    return Task::none();
                }
                return store_task(&self.store, move |s| s.set_unread(&chat, n));
            }
            Event::ChatRead { chat } => {
                let chat = self.lid_map.normalize(&chat);
                self.chats.mark_read(&chat);
                return store_task(&self.store, move |s| s.set_unread(&chat, 0));
            }
            Event::ChatCleared { chat, keep_starred } => {
                let chat = self.lid_map.normalize(&chat);
                return self.apply_clear(chat, keep_starred);
            }
            Event::ChatDeleted { chat } => {
                let chat = self.lid_map.normalize(&chat);
                return self.apply_delete(chat);
            }
            Event::Call(signal) => return self.on_call_signal(signal),
            Event::LidMappings(pairs) => {
                for (lid, pn) in &pairs {
                    if crate::net::is_lid_pair(lid, pn) {
                        self.chats.names.link(lid, pn);
                    }
                }
                let learned: Vec<_> = pairs
                    .into_iter()
                    .filter(|(lid, pn)| self.lid_map.learn(lid, pn))
                    .collect();
                if !learned.is_empty() {
                    return store_task(&self.store, move |s| s.set_lid_pairs(&learned));
                }
            }
            Event::Reaction {
                chat,
                target,
                reactor,
                emoji,
                ts,
            } => return self.on_reaction_event(chat, target, reactor, emoji, ts),
            Event::Edited {
                chat,
                id,
                sender,
                from_me,
                content,
            } => return self.on_edit_event(chat, id, sender, from_me, *content),
            Event::DeletedForMe { chat, id } => return self.on_deleted_for_me(chat, id),
            Event::ChatState {
                chat,
                participant,
                state,
            } => self.on_chat_state(chat, participant, state),
            Event::Presence {
                jid,
                online,
                last_seen,
            } => {
                let jid = self.lid_map.normalize(&jid);
                self.presence.update(&jid, online, last_seen, now_secs());
            }
            Event::PictureChanged { jid } => return self.on_picture_changed(jid),
            Event::SessionUnreadable(reason) => {
                self.advance_auth(AuthEvent::Fatal(reason));
                self.session_unreadable = true;
            }
            // Handled by `on_nav_event` before this match.
            Event::Status(_) | Event::StatusRevoked { .. } | Event::Star(_) => {}
            Event::Fatal(msg) => self.advance_auth(AuthEvent::Fatal(msg)),
        }
        Task::none()
    }

    /// Asks for a full app-state snapshot until one delivered, once per
    /// cache. Sessions linked by an older Ferrochat already hold the contact
    /// / archive / list collections at their latest version, so the
    /// mutations that carry the address-book names would never be replayed
    /// otherwise. A resync that did not deliver is retried
    /// ([`RESYNC_RETRY`], at most [`RESYNC_MAX_ATTEMPTS`] per run) and the
    /// marker is only written after a success.
    fn maybe_resync(&mut self) -> Task<Message> {
        if self.resync.done || self.resync.running {
            return Task::none();
        }
        let Some(client) = self.client.clone() else {
            return Task::none();
        };
        if self.store.kv(RESYNC_KEY).ok().flatten().as_deref() == Some(RESYNC_VERSION) {
            tracing::info!("app-state resync: already done for this cache");
            self.resync.done = true;
            return Task::none();
        }
        if self.resync.attempts >= RESYNC_MAX_ATTEMPTS {
            return Task::none();
        }
        self.resync.attempts += 1;
        self.resync.running = true;
        let attempt = self.resync.attempts;
        tracing::info!("app-state resync: requesting full snapshots (attempt {attempt})");
        Task::perform(
            async move { client.resync_app_state().await.map_err(|e| e.to_string()) },
            move |result| Message::done(Done::Resync { attempt, result }),
        )
    }

    fn on_resync(
        &mut self,
        attempt: u32,
        result: Result<net::ResyncReport, String>,
    ) -> Task<Message> {
        self.resync.running = false;
        let ok = match &result {
            Ok(r) => {
                let applied = r.collections.iter().filter(|c| c.applied()).count();
                tracing::info!(
                    "app-state resync attempt {attempt}: {applied}/{} collections applied, \
                     {} contact entries, sync key stored before: {}",
                    r.collections.len(),
                    r.contacts,
                    r.had_sync_key
                );
                r.succeeded(attempt, RESYNC_MAX_EMPTY)
            }
            Err(e) => {
                tracing::warn!("app-state resync attempt {attempt} failed: {e}");
                false
            }
        };
        self.log_name_stats("after app-state resync");
        if ok {
            self.resync.done = true;
            return store_task(&self.store, |s| s.set_kv(RESYNC_KEY, RESYNC_VERSION));
        }
        if self.resync.attempts >= RESYNC_MAX_ATTEMPTS {
            tracing::warn!(
                "app-state resync: no address book after {attempt} attempts; trying again at the \
                 next start"
            );
            return Task::none();
        }
        tracing::info!("app-state resync: retrying in {} s", RESYNC_RETRY.as_secs());
        Task::perform(async { tokio::time::sleep(RESYNC_RETRY).await }, |_| {
            Message::done(Done::ResyncDue)
        })
    }

    /// Info-level summary of where names come from. Counts only.
    fn log_name_stats(&self, when: &str) {
        let st = self.chats.name_stats();
        tracing::info!(
            "names {when}: saved={} business={} push={} lid_pairs={}; direct chats {} \
             (saved {}, business {}, push {}, chat record {}, number only {})",
            st.saved,
            st.business,
            st.push,
            self.lid_map.len(),
            st.direct_chats,
            st.chats_saved,
            st.chats_business,
            st.chats_push,
            st.chats_chat_name,
            st.chats_number
        );
    }

    /// Imports the LID ↔ phone pairs the protocol library learned (usync,
    /// device lists, earlier history syncs) into our map.
    fn import_lid_pairs(&self) -> Task<Message> {
        let Some(client) = self.client.clone() else {
            return Task::none();
        };
        Task::perform(
            async move { client.known_lid_pairs().await.unwrap_or_default() },
            |pairs| {
                if pairs.is_empty() {
                    Message::done(Done::Nothing)
                } else {
                    Message::Net(Event::LidMappings(pairs))
                }
            },
        )
    }

    /// Restarts the protocol client (new subscription generation).
    fn restart_client(&mut self) {
        self.reset_live_state();
        self.client = None;
        self.connection = Connection::Connecting;
        self.net_generation += 1;
    }

    /// App-level exponential backoff with jitter (1 s → 60 s) after the
    /// server refused the connection for a non-transient reason.
    fn schedule_reconnect(&mut self) -> Task<Message> {
        self.offline_attempt = self.offline_attempt.saturating_add(1);
        let delay = backoff_delay(self.offline_attempt, jitter_ms());
        let generation = self.net_generation;
        Task::perform(tokio::time::sleep(delay), move |_| {
            Message::Ui(Ui::AutoReconnect { generation })
        })
    }

    /// Phase 1: normalise addressing and persist; the store tells which
    /// messages are new (live or drained from the offline queue).
    fn on_messages(&mut self, mut messages: Vec<ChatMessage>, origin: Origin) -> Task<Message> {
        let own = self.own_jid.clone().unwrap_or_default();
        if self.wire.len() > 4096 {
            self.wire.clear();
        }
        for m in &mut messages {
            let (chat, sender) = (m.chat.clone(), m.sender.clone());
            self.lid_map.apply(m);
            if m.chat != chat {
                self.wire.insert(m.chat.clone(), chat);
            }
            if m.sender != sender {
                self.wire.insert(m.sender.clone(), sender);
            }
            if let Some(q) = &mut m.reply {
                q.sender = self.lid_map.normalize(&q.sender);
            }
            for r in &mut m.reactions {
                if r.reactor.is_empty() {
                    // History marks our own reactions without a JID.
                    r.reactor = own.clone();
                }
                r.reactor = self.lid_map.normalize(&r.reactor);
            }
        }
        self.pending_inserts += 1;
        let s = self.store.clone();
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    let new_ids = s.insert_messages(&messages).unwrap_or_else(|e| {
                        tracing::warn!(error = %e, "cache write failed");
                        // Without the cache we cannot dedupe: treat all as new.
                        messages.iter().map(|m| m.id.clone()).collect()
                    });
                    (messages, new_ids)
                })
                .await
                .ok()
            },
            move |r| {
                let (messages, new_ids) = r.unwrap_or_default();
                Message::done(Done::MessagesStored {
                    messages,
                    new_ids,
                    origin,
                })
            },
        )
    }

    /// Phase 2: reducers, unread counters, read receipts, notifications.
    fn apply_stored_messages(
        &mut self,
        messages: Vec<ChatMessage>,
        new_ids: Vec<String>,
        origin: Origin,
    ) -> Task<Message> {
        let open_chat = self.open.as_ref().map(|c| c.chat.clone());
        let fresh: HashSet<String> = match origin {
            Origin::History => HashSet::new(),
            Origin::Live | Origin::Offline => new_ids.into_iter().collect(),
        };
        let counts = origin != Origin::History;
        let pushed = self.chats.learn_push_names(&messages);
        // Only a chat the user is reading (focused, at the bottom) takes
        // new messages as read; otherwise they count, even in the open chat.
        let reading = self.reading_chat().map(str::to_string);
        self.chats.apply_messages(
            &messages,
            reading.as_deref(),
            true,
            counts.then_some(&fresh),
        );
        self.index_thumbs(&messages);
        let mut tasks = Vec::new();
        if !pushed.is_empty() {
            tasks.push(store_task(&self.store, move |s| {
                s.set_contact_names(&pushed)
            }));
        }
        if origin == Origin::Live {
            for m in messages.iter().filter(|m| !m.from_me) {
                // A message from someone ends their "typing…".
                self.typing.clear_sender(&m.chat, &m.sender);
            }
        }
        if let Some(c) = &mut self.open {
            let mine: Vec<_> = messages
                .iter()
                .filter(|m| m.chat == c.chat)
                // A window opened in old history does not grow at the new end.
                .filter(|_| !c.has_newer)
                .cloned()
                .collect();
            if !mine.is_empty() {
                let incoming = mine
                    .iter()
                    .filter(|m| !m.from_me && fresh.contains(&m.id))
                    .count();
                let first_new = mine
                    .iter()
                    .find(|m| !m.from_me && fresh.contains(&m.id))
                    .map(|m| m.id.clone());
                c.merge(mine);
                if counts && incoming > 0 {
                    if reading.is_none() {
                        // Not on screen: the divider marks where they start
                        // (a new run of unread messages moves it).
                        match &mut c.unread {
                            Some(d) if self.pending_read > 0 => d.count += incoming,
                            _ => {
                                c.unread = Some(state::UnreadDivider {
                                    first: first_new,
                                    count: incoming,
                                })
                            }
                        }
                    }
                    self.pending_read += incoming;
                }
                if origin == Origin::Live {
                    tasks.push(operation::snap_to(
                        CONVERSATION_SCROLL,
                        operation::RelativeOffset::START,
                    ));
                }
            }
        }
        tasks.push(self.auto_download_images());
        tasks.push(self.send_pending_reads());
        tasks.push(self.want_visible_avatars());
        if counts && self.cfg.screenshot.is_none() {
            let notify: Vec<&ChatMessage> = messages
                .iter()
                .filter(|m| fresh.contains(&m.id))
                .filter(|m| {
                    let muted = self.settings.muted || self.is_muted(&m.chat);
                    state::should_notify(m, open_chat.as_deref(), self.focused, muted)
                })
                .collect();
            let mode = self.cfg.notification_preview;
            let texts: Vec<(String, String, Option<String>)> = match origin {
                Origin::Live => notify
                    .iter()
                    .map(|m| {
                        let sender = self
                            .chats
                            .resolve(&m.sender, m.sender_name.as_deref())
                            .plain();
                        let group =
                            net::is_group_jid(&m.chat).then(|| self.chats.plain_name(&m.chat));
                        let (summary, body) = crate::notify::notification_text(
                            &sender,
                            group.as_deref(),
                            &self.preview(m),
                            mode,
                        );
                        (summary, body, Some(m.chat.clone()))
                    })
                    .collect(),
                // One summary for an offline drain instead of a burst.
                _ if !notify.is_empty() => {
                    let chats: HashSet<&str> = notify.iter().map(|m| m.chat.as_str()).collect();
                    let (summary, body) = crate::notify::summary_text(notify.len(), chats.len());
                    let only = (chats.len() == 1).then(|| notify[0].chat.clone());
                    vec![(summary, body, only)]
                }
                _ => Vec::new(),
            };
            for (summary, body, chat) in texts {
                tasks.push(blocking(
                    move || {
                        // Clicking it shows the window and opens that chat.
                        let on_click = chat.map(|chat| -> crate::notify::OnClick {
                            Box::new(move || {
                                crate::ext::send(crate::ext::External::Show { chat: Some(chat) })
                            })
                        });
                        crate::notify::show(&summary, &body, on_click)
                    },
                    |_| Message::done(Done::Nothing),
                ));
            }
        }
        Task::batch(tasks)
    }

    // ---------------------------------------------------------------- Done

    fn on_done(&mut self, done: Done) -> Task<Message> {
        match done {
            Done::Loaded {
                chat,
                msgs,
                older,
                has_more,
            } => {
                self.index_thumbs(&msgs);
                if let Some(c) = &mut self.open
                    && c.chat == chat
                {
                    if older {
                        c.prepend(msgs, has_more);
                    } else if c.has_newer {
                        // A jump already placed the window: do not splice
                        // the newest page next to it.
                    } else {
                        c.merge(msgs);
                        c.has_more = has_more;
                        if let Some(d) = &mut c.unread
                            && d.first.is_none()
                        {
                            d.first = state::first_unread(&c.messages, d.count);
                        }
                        let scene = self.scene_tasks();
                        return Task::batch([
                            operation::snap_to(
                                CONVERSATION_SCROLL,
                                operation::RelativeOffset::START,
                            ),
                            self.auto_download_images(),
                            self.send_pending_reads(),
                            Task::batch(scene),
                        ]);
                    }
                }
            }
            Done::Sent { chat, id, result } => {
                let status = match &result {
                    Ok(()) => ReceiptStatus::Sent,
                    Err(e) => {
                        tracing::warn!(error = %e, "send failed");
                        ReceiptStatus::Failed
                    }
                };
                return self.on_net(Event::Receipt {
                    chat,
                    ids: vec![id],
                    status,
                    participant: None,
                    timestamp: 0,
                });
            }
            Done::MediaSent { chat, id, result } => match result {
                Ok(content) => {
                    if let Some(c) = &mut self.open
                        && c.chat == chat
                    {
                        c.set_content(&id, content.clone());
                    }
                    let (c2, i2) = (chat.clone(), id.clone());
                    let t = store_task(&self.store, move |s| s.update_message(&c2, &i2, &content));
                    return Task::batch([
                        t,
                        self.on_net(Event::Receipt {
                            chat,
                            ids: vec![id],
                            status: ReceiptStatus::Sent,
                            participant: None,
                            timestamp: 0,
                        }),
                    ]);
                }
                Err(e) => {
                    self.toast(format!("Could not send media: {e}"));
                    return self.on_net(Event::Receipt {
                        chat,
                        ids: vec![id],
                        status: ReceiptStatus::Failed,
                        participant: None,
                        timestamp: 0,
                    });
                }
            },
            Done::Downloaded { key, result } => {
                self.downloading.remove(&key);
                match result {
                    Ok(Fetched::Bytes(bytes)) => {
                        let size = bytes.len();
                        // Hand the buffer to iced without copying when we hold
                        // the only reference.
                        let owned = Arc::try_unwrap(bytes).unwrap_or_else(|a| (*a).clone());
                        self.images.put(key, image::Handle::from_bytes(owned), size);
                    }
                    Ok(Fetched::Rgba {
                        width,
                        height,
                        pixels,
                    }) => {
                        let size = pixels.len();
                        let owned = Arc::try_unwrap(pixels).unwrap_or_else(|a| (*a).clone());
                        self.images
                            .put(key, image::Handle::from_rgba(width, height, owned), size);
                    }
                    Err(e) => tracing::warn!(error = %e, "media download failed"),
                }
                return self.pump_downloads();
            }
            Done::MessagesStored {
                messages,
                new_ids,
                origin,
            } => {
                self.pending_inserts = self.pending_inserts.saturating_sub(1);
                let applied = self.apply_stored_messages(messages, new_ids, origin);
                return Task::batch([applied, self.replay_deferred()]);
            }
            Done::Poster { chat, id, result } => return self.on_poster(chat, id, result),
            Done::ViewerImage { id, result } => self.on_viewer_image(id, result),
            Done::Info(d) => return self.on_info(*d),
            Done::MsgInfo { chat, id, rows } => self.on_msg_info(chat, id, rows),
            Done::ViewerSaved(r) => match r {
                Ok(Some(p)) => {
                    let name = p
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    self.toast(format!("Saved {name}"));
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "saving a photo failed");
                    self.toast("The photo could not be saved.");
                }
            },
            Done::PlayerReady { result, then } => match result {
                Ok(p) => {
                    self.player = Some(p);
                    return self.play_voice(then);
                }
                Err(e) => self.toast(e),
            },
            Done::VoiceReady { id, result } => match result {
                Ok(pcm) => {
                    if let Some(key) = self
                        .find_message(&id)
                        .and_then(|m| m.content.media())
                        .map(|m| m.cache_key())
                    {
                        let size = pcm.samples.len() * 4;
                        self.voice_cache.put(key, pcm.clone(), size);
                    }
                    if self.voice.as_ref().is_some_and(|v| v.id == id) {
                        self.start_playback(id, pcm);
                    }
                }
                Err(e) => {
                    self.voice = None;
                    self.toast(format!("Could not play voice note: {e}"));
                }
            },
            Done::Prepared(Some(Ok(media))) => return self.send_media(*media),
            Done::Prepared(Some(Err(e))) => self.toast(e),
            Done::Prepared(None) => {}
            Done::Staged { chat, staged } => return self.on_staged(chat, staged),
            Done::DocumentOpened(Ok((path, dangerous))) => {
                if dangerous {
                    self.toast(format!(
                        "Saved to {}. Not opened: this file type can run programs.",
                        path.display()
                    ));
                } else {
                    self.toast(format!("Saved to {}", path.display()));
                }
            }
            Done::DocumentOpened(Err(e)) => self.toast(format!("Could not open document: {e}")),
            Done::Avatar { jid, result } => return self.on_avatar(jid, result),
            Done::ReactionSent {
                chat,
                id,
                previous,
                result,
            } => return self.on_reaction_sent(chat, id, previous, result),
            Done::ActionDone { what, result } => {
                if what == "jump" {
                    if result.is_err() {
                        self.toast("That message is no longer in the local history.");
                    }
                } else {
                    self.on_action_done(what, result);
                }
            }
            Done::GroupStatus {
                chat,
                size,
                changed,
            } => self.on_group_status(chat, size, changed),
            Done::ListSearch { generation, hits } => {
                if generation == self.search_generation {
                    self.message_hits = hits;
                }
            }
            Done::ChatSearch {
                generation,
                chat,
                hits,
            } => return self.on_chat_search_results(generation, chat, hits),
            Done::Window {
                chat,
                anchor,
                window,
            } => return self.on_window(chat, anchor, window),
            Done::Newer {
                chat,
                msgs,
                has_newer,
            } => self.on_newer(chat, msgs, has_newer),
            Done::Located(loc) => return self.on_located(loc),
            Done::PopupAt { id, loc } => self.on_popup_at(id, loc),
            Done::LastMessage { chat, last } => self.chats.set_last(&chat, last.map(|b| *b)),
            Done::VideoFile { id, result } => self.on_video_file(id, result),
            Done::VideoOpened(Ok(_)) => self.toast("Opened in your video player."),
            Done::VideoOpened(Err(e)) => self.toast(format!("Could not open the video: {e}")),
            Done::Stored(Err(e)) => tracing::warn!(error = %e, "cache write failed"),
            Done::Autostart(Ok(on)) => self.autostart = on,
            Done::Autostart(Err(e)) => {
                tracing::warn!(error = %e, "autostart entry not changed");
                self.toast("Could not change the start-at-login setting.");
            }
            Done::FlagFailed { chat, undo, error } => {
                return self.on_flag_failed(chat, undo, error);
            }
            Done::MemberFailed {
                list,
                chat,
                undo,
                error,
            } => return self.on_member_failed(list, chat, undo, error),
            Done::Nav(d) => return self.on_nav_done(*d),
            Done::ChatAct(d) => return self.on_chat_act_done(d),
            Done::Call(d) => return self.on_call_done(d),
            Done::Stored(Ok(())) | Done::Nothing => {}
            Done::Resync { attempt, result } => return self.on_resync(attempt, result),
            Done::ResyncDue => return self.maybe_resync(),
            Done::CleanedUp(problems) => {
                if !problems.is_empty() {
                    tracing::warn!(?problems, "logout cleanup incomplete");
                    self.toast(format!(
                        "Logged out, but some local data could not be removed: {}",
                        problems.join("; ")
                    ));
                }
                self.restart_client();
                self.qr = None;
            }
            Done::Screenshot(shot) => {
                let Some((_, path)) = self.cfg.screenshot.clone() else {
                    return Task::none();
                };
                return blocking(
                    move || save_png(&path, &shot).map(|_| path),
                    |r| Message::done(Done::ScreenshotSaved(r)),
                );
            }
            Done::ScreenshotSaved(r) => {
                match r {
                    Ok(p) => eprintln!("screenshot written to {}", p.display()),
                    Err(e) => eprintln!("screenshot failed: {e}"),
                }
                return iced::exit();
            }
        }
        Task::none()
    }

    /// Screenshot mode: drives the UI into the state a scene shows, using
    /// the same messages a user's clicks would produce.
    fn scene_tasks(&mut self) -> Vec<Task<Message>> {
        let Some((target, _)) = self.cfg.screenshot.clone() else {
            return Vec::new();
        };
        if self.scene_started {
            return Vec::new();
        }
        self.scene_started = true;
        let Some(c) = &self.open else {
            return Vec::new();
        };
        let find = |needle: &str| {
            c.messages
                .iter()
                .find(|m| matches!(&m.content, Content::Text(t) if t.contains(needle)))
                .map(|m| m.id.clone())
        };
        let ui = |u: Ui| Task::done(Message::Ui(u));
        let mut v = Vec::new();
        let mut hover = None;
        match target {
            ScreenshotTarget::Reply => {
                if let (Some(a), Some(b)) = (find("Perfecto, ¡gracias!"), find("Qué pasada")) {
                    v.push(ui(Ui::StartReply(a)));
                    hover = Some(b.clone());
                    v.push(ui(Ui::ToggleMessageMenu(b)));
                }
            }
            ScreenshotTarget::Reactions => {
                if let Some(a) = find("Qué pasada") {
                    hover = Some(a.clone());
                    v.push(ui(Ui::ToggleReactionPicker(a)));
                }
            }
            ScreenshotTarget::ChatSearch => {
                v.push(ui(Ui::ToggleChatSearch));
                v.push(ui(Ui::ChatSearch("café".into())));
            }
            ScreenshotTarget::ListSearch => v.push(ui(Ui::Search("hola".into()))),
            ScreenshotTarget::Paste => {
                // What a pasted screenshot goes through, minus the clipboard.
                let staged = ::image::load_from_memory(&crate::mock::sunset_png(800, 500))
                    .ok()
                    .map(|i| {
                        let i = i.to_rgba8();
                        paste::Staged::from_image(&paste::RawImage {
                            width: i.width() as usize,
                            height: i.height() as usize,
                            rgba: i.into_raw(),
                        })
                    });
                v.push(Task::done(Message::done(Done::Staged {
                    chat: c.chat.clone(),
                    staged: staged.map(Box::new),
                })));
                v.push(ui(Ui::AttachCaption("Look at this sunset".into())));
            }
            ScreenshotTarget::AttachVideo | ScreenshotTarget::AttachGif => {
                // The real path of a picked file: staged, then probed (and
                // for a GIF converted) by the helper.
                let gif = target == ScreenshotTarget::AttachGif;
                let dir =
                    std::env::temp_dir().join(format!("ferrochat-shot-{}", std::process::id()));
                let chat = c.chat.clone();
                v.push(Task::perform(
                    async move {
                        let path = dir.join(if gif { "dancing.gif" } else { "trip.mp4" });
                        let bytes = if gif {
                            crate::mock::sample_gif()
                        } else {
                            include_bytes!("../../assets/mock-clip.mp4").to_vec()
                        };
                        tokio::fs::create_dir_all(&dir).await.ok()?;
                        tokio::fs::write(&path, bytes).await.ok()?;
                        let staged = paste::stage_paths(vec![path], false).await;
                        let _ = tokio::fs::remove_dir_all(&dir).await;
                        staged.map(Box::new)
                    },
                    move |staged| Message::done(Done::Staged { chat, staged }),
                ));
            }
            ScreenshotTarget::Drop => v.push(ui(Ui::Drop(drop::DropMsg::Hovered))),
            ScreenshotTarget::MessageInfo | ScreenshotTarget::GroupMessageInfo => {
                // Receipts as they would arrive, then the panel.
                let group = target == ScreenshotTarget::GroupMessageInfo;
                let needle = if group {
                    "Yo llevo las bebidas"
                } else {
                    "Llevo yo el café"
                };
                if let Some(m) = c.messages.iter().find(|m| {
                    m.from_me && matches!(&m.content, Content::Text(t) if t.contains(needle))
                }) {
                    let (chat, id, ts) = (c.chat.clone(), m.id.clone(), m.timestamp);
                    let receipt = |status, participant: Option<&str>, at: i64| {
                        Task::done(Message::Net(net::Event::Receipt {
                            chat: chat.clone(),
                            ids: vec![id.clone()],
                            status,
                            participant: participant.map(str::to_string),
                            timestamp: ts + at,
                        }))
                    };
                    let steps = if group {
                        vec![
                            receipt(
                                ReceiptStatus::Delivered,
                                Some("34677777777@s.whatsapp.net"),
                                4,
                            ),
                            receipt(ReceiptStatus::Read, Some("34677777777@s.whatsapp.net"), 95),
                            receipt(
                                ReceiptStatus::Delivered,
                                Some("34688888888@s.whatsapp.net"),
                                7,
                            ),
                            receipt(ReceiptStatus::Read, Some("34688888888@s.whatsapp.net"), 210),
                            receipt(
                                ReceiptStatus::Delivered,
                                Some("34699999999@s.whatsapp.net"),
                                40,
                            ),
                        ]
                    } else {
                        vec![
                            receipt(ReceiptStatus::Delivered, None, 3),
                            receipt(ReceiptStatus::Read, None, 75),
                        ]
                    };
                    let open = ui(Ui::MessageInfo(id.clone()));
                    v.push(
                        Task::batch(steps).chain(
                            Task::future(tokio::time::sleep(Duration::from_millis(500)))
                                .discard()
                                .chain(open),
                        ),
                    );
                }
            }
            ScreenshotTarget::Emoji => {
                v.push(ui(Ui::Emoji("\u{1F602}")));
                v.push(ui(Ui::Emoji("\u{2764}\u{FE0F}")));
                v.push(ui(Ui::Emoji("\u{1F1EA}\u{1F1F8}")));
                v.push(ui(Ui::ToggleEmoji));
            }
            ScreenshotTarget::Info | ScreenshotTarget::GroupInfo => v.push(ui(Ui::ToggleInfo)),
            ScreenshotTarget::Viewer => {
                if let Some(id) = c
                    .messages
                    .iter()
                    .rev()
                    .find(|m| matches!(m.content, Content::Image { .. }))
                    .map(|m| m.id.clone())
                {
                    v.push(ui(Ui::OpenImage(id)));
                }
            }
            ScreenshotTarget::Call => v.extend(call::scene(&c.chat, false)),
            ScreenshotTarget::CallActive => v.extend(call::scene(&c.chat, true)),
            ScreenshotTarget::Select | ScreenshotTarget::Forward => {
                if let (Some(a), Some(b)) = (find("Mira esto"), find("Llevo yo el café")) {
                    v.push(ui(Ui::Forward(forward::Fwd::Select(b))));
                    v.push(ui(Ui::Forward(forward::Fwd::Toggle(a))));
                }
                if target == ScreenshotTarget::Forward {
                    v.push(ui(Ui::Forward(forward::Fwd::Selected)));
                    for chat in ["34622222222@s.whatsapp.net", "120363000000000001@g.us"] {
                        v.push(ui(Ui::Forward(forward::Fwd::Target(chat.into()))));
                    }
                }
            }
            ScreenshotTarget::Delete => {
                if let Some(a) = find("Llevo yo el café") {
                    v.push(ui(Ui::AskDelete(a)));
                }
            }
            ScreenshotTarget::Video => {
                if let Some(id) = c
                    .messages
                    .iter()
                    .find(|m| matches!(m.content, Content::Video { .. }))
                    .map(|m| m.id.clone())
                {
                    v.push(ui(Ui::OpenVideo(id)));
                }
            }
            _ => {}
        }
        self.hovered = hover;
        v
    }

    /// Our own JIDs, for "@You" mentions.
    pub fn own_ids(&self) -> Vec<&str> {
        self.own_jid.as_deref().into_iter().collect()
    }

    /// One-line preview of a message, mentions shown as "@Name".
    pub fn preview(&self, m: &ChatMessage) -> String {
        self.chats.preview(m, &self.own_ids())
    }

    /// Logical size of the main window.
    pub fn window_size(&self) -> iced::Size {
        self.geom.size
    }

    /// Current playback / recording status (for tests).
    pub fn is_recording(&self) -> bool {
        self.recording_since.is_some()
    }
}

/// Exponential backoff: 1 s, 2 s, 4 s … capped at 60 s, plus up to 1 s jitter.
pub fn backoff_delay(attempt: u32, jitter_ms: u64) -> Duration {
    let base = 1u64 << attempt.saturating_sub(1).min(6);
    Duration::from_millis(base.min(60) * 1000 + jitter_ms % 1000)
}

fn jitter_ms() -> u64 {
    let mut b = [0u8; 2];
    let _ = getrandom::fill(&mut b);
    u16::from_le_bytes(b) as u64
}

pub(crate) fn unique_path(dir: &std::path::Path, name: &str) -> PathBuf {
    let candidate = dir.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) => (s.to_string(), format!(".{e}")),
        None => (name.to_string(), String::new()),
    };
    (1..1000)
        .map(|i| dir.join(format!("{stem} ({i}){ext}")))
        .find(|p| !p.exists())
        .unwrap_or(candidate)
}

fn save_png(path: &std::path::Path, shot: &iced::window::Screenshot) -> Result<(), String> {
    let (w, h) = (shot.size.width, shot.size.height);
    let img = ::image::RgbaImage::from_raw(w, h, shot.rgba.to_vec())
        .ok_or_else(|| "bad screenshot buffer".to_string())?;
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    img.save(path).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_caps() {
        assert_eq!(backoff_delay(1, 0), Duration::from_secs(1));
        assert_eq!(backoff_delay(2, 0), Duration::from_secs(2));
        assert_eq!(backoff_delay(4, 0), Duration::from_secs(8));
        assert_eq!(backoff_delay(30, 0), Duration::from_secs(60));
        assert!(backoff_delay(1, 999) < Duration::from_secs(2));
    }

    #[test]
    fn qr_handle_renders() {
        assert!(qr_handle("2@abc,def,ghi").is_some());
    }

    #[test]
    fn unique_paths_do_not_overwrite() {
        let dir = std::env::temp_dir().join(format!("ferrochat-unique-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.pdf"), b"x").unwrap();
        assert_eq!(unique_path(&dir, "a.pdf"), dir.join("a (1).pdf"));
        assert_eq!(unique_path(&dir, "b.pdf"), dir.join("b.pdf"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
