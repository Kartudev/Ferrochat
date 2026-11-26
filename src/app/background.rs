//! Background mode: window lifecycle, tray events, settings toggles.
//!
//! The decisions are in `window::WindowMode`; this file turns them into iced
//! tasks. Hiding *closes* the window (iced `daemon` keeps running with zero
//! windows) so the renderer is freed; showing opens a new one at the last
//! size and position.

use iced::{Point, Size, Task, window};

use super::window::{Effect, WindowMode};
use super::{App, Done, Message, Setting};
use crate::ext::External;
use crate::tray::{TrayCmd, TrayModel};

/// Last known window geometry, reused when the window is reopened.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowGeom {
    /// Logical size.
    pub size: Size,
    /// Top-left position, when the platform reports it (not on Wayland).
    pub position: Option<Point>,
}

impl WindowGeom {
    /// Default size for a normal run, or the fixed screenshot size.
    pub fn initial(screenshot: bool) -> Self {
        Self {
            size: if screenshot {
                Size::new(1280.0, 800.0)
            } else {
                Size::new(1200.0, 780.0)
            },
            position: None,
        }
    }
}

/// Returns freed heap pages to the OS (glibc keeps them otherwise, so the
/// resident size would not drop after the window and its surfaces are gone).
#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn trim_memory() {
    unsafe extern "C" {
        fn malloc_trim(pad: usize) -> i32;
    }
    // SAFETY: `malloc_trim` takes no pointers and is safe to call at any time.
    unsafe {
        malloc_trim(0);
    }
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn trim_memory() {}

/// Maps a tray click / menu entry to the message that handles it.
impl From<TrayCmd> for Message {
    fn from(cmd: TrayCmd) -> Self {
        Message::External(External::Tray(cmd))
    }
}

