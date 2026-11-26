//! Paths, command-line flags and environment overrides.

use std::path::PathBuf;

/// Application id used for the keyring service, the Wayland `app_id` and
/// desktop notifications.
pub const APP_ID: &str = "dev.ferro.ferrochat";

/// Which screen `--screenshot` renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScreenshotTarget {
    /// QR pairing screen.
    Login,
    /// Chat list with no conversation open.
    ChatList,
    /// Chat list plus an open conversation with every bubble kind.
    Chat,
    /// Conversation with the reply bar and the message menu open.
    Reply,
    /// Conversation with the quick-reaction picker open.
    Reactions,
    /// Conversation with the in-chat search bar and its highlighted hit.
    ChatSearch,
    /// Chat list showing message search results.
    ListSearch,
    /// Conversation with the "delete message" dialog.
    Delete,
    /// Conversation in "Select messages" mode with two messages ticked.
    Select,
    /// The "Forward message to" dialog with two chats ticked.
    Forward,
    /// Settings page (background / tray toggles).
    Settings,
    /// Chat list showing the archived chats.
    Archived,
    /// Chat list with a custom list selected.
    Lists,
    /// Chat list with a chat's right-click menu open.
    ChatMenu,
    /// The "Mute notifications" duration dialog.
    Mute,
    /// The in-app video player.
    Video,
    /// Status tab: recent and viewed updates.
    StatusList,
    /// Status viewer on a photo status.
    StatusViewer,
    /// Communities tab.
    Communities,
    /// "New chat" panel.
    NewChat,
    /// "New group": picking participants.
    NewGroup,
    /// "New group": name and icon.
    NewGroupName,
    /// "Starred messages" panel.
    Starred,
    /// The chats header "⋮" menu.
    HeaderMenu,
    /// Conversation with the attachment preview of a pasted image.
    Paste,
    /// Attachment preview of a picked MP4 (probed by the helper).
    AttachVideo,
    /// Attachment preview of a picked GIF (converted by the helper).
    AttachGif,
    /// Files dragged over the open conversation.
    Drop,
    /// "Message info" of one of our messages in a direct chat.
    MessageInfo,
    /// "Message info" of one of our messages in a group.
    GroupMessageInfo,
    /// Conversation with the emoji drawer open.
    Emoji,
    /// The full-size image viewer.
    Viewer,
    /// The contact info drawer of a one-to-one chat.
    Info,
    /// The group info drawer.
    GroupInfo,
    /// Chat list with the "new version available" banner.
    UpdateBanner,
    /// Settings page with an update available (Updates section).
    UpdateSettings,
    /// Call bubbles in a chat and an incoming call ringing.
    Call,
    /// A call in progress.
    CallActive,
}

impl ScreenshotTarget {
    /// Screens drawn from the chat list alone (no conversation is opened).
    pub fn list_only(self) -> bool {
        matches!(
            self,
            Self::Login
                | Self::ChatList
                | Self::Archived
                | Self::Lists
                | Self::ChatMenu
                | Self::Mute
                | Self::StatusList
                | Self::StatusViewer
                | Self::Communities
                | Self::NewChat
                | Self::NewGroup
                | Self::NewGroupName
                | Self::Starred
                | Self::HeaderMenu
                | Self::UpdateBanner
        )
    }
}

