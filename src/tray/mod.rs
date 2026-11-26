//! System tray icon (StatusNotifierItem on Linux, the notification area on
//! Windows).
//!
//! The pure parts (what the tooltip says, which status, which menu entries
//! and what they do) live here and are unit-tested. The D-Bus service is in
//! `linux.rs`, the Win32 one in `windows.rs`; macOS gets a stub that reports
//! "no tray" (not implemented), so the app there closes to quit as before.

use std::sync::OnceLock;

use tokio::sync::watch;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(not(any(target_os = "linux", windows)))]
mod stub;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
pub use linux::start;
#[cfg(not(any(target_os = "linux", windows)))]
pub use stub::start;
#[cfg(windows)]
pub use windows::{shutdown, start};

/// Removes the tray icon at quit (only Windows needs it: the D-Bus service
/// goes away with the process).
#[cfg(not(windows))]
pub fn shutdown() {}

/// What a tray click or menu entry asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCmd {
    /// Show the window if hidden, hide it if shown (left click, Show/Hide).
    Toggle,
    /// Flip "mute notifications".
    ToggleMute,
    /// Flip "start minimized to tray".
    ToggleStartMinimized,
    /// Quit the app.
    Quit,
}

/// Everything the tray displays.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrayModel {
    /// Chats with unread messages (muted chats excluded).
    pub unread_chats: u32,
    /// The window is currently shown.
    pub window_visible: bool,
    /// Notifications are muted.
    pub muted: bool,
    /// Launch minimized to the tray.
    pub start_minimized: bool,
}

/// The tray's StatusNotifierItem status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attention {
    /// Normal icon.
    Idle,
    /// Unread chats: `NeedsAttention` with the attention icon.
    NeedsAttention,
}

/// One entry of the context menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuEntry {
    /// A plain entry.
    Item {
        /// Label.
        label: &'static str,
        /// Action.
        cmd: TrayCmd,
    },
    /// An entry with a check mark.
    Check {
        /// Label.
        label: &'static str,
        /// Check state.
        checked: bool,
        /// Action.
        cmd: TrayCmd,
    },
    /// A divider.
    Separator,
}

impl TrayModel {
    /// Tooltip title: "Ferrochat — 3 unread chats".
    pub fn tooltip(&self) -> String {
        match self.unread_chats {
            0 => "Ferrochat".to_string(),
            1 => "Ferrochat \u{2014} 1 unread chat".to_string(),
            n => format!("Ferrochat \u{2014} {n} unread chats"),
        }
    }

    /// Status: attention while any chat is unread.
    pub fn attention(&self) -> Attention {
        if self.unread_chats > 0 {
            Attention::NeedsAttention
        } else {
            Attention::Idle
        }
    }

    /// The context menu: Show/Hide, Mute notifications, Start minimized, Quit.
    pub fn menu(&self) -> Vec<MenuEntry> {
        vec![
            MenuEntry::Item {
                label: if self.window_visible {
                    "Hide Ferrochat"
                } else {
                    "Show Ferrochat"
                },
                cmd: TrayCmd::Toggle,
            },
            MenuEntry::Separator,
            MenuEntry::Check {
                label: "Mute notifications",
                checked: self.muted,
                cmd: TrayCmd::ToggleMute,
            },
            MenuEntry::Check {
                label: "Start minimized to tray",
                checked: self.start_minimized,
                cmd: TrayCmd::ToggleStartMinimized,
            },
            MenuEntry::Separator,
            MenuEntry::Item {
                label: "Quit",
                cmd: TrayCmd::Quit,
            },
        ]
    }
}

fn channel() -> &'static watch::Sender<TrayModel> {
    static CH: OnceLock<watch::Sender<TrayModel>> = OnceLock::new();
    CH.get_or_init(|| watch::channel(TrayModel::default()).0)
}

/// Publishes the latest model. The tray service (if running) picks it up;
/// without one this only stores the value.
pub fn publish(model: TrayModel) {
    channel().send_replace(model);
}

/// A receiver for model changes (used by the tray service).
pub fn subscribe() -> watch::Receiver<TrayModel> {
    channel().subscribe()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(unread: u32) -> TrayModel {
        TrayModel {
            unread_chats: unread,
            ..Default::default()
        }
    }

    #[test]
    fn tooltip_counts_unread_chats() {
        assert_eq!(model(0).tooltip(), "Ferrochat");
        assert_eq!(model(1).tooltip(), "Ferrochat \u{2014} 1 unread chat");
        assert_eq!(model(7).tooltip(), "Ferrochat \u{2014} 7 unread chats");
    }

    #[test]
    fn attention_follows_unread() {
        assert_eq!(model(0).attention(), Attention::Idle);
        assert_eq!(model(2).attention(), Attention::NeedsAttention);
    }

    #[test]
    fn menu_has_the_four_items_in_order() {
        let m = TrayModel {
            window_visible: true,
            muted: true,
            ..Default::default()
        };
        assert_eq!(
            m.menu(),
            vec![
                MenuEntry::Item {
                    label: "Hide Ferrochat",
                    cmd: TrayCmd::Toggle
                },
                MenuEntry::Separator,
                MenuEntry::Check {
                    label: "Mute notifications",
                    checked: true,
                    cmd: TrayCmd::ToggleMute
                },
                MenuEntry::Check {
                    label: "Start minimized to tray",
                    checked: false,
                    cmd: TrayCmd::ToggleStartMinimized
                },
                MenuEntry::Separator,
                MenuEntry::Item {
                    label: "Quit",
                    cmd: TrayCmd::Quit
                },
            ]
        );
        assert!(matches!(
            TrayModel::default().menu()[0],
            MenuEntry::Item {
                label: "Show Ferrochat",
                ..
            }
        ));
    }

    #[test]
    fn publish_reaches_subscribers() {
        let rx = subscribe();
        publish(model(4));
        assert_eq!(rx.borrow().unread_chats, 4);
        publish(TrayModel::default());
    }
}