impl App {
    /// Settings for a (re)opened main window.
    pub(crate) fn window_settings(&self) -> window::Settings {
        window::Settings {
            size: self.geom.size,
            position: self
                .geom
                .position
                .map(window::Position::Specific)
                .unwrap_or_default(),
            min_size: Some(Size::new(760.0, 520.0)),
            // Close requests are routed through `WindowMode`.
            exit_on_close_request: false,
            icon: crate::icon::window_icon(),
            platform_specific: window::settings::PlatformSpecific {
                #[cfg(target_os = "linux")]
                application_id: crate::config::APP_ID.into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    pub(super) fn open_window(&mut self) -> Task<Message> {
        if self.window_id.is_some() {
            return Task::none();
        }
        let (id, task) = window::open(self.window_settings());
        self.window_id = Some(id);
        task.discard()
    }

    fn close_window(&mut self) -> Task<Message> {
        self.focused = false;
        // Transient layers belong to the window that is going away.
        self.popup = None;
        self.hovered = None;
        self.menu_open = false;
        self.emoji_open = false;
        match self.window_id.take() {
            Some(id) => window::close(id),
            None => Task::none(),
        }
    }

    fn focus_window(&self) -> Task<Message> {
        match self.window_id {
            Some(id) => Task::batch([window::minimize(id, false), window::gain_focus(id)]),
            None => Task::none(),
        }
    }

    pub(super) fn quit(&mut self) -> Task<Message> {
        iced::exit()
    }

    pub(super) fn apply(&mut self, effect: Effect) -> Task<Message> {
        match effect {
            Effect::None => Task::none(),
            Effect::OpenWindow => self.open_window(),
            Effect::CloseWindow => self.close_window(),
            Effect::Focus => self.focus_window(),
            Effect::Exit => self.quit(),
        }
    }

    /// Show the window and, if given, open that chat.
    fn show_chat(&mut self, chat: Option<String>) -> Task<Message> {
        let effect = self.window.show();
        let show = self.apply(effect);
        let open = match chat {
            Some(c) if !self.auth.shows_login() && self.chats.get(&c).is_some() => {
                self.settings_open = false;
                self.open_chat(c)
            }
            _ => Task::none(),
        };
        Task::batch([show, open])
    }

    pub(super) fn on_external(&mut self, ev: External) -> Task<Message> {
        match ev {
            External::Show { chat } => self.show_chat(chat),
            External::TrayUp(up) => {
                self.tray_checked = true;
                let effect = self.window.tray_changed(up);
                self.apply(effect)
            }
            External::Tray(TrayCmd::Toggle) => {
                let effect = self.window.toggle();
                self.apply(effect)
            }
            External::Tray(TrayCmd::ToggleMute) => self.toggle_setting(Setting::Muted),
            External::Tray(TrayCmd::ToggleStartMinimized) => {
                self.toggle_setting(Setting::StartMinimized)
            }
            External::Tray(TrayCmd::Quit) => self.quit(),
        }
    }

    pub(super) fn on_close_requested(&mut self, id: window::Id) -> Task<Message> {
        if Some(id) != self.window_id {
            return window::close(id);
        }
        let effect = self.window.close_requested();
        self.apply(effect)
    }

    pub(super) fn on_window_closed(&mut self, id: window::Id) -> Task<Message> {
        if Some(id) != self.window_id {
            // Our own close (hide to tray): hand the renderer's freed pages
            // back to the OS.
            trim_memory();
            return Task::none();
        }
        self.window_id = None;
        let effect = self.window.window_closed();
        self.apply(effect)
    }

    pub(super) fn on_window_resized(&mut self, id: window::Id, size: Size) {
        if Some(id) == self.window_id && size.width >= 1.0 && size.height >= 1.0 {
            self.geom.size = size;
        }
    }

    pub(super) fn on_window_moved(&mut self, id: window::Id, position: Point) {
        if Some(id) == self.window_id {
            self.geom.position = Some(position);
        }
    }

    /// Flips one preference, persists it and applies its effect.
    pub(super) fn toggle_setting(&mut self, setting: Setting) -> Task<Message> {
        let persist = |app: &Self| -> Task<Message> {
            if app.cfg.mock {
                return Task::none();
            }
            let (dir, settings) = (app.cfg.config_dir.clone(), app.settings.clone());
            super::blocking(
                move || settings.save(&dir).map_err(|e| e.to_string()),
                |r| Message::done(Done::Stored(r)),
            )
        };
        match setting {
            Setting::CloseToTray => {
                self.settings.close_to_tray = !self.settings.close_to_tray;
                self.window.close_to_tray = self.settings.close_to_tray;
                persist(self)
            }
            Setting::StartMinimized => {
                self.settings.start_minimized = !self.settings.start_minimized;
                persist(self)
            }
            Setting::Muted => {
                self.settings.muted = !self.settings.muted;
                persist(self)
            }
            Setting::Autostart => {
                let want = !self.autostart;
                if self.cfg.mock {
                    // No disk writes in mock mode.
                    self.autostart = want;
                    return Task::none();
                }
                super::blocking(
                    move || {
                        if want {
                            crate::autostart::enable()
                        } else {
                            crate::autostart::disable()
                        }
                        .map(|()| want)
                        .map_err(|e| e.to_string())
                    },
                    |r| Message::done(Done::Autostart(r)),
                )
            }
        }
    }

    /// A tray icon is registered and can bring the window back.
    pub(crate) fn tray_up(&self) -> bool {
        self.window.tray_up
    }

    /// Chats with unread messages, muted chats excluded.
    pub(crate) fn unread_chats(&self) -> u32 {
        let now = super::now_secs();
        self.chats
            .entries
            .iter()
            .filter(|e| e.meta.unread > 0 && !e.meta.is_muted(now))
            .count() as u32
    }

    /// What the tray should currently show.
    pub(crate) fn tray_model(&self) -> TrayModel {
        TrayModel {
            unread_chats: self.unread_chats(),
            window_visible: self.window.open,
            muted: self.settings.muted,
            start_minimized: self.settings.start_minimized,
        }
    }

    /// Pushes the tray model when it changed.
    pub(super) fn sync_tray(&mut self) {
        let model = self.tray_model();
        if self.tray_sent.as_ref() != Some(&model) {
            crate::tray::publish(model.clone());
            self.tray_sent = Some(model);
        }
    }

    /// Initial window state for `cfg` and the saved settings.
    pub(super) fn initial_mode(cfg: &crate::config::Config, close_to_tray: bool) -> WindowMode {
        WindowMode::start(cfg.minimized, close_to_tray)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Cli, Config};

    fn app(args: &[&str]) -> App {
        let args = args.iter().map(|s| s.to_string());
        let Cli::Run(cfg) = Config::from_args(args) else {
            panic!("bad args")
        };
        App::boot(*cfg).0
    }

    #[test]
    fn tray_commands_map_to_external_messages() {
        for cmd in [
            TrayCmd::Toggle,
            TrayCmd::ToggleMute,
            TrayCmd::ToggleStartMinimized,
            TrayCmd::Quit,
        ] {
            assert!(matches!(
                Message::from(cmd),
                Message::External(External::Tray(c)) if c == cmd
            ));
        }
    }

    #[test]
    fn boots_with_a_window_unless_minimized() {
        let a = app(&["--mock"]);
        assert!(a.window.open && a.window_id.is_some());
        let b = app(&["--mock", "--minimized"]);
        assert!(!b.window.open && b.window_id.is_none());
        assert!(!b.focused);
    }

    #[test]
    fn close_goes_to_the_tray_only_when_one_exists() {
        let mut a = app(&["--mock"]);
        let id = a.window_id.unwrap();
        // No tray yet: closing quits (Exit effect leaves the window state alone).
        let _ = a.update(Message::CloseRequested(id));
        assert!(a.window.open);
        // Tray registered: closing hides and forgets the window.
        let _ = a.update(Message::External(External::TrayUp(true)));
        let _ = a.update(Message::CloseRequested(id));
        assert!(!a.window.open && a.window_id.is_none());
        // Tray menu "Show" reopens it.
        let _ = a.update(Message::External(External::Tray(TrayCmd::Toggle)));
        assert!(a.window.open && a.window_id.is_some());
    }

    #[test]
    fn second_launch_and_notification_click_show_the_window() {
        let mut a = app(&["--mock"]);
        let _ = a.update(Message::External(External::TrayUp(true)));
        let id = a.window_id.unwrap();
        let _ = a.update(Message::CloseRequested(id));
        assert!(a.window_id.is_none());
        let _ = a.update(Message::External(External::Show { chat: None }));
        assert!(a.window.open && a.window_id.is_some());
    }

    #[test]
    fn minimized_start_without_a_tray_opens_the_window() {
        let mut a = app(&["--mock", "--minimized"]);
        let _ = a.update(Message::External(External::TrayUp(false)));
        assert!(a.window.open && a.window_id.is_some());
    }

    #[test]
    fn toggles_flip_settings_and_the_tray_model_follows() {
        let mut a = app(&["--mock"]);
        assert!(a.settings.close_to_tray);
        let _ = a.update(Message::External(External::Tray(TrayCmd::ToggleMute)));
        assert!(a.settings.muted && a.tray_model().muted);
        let _ = a.update(Message::External(External::Tray(
            TrayCmd::ToggleStartMinimized,
        )));
        assert!(a.settings.start_minimized && a.tray_model().start_minimized);
        let _ = a.update(Message::Ui(crate::app::Ui::Toggle(Setting::CloseToTray)));
        assert!(!a.settings.close_to_tray && !a.window.close_to_tray);
        // Close-to-tray off: closing quits even with a tray.
        let _ = a.update(Message::External(External::TrayUp(true)));
        let id = a.window_id.unwrap();
        let _ = a.update(Message::CloseRequested(id));
        assert!(a.window.open);
    }

    #[test]
    fn mock_autostart_toggle_writes_nothing() {
        let mut a = app(&["--mock"]);
        assert!(!a.autostart);
        let _ = a.update(Message::Ui(crate::app::Ui::Toggle(Setting::Autostart)));
        assert!(a.autostart);
    }

    #[test]
    fn geometry_is_remembered_for_reopen() {
        let mut a = app(&["--mock"]);
        let id = a.window_id.unwrap();
        let _ = a.update(Message::WindowResized(id, Size::new(900.0, 640.0)));
        let _ = a.update(Message::WindowMoved(id, Point::new(40.0, 50.0)));
        let s = a.window_settings();
        assert_eq!(s.size, Size::new(900.0, 640.0));
        assert!(matches!(s.position, window::Position::Specific(p) if p == Point::new(40.0, 50.0)));
    }
}