impl ScreenshotTarget {
    /// Screens of the Status / Communities / panels family.
    pub fn is_nav(self) -> bool {
        matches!(
            self,
            Self::StatusList
                | Self::StatusViewer
                | Self::Communities
                | Self::NewChat
                | Self::NewGroup
                | Self::NewGroupName
                | Self::Starred
                | Self::HeaderMenu
        )
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "login" | "qr" => Some(Self::Login),
            "chats" | "chat-list" => Some(Self::ChatList),
            "chat" | "conversation" => Some(Self::Chat),
            "reply" | "menu" => Some(Self::Reply),
            "reactions" => Some(Self::Reactions),
            "chat-search" => Some(Self::ChatSearch),
            "list-search" => Some(Self::ListSearch),
            "delete" => Some(Self::Delete),
            "select" => Some(Self::Select),
            "forward" => Some(Self::Forward),
            "settings" => Some(Self::Settings),
            "archived" => Some(Self::Archived),
            "lists" => Some(Self::Lists),
            "chat-menu" => Some(Self::ChatMenu),
            "mute" => Some(Self::Mute),
            "video" => Some(Self::Video),
            "status" | "status-list" => Some(Self::StatusList),
            "status-viewer" => Some(Self::StatusViewer),
            "communities" => Some(Self::Communities),
            "new-chat" => Some(Self::NewChat),
            "new-group" => Some(Self::NewGroup),
            "new-group-name" => Some(Self::NewGroupName),
            "starred" => Some(Self::Starred),
            "header-menu" | "menu-header" => Some(Self::HeaderMenu),
            "paste" | "attach" => Some(Self::Paste),
            "attach-video" => Some(Self::AttachVideo),
            "attach-gif" => Some(Self::AttachGif),
            "drop" => Some(Self::Drop),
            "message-info" => Some(Self::MessageInfo),
            "group-message-info" => Some(Self::GroupMessageInfo),
            "emoji" | "emoji-picker" => Some(Self::Emoji),
            "viewer" | "image-viewer" => Some(Self::Viewer),
            "info" | "contact-info" => Some(Self::Info),
            "group-info" => Some(Self::GroupInfo),
            "update-banner" => Some(Self::UpdateBanner),
            "update-settings" => Some(Self::UpdateSettings),
            "call" | "call-incoming" => Some(Self::Call),
            "call-active" => Some(Self::CallActive),
            _ => None,
        }
    }
}

/// What the notification body shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NotificationPreview {
    /// Sender and message preview (official client default).
    SenderAndPreview,
    /// Only the sender name.
    SenderOnly,
}

/// Runtime configuration.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Config {
    /// `$XDG_DATA_HOME/ferrochat`: cache database and the encrypted Signal store.
    pub data_dir: PathBuf,
    /// `$XDG_STATE_HOME/ferrochat`: log file.
    pub state_dir: PathBuf,
    /// `$XDG_CONFIG_HOME/ferrochat`: `settings.json`.
    pub config_dir: PathBuf,
    /// `$XDG_CACHE_HOME/ferrochat`: profile pictures and videos opened in the
    /// system player. Disposable and size-bounded.
    pub cache_dir: PathBuf,
    /// Where documents are saved when opened.
    pub downloads_dir: PathBuf,
    /// Run against the in-process fake protocol with mock data.
    pub mock: bool,
    /// Start with no window, only the tray icon (`--minimized`).
    pub minimized: bool,
    /// Render one screen to a PNG and exit.
    pub screenshot: Option<(ScreenshotTarget, PathBuf)>,
    /// Light theme instead of the default dark one.
    pub light_theme: bool,
    /// Desktop notification privacy level.
    pub notification_preview: NotificationPreview,
    /// Log `first_frame_ms` at startup.
    pub trace_startup: bool,
}

/// Result of parsing the command line.
#[derive(Debug)]
pub enum Cli {
    /// Run the app with this configuration.
    Run(Box<Config>),
    /// Print help and exit.
    Help,
    /// Bad arguments.
    Invalid(String),
}

/// Usage text for `--help`.
pub const USAGE: &str = "\
ferrochat - unofficial native WhatsApp desktop client

USAGE:
    ferrochat [--mock] [--minimized] [--screenshot <login|chats|chat|reply|reactions|chat-search|list-search|delete|settings|archived|lists|chat-menu|video|status|status-viewer|communities|new-chat|new-group|new-group-name|starred|header-menu|paste|update-banner|update-settings> <out.png>]

OPTIONS:
    --mock                      Run with mock data and a fake protocol (no network)
    --minimized                 Start in the system tray without opening the window
    --screenshot <screen> <png> Render a screen (implies --mock), save it and exit
    -h, --help                  Show this help

ENVIRONMENT:
    RUST_LOG                    Log filter (default: info)
    FERRO_THEME=light|dark      Colour scheme (default: dark)
    FERRO_TRACE_STARTUP=1       Log first_frame_ms
    FERROCHAT_DATA_DIR          Override the data directory
    FERROCHAT_CONFIG_DIR        Override the settings directory
    FERROCHAT_CACHE_DIR         Override the cache directory (avatars, videos)
    FERROCHAT_NOTIFY=sender     Notifications show the sender only
    FERRO_RELEASE_API           Override the update check URL (testing)
    FERRO_UPDATE_FORCE=1        Check for updates even in debug/mock runs
