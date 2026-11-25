//! Lucide icons (ISC licence, see assets/icons/LICENSE), embedded with
//! `include_bytes!`. Generated from the files in `assets/icons/`.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use iced::widget::svg;

/// Every icon the UI uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Icon {
    /// `archive`
    Archive,
    /// `arrow-left`
    ArrowLeft,
    /// `bell-off`
    BellOff,
    /// `check-check`
    CheckCheck,
    /// `check`
    Check,
    /// `chevron-down`
    ChevronDown,
    /// `circle-alert`
    CircleAlert,
    /// `circle-dashed`
    CircleDashed,
    /// `clock`
    Clock,
    /// `download`
    Download,
    /// `ellipsis-vertical`
    EllipsisVertical,
    /// `file-text`
    FileText,
    /// `image`
    Image,
    /// `laptop`
    Laptop,
    /// `list-filter`
    ListFilter,
    /// `lock`
    Lock,
    /// `log-out`
    LogOut,
    /// `message-circle`
    MessageCircle,
    /// `message-square-plus`
    MessageSquarePlus,
    /// `mic`
    Mic,
    /// `paperclip`
    Paperclip,
    /// `pause`
    Pause,
    /// `phone`
    Phone,
    /// `pin`
    Pin,
    /// `play`
    Play,
    /// `plus`
    Plus,
    /// `refresh-cw`
    RefreshCw,
    /// `search`
    Search,
    /// `send-horizontal`
    SendHorizontal,
    /// `settings`
    Settings,
    /// `smile`
    Smile,
    /// `square`
    Square,
    /// `star`
    Star,
    /// `trash-2`
    Trash2,
    /// `user`
    User,
    /// `users-round`
    UsersRound,
    /// `users`
    Users,
    /// `video`
    Video,
    /// `wifi-off`
    WifiOff,
    /// `chevron-up`
    ChevronUp,
    /// `copy`
    Copy,
    /// `pencil`
    Pencil,
    /// `reply`
    Reply,
    /// `smile-plus`
    SmilePlus,
    /// `x`
    X,
    /// `external-link`
    ExternalLink,
    /// `maximize`
    Maximize,
    /// `minimize`
    Minimize,
    /// `rotate-ccw`
    RotateCcw,
    /// `volume-2`
    Volume2,
    /// `volume-x`
    VolumeX,
    /// `chevron-left`
    ChevronLeft,
    /// `chevron-right`
    ChevronRight,
    /// `camera`
    Camera,
    /// `type`
    Type,
    /// `palette`
    Palette,
    /// `star-off`
    StarOff,
    /// `user-plus`
    UserPlus,
    /// `zoom-in`
    ZoomIn,
    /// `zoom-out`
    ZoomOut,
    /// `ban`
    Ban,
    /// `phone-off`
    PhoneOff,
    /// `phone-missed`
    PhoneMissed,
    /// `phone-incoming`
    PhoneIncoming,
    /// `phone-outgoing`
    PhoneOutgoing,
    /// `mic-off`
    MicOff,
    /// `video-off`
    VideoOff,
    /// `forward`
    Forward,
    /// `info`
    Info,
}