";

impl Config {
    /// Parses `args` (without the program name) and the environment.
    pub fn from_args<I: IntoIterator<Item = String>>(args: I) -> Cli {
        let mut mock = false;
        let mut minimized = false;
        let mut screenshot = None;
        let mut it = args.into_iter();
        while let Some(arg) = it.next() {
            match arg.as_str() {
                "--mock" => mock = true,
                "--minimized" => minimized = true,
                "-h" | "--help" => return Cli::Help,
                "--screenshot" => {
                    let (Some(target), Some(out)) = (it.next(), it.next()) else {
                        return Cli::Invalid("--screenshot needs <screen> <out.png>".into());
                    };
                    let Some(target) = ScreenshotTarget::parse(&target) else {
                        return Cli::Invalid(format!("unknown screen '{target}'"));
                    };
                    screenshot = Some((target, PathBuf::from(out)));
                    mock = true;
                }
                other => return Cli::Invalid(format!("unknown argument '{other}'")),
            }
        }
        let mut cfg = Self::defaults();
        cfg.mock = mock;
        // A screenshot always needs its window.
        cfg.minimized = minimized && screenshot.is_none();
        cfg.screenshot = screenshot;
        Cli::Run(Box::new(cfg))
    }

    /// Default paths plus environment overrides.
    pub fn defaults() -> Self {
        let dirs = directories::ProjectDirs::from("dev", "ferro", "ferrochat");
        let data_dir = std::env::var_os("FERROCHAT_DATA_DIR")
            .map(PathBuf::from)
            .or_else(|| dirs.as_ref().map(|d| d.data_dir().to_path_buf()))
            .unwrap_or_else(|| PathBuf::from(".ferrochat"));
        let state_dir = dirs
            .as_ref()
            .and_then(|d| d.state_dir().map(|p| p.to_path_buf()))
            .unwrap_or_else(|| data_dir.join("state"));
        let config_dir = std::env::var_os("FERROCHAT_CONFIG_DIR")
            .map(PathBuf::from)
            .or_else(|| dirs.as_ref().map(|d| d.config_dir().to_path_buf()))
            .unwrap_or_else(|| data_dir.join("config"));
        let cache_dir = std::env::var_os("FERROCHAT_CACHE_DIR")
            .map(PathBuf::from)
            .or_else(|| dirs.as_ref().map(|d| d.cache_dir().to_path_buf()))
            .unwrap_or_else(|| data_dir.join("cache"));
        let downloads_dir = directories::UserDirs::new()
            .and_then(|u| u.download_dir().map(|p| p.to_path_buf()))
            .unwrap_or_else(std::env::temp_dir);
        Self {
            data_dir,
            state_dir,
            config_dir,
            cache_dir,
            downloads_dir,
            mock: false,
            minimized: false,
            screenshot: None,
            light_theme: std::env::var("FERRO_THEME").is_ok_and(|v| v == "light"),
            notification_preview: if std::env::var("FERROCHAT_NOTIFY").is_ok_and(|v| v == "sender")
            {
                NotificationPreview::SenderOnly
            } else {
                NotificationPreview::SenderAndPreview
            },
            trace_startup: std::env::var("FERRO_TRACE_STARTUP").is_ok_and(|v| v == "1"),
        }
    }

    /// The SQLite message cache.
    pub fn cache_db(&self) -> PathBuf {
        self.data_dir.join("cache.sqlite3")
    }

    /// The SQLCipher-encrypted Signal/session store used by the protocol library.
    pub fn signal_db(&self) -> PathBuf {
        self.data_dir.join("session.sqlcipher")
    }
}

/// Creates `dir` (and parents) and restricts it to the owner (0700). An
/// existing directory (e.g. a user-chosen `FERROCHAT_DATA_DIR`) keeps its
/// permissions: we only tighten what we create.
pub fn secure_dir(dir: &std::path::Path) -> std::io::Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Why the instance lock could not be taken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockError {
    /// Another Ferrochat holds the lock for this data directory.
    AlreadyRunning,
    /// Any other failure (permissions, I/O), already worded for the user.
    Other(String),
}

impl std::fmt::Display for LockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyRunning => f.write_str(
                "Ferrochat is already running for this data directory. Switch to the open window.",
            ),
            Self::Other(m) => f.write_str(m),
        }
    }
}

/// Takes an exclusive advisory lock on `<data_dir>/ferrochat.lock`. Keep the
/// returned file alive for the whole process; a second instance fails here
/// with [`LockError::AlreadyRunning`] and asks the first one to show its
/// window (see `instance`).
pub fn acquire_instance_lock(data_dir: &std::path::Path) -> Result<std::fs::File, LockError> {
    secure_dir(data_dir)
        .map_err(|e| LockError::Other(format!("cannot create {}: {e}", data_dir.display())))?;
    let path = data_dir.join("ferrochat.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|e| LockError::Other(format!("cannot open {}: {e}", path.display())))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => Err(LockError::AlreadyRunning),
        Err(std::fs::TryLockError::Error(e)) => Err(LockError::Other(format!(
            "cannot lock {}: {e}",
            path.display()
        ))),
    }
}

/// Restricts an SQLite database and its `-wal` / `-shm` / `-journal` side
/// files to the owner (0600). Missing files are ignored.
pub fn secure_db_files(db: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for suffix in ["", "-wal", "-shm", "-journal"] {
            let mut p = db.as_os_str().to_owned();
            p.push(suffix);
            let _ = std::fs::set_permissions(
                std::path::PathBuf::from(p),
                std::fs::Permissions::from_mode(0o600),
            );
        }
    }
    #[cfg(not(unix))]
    let _ = db;
}

/// Opens `<state_dir>/log`, rotating it to `log.1` when over 5 MB.
pub fn open_log_file(state_dir: &std::path::Path) -> Option<std::fs::File> {
    secure_dir(state_dir).ok()?;
    let path = state_dir.join("log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 5 * 1024 * 1024) {
        let _ = std::fs::rename(&path, state_dir.join("log.1"));
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_screenshot_and_implies_mock() {
        match Config::from_args(args(&["--screenshot", "chat", "/tmp/x.png"])) {
            Cli::Run(cfg) => {
                assert!(cfg.mock);
                assert_eq!(
                    cfg.screenshot,
                    Some((ScreenshotTarget::Chat, PathBuf::from("/tmp/x.png")))
                );
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn data_dir_and_db_files_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("ferrochat-perm-{}", std::process::id()));
        secure_dir(&dir).unwrap();
        let db = dir.join("x.sqlite3");
        std::fs::write(&db, b"x").unwrap();
        std::fs::write(dir.join("x.sqlite3-wal"), b"x").unwrap();
        secure_db_files(&db);
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&db), 0o600);
        assert_eq!(mode(&dir.join("x.sqlite3-wal")), 0o600);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn second_instance_is_refused() {
        let dir = std::env::temp_dir().join(format!("ferrochat-lock-{}", std::process::id()));
        let first = acquire_instance_lock(&dir).unwrap();
        assert_eq!(
            acquire_instance_lock(&dir).unwrap_err(),
            LockError::AlreadyRunning
        );
        drop(first);
        assert!(acquire_instance_lock(&dir).is_ok());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn existing_dirs_keep_their_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("ferrochat-keep-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        secure_dir(&dir).unwrap();
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn minimized_flag_and_settings_screenshot() {
        match Config::from_args(args(&["--minimized"])) {
            Cli::Run(cfg) => assert!(cfg.minimized && !cfg.mock),
            other => panic!("unexpected {other:?}"),
        }
        // Screenshots never start without a window.
        match Config::from_args(args(&["--minimized", "--screenshot", "settings", "x.png"])) {
            Cli::Run(cfg) => {
                assert!(!cfg.minimized);
                assert_eq!(
                    cfg.screenshot.map(|(t, _)| t),
                    Some(ScreenshotTarget::Settings)
                );
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn rejects_unknown() {
        assert!(matches!(
            Config::from_args(args(&["--nope"])),
            Cli::Invalid(_)
        ));
        assert!(matches!(
            Config::from_args(args(&["--screenshot", "foo", "x"])),
            Cli::Invalid(_)
        ));
        assert!(matches!(Config::from_args(args(&["-h"])), Cli::Help));
    }
}