impl Icon {
    fn bytes(self) -> &'static [u8] {
        match self {
            Icon::Archive => include_bytes!("../../assets/icons/archive.svg"),
            Icon::Info => include_bytes!("../../assets/icons/info.svg"),
            Icon::ZoomIn => include_bytes!("../../assets/icons/zoom-in.svg"),
            Icon::ZoomOut => include_bytes!("../../assets/icons/zoom-out.svg"),
            Icon::Ban => include_bytes!("../../assets/icons/ban.svg"),
            Icon::ArrowLeft => include_bytes!("../../assets/icons/arrow-left.svg"),
            Icon::BellOff => include_bytes!("../../assets/icons/bell-off.svg"),
            Icon::CheckCheck => include_bytes!("../../assets/icons/check-check.svg"),
            Icon::Check => include_bytes!("../../assets/icons/check.svg"),
            Icon::ChevronDown => include_bytes!("../../assets/icons/chevron-down.svg"),
            Icon::CircleAlert => include_bytes!("../../assets/icons/circle-alert.svg"),
            Icon::CircleDashed => include_bytes!("../../assets/icons/circle-dashed.svg"),
            Icon::Clock => include_bytes!("../../assets/icons/clock.svg"),
            Icon::Download => include_bytes!("../../assets/icons/download.svg"),
            Icon::EllipsisVertical => include_bytes!("../../assets/icons/ellipsis-vertical.svg"),
            Icon::FileText => include_bytes!("../../assets/icons/file-text.svg"),
            Icon::Image => include_bytes!("../../assets/icons/image.svg"),
            Icon::Laptop => include_bytes!("../../assets/icons/laptop.svg"),
            Icon::ListFilter => include_bytes!("../../assets/icons/list-filter.svg"),
            Icon::Lock => include_bytes!("../../assets/icons/lock.svg"),
            Icon::LogOut => include_bytes!("../../assets/icons/log-out.svg"),
            Icon::MessageCircle => include_bytes!("../../assets/icons/message-circle.svg"),
            Icon::MessageSquarePlus => include_bytes!("../../assets/icons/message-square-plus.svg"),
            Icon::Mic => include_bytes!("../../assets/icons/mic.svg"),
            Icon::Paperclip => include_bytes!("../../assets/icons/paperclip.svg"),
            Icon::Pause => include_bytes!("../../assets/icons/pause.svg"),
            Icon::Phone => include_bytes!("../../assets/icons/phone.svg"),
            Icon::Pin => include_bytes!("../../assets/icons/pin.svg"),
            Icon::Play => include_bytes!("../../assets/icons/play.svg"),
            Icon::Plus => include_bytes!("../../assets/icons/plus.svg"),
            Icon::RefreshCw => include_bytes!("../../assets/icons/refresh-cw.svg"),
            Icon::Search => include_bytes!("../../assets/icons/search.svg"),
            Icon::SendHorizontal => include_bytes!("../../assets/icons/send-horizontal.svg"),
            Icon::Settings => include_bytes!("../../assets/icons/settings.svg"),
            Icon::Smile => include_bytes!("../../assets/icons/smile.svg"),
            Icon::Square => include_bytes!("../../assets/icons/square.svg"),
            Icon::Star => include_bytes!("../../assets/icons/star.svg"),
            Icon::Trash2 => include_bytes!("../../assets/icons/trash-2.svg"),
            Icon::User => include_bytes!("../../assets/icons/user.svg"),
            Icon::UsersRound => include_bytes!("../../assets/icons/users-round.svg"),
            Icon::Users => include_bytes!("../../assets/icons/users.svg"),
            Icon::Video => include_bytes!("../../assets/icons/video.svg"),
            Icon::WifiOff => include_bytes!("../../assets/icons/wifi-off.svg"),
            Icon::ChevronUp => include_bytes!("../../assets/icons/chevron-up.svg"),
            Icon::Copy => include_bytes!("../../assets/icons/copy.svg"),
            Icon::Pencil => include_bytes!("../../assets/icons/pencil.svg"),
            Icon::Reply => include_bytes!("../../assets/icons/reply.svg"),
            Icon::SmilePlus => include_bytes!("../../assets/icons/smile-plus.svg"),
            Icon::X => include_bytes!("../../assets/icons/x.svg"),
            Icon::ExternalLink => include_bytes!("../../assets/icons/external-link.svg"),
            Icon::Maximize => include_bytes!("../../assets/icons/maximize.svg"),
            Icon::Minimize => include_bytes!("../../assets/icons/minimize.svg"),
            Icon::RotateCcw => include_bytes!("../../assets/icons/rotate-ccw.svg"),
            Icon::Volume2 => include_bytes!("../../assets/icons/volume-2.svg"),
            Icon::VolumeX => include_bytes!("../../assets/icons/volume-x.svg"),
            Icon::ChevronLeft => include_bytes!("../../assets/icons/chevron-left.svg"),
            Icon::ChevronRight => include_bytes!("../../assets/icons/chevron-right.svg"),
            Icon::Camera => include_bytes!("../../assets/icons/camera.svg"),
            Icon::Type => include_bytes!("../../assets/icons/type.svg"),
            Icon::Palette => include_bytes!("../../assets/icons/palette.svg"),
            Icon::StarOff => include_bytes!("../../assets/icons/star-off.svg"),
            Icon::UserPlus => include_bytes!("../../assets/icons/user-plus.svg"),
            Icon::PhoneOff => include_bytes!("../../assets/icons/phone-off.svg"),
            Icon::PhoneMissed => include_bytes!("../../assets/icons/phone-missed.svg"),
            Icon::PhoneIncoming => include_bytes!("../../assets/icons/phone-incoming.svg"),
            Icon::PhoneOutgoing => include_bytes!("../../assets/icons/phone-outgoing.svg"),
            Icon::MicOff => include_bytes!("../../assets/icons/mic-off.svg"),
            Icon::VideoOff => include_bytes!("../../assets/icons/video-off.svg"),
            Icon::Forward => include_bytes!("../../assets/icons/forward.svg"),
        }
    }

    /// The svg handle of this icon, created once and cloned afterwards
    /// (a handle built per frame would be re-hashed and, were its id not
    /// content-derived, re-rasterised on every redraw).
    pub fn handle(self) -> svg::Handle {
        static CACHE: OnceLock<Mutex<HashMap<Icon, svg::Handle>>> = OnceLock::new();
        let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
        match cache.lock() {
            Ok(mut m) => m
                .entry(self)
                .or_insert_with(|| svg::Handle::from_memory(self.bytes()))
                .clone(),
            Err(_) => svg::Handle::from_memory(self.bytes()),
        }
    }
}
