//! Updater glue: timers, checks, install runs and restart as iced tasks.
//! The decisions are in `update::state::Updater`.

use std::time::{Duration, Instant};

use iced::Task;

use super::{App, Done, Message, Ui};
use crate::config::{Config, ScreenshotTarget};
use crate::update::method::{self, InstallMethod, ScriptLayout};
use crate::update::release::{Asset, Release, archive_name, tag_page};
use crate::update::{
    self, CRATE, CheckError, InstallError, Phase, Stage, UpdateMsg, Updater, http, install,
    restart, schedule, state,
};

fn wrap(m: UpdateMsg) -> Message {
    Message::Update(m)
}

impl App {
    /// The updater for this run: no network in mock / screenshot / debug
    /// runs (unless `FERRO_UPDATE_FORCE=1`), no disk writes in mock mode.
    pub(super) fn new_updater(cfg: &Config) -> Updater {
        let allowed = schedule::checks_allowed(
            cfg.mock,
            cfg.screenshot.is_some(),
            cfg!(debug_assertions),
            schedule::forced(),
        );
        let api = allowed.then(http::api_endpoint).flatten();
        let path = (!cfg.mock).then(|| cfg.state_dir.join(state::FILE));
        let method = method::detect(&method::probe());
        if let InstallMethod::Script(layout) = &method
            && layout.windows
            && !cfg.mock
        {
            // The program a previous update moved aside (it was running then).
            let dir = layout.bin_dir.clone();
            let _ = std::thread::Builder::new()
                .name("updater-cleanup".into())
                .spawn(move || install::clean_leftovers(&dir));
        }
        let mut u = Updater::new(method, api, path);
        if matches!(
            cfg.screenshot,
            Some((
                ScreenshotTarget::UpdateBanner | ScreenshotTarget::UpdateSettings,
                _
            ))
        ) {
            demo(&mut u);
        }
        u
    }

    /// Boot: arm the first automatic check (10-20 s, off the critical path).
    pub(super) fn updates_boot(&mut self) -> Task<Message> {
        if self.updater.enabled() && self.settings.check_updates {
            self.schedule_update_check(schedule::first_delay(schedule::random()))
        } else {
            Task::none()
        }
    }

    fn schedule_update_check(&self, delay: Duration) -> Task<Message> {
        let generation = self.updater.generation;
        Task::perform(
            async move {
                tokio::time::sleep(delay).await;
                generation
            },
            |g| wrap(UpdateMsg::Due(g)),
        )
    }

    fn start_update_check(&mut self, manual: bool) -> Task<Message> {
        if !self.updater.begin_check(manual) {
            return Task::none();
        }
        let Some((api, policy)) = self.updater.api.clone() else {
            return Task::none();
        };
        let etag = self.updater.etag();
        Task::perform(
            update::off_thread(move || http::fetch_latest(&api, &policy, etag.as_deref())),
            move |r| {
                wrap(UpdateMsg::Checked {
                    manual,
                    result: r.unwrap_or_else(|| Err(CheckError::Failed("check failed".into()))),
                })
            },
        )
    }

    fn persist_updater(&self) -> Task<Message> {
        let Some(path) = self.updater.path.clone() else {
            return Task::none();
        };
        let data = self.updater.persisted.clone();
        super::blocking(
            move || data.save(&path).map_err(|e| e.to_string()),
            |r| Message::done(Done::Stored(r)),
        )
    }

    pub(super) fn on_update(&mut self, m: UpdateMsg) -> Task<Message> {
        match m {
            UpdateMsg::Due(g) => {
                if g != self.updater.generation || !self.settings.check_updates {
                    return Task::none();
                }
                let t = self.start_update_check(false);
                if matches!(
                    self.updater.check,
                    update::CheckState::Checking { manual: false }
                ) {
                    t
                } else {
                    // A manual check is running: keep the timer chain alive.
                    self.schedule_update_check(schedule::next_delay(schedule::random()))
                }
            }
            UpdateMsg::CheckNow => self.start_update_check(true),
            UpdateMsg::Checked { manual, result } => {
                let retry = match &result {
                    Err(CheckError::RateLimited { retry_after }) => Some(*retry_after),
                    _ => None,
                };
                let changed = self.updater.finish_check(result);
                let persist = if changed {
                    self.persist_updater()
                } else {
                    Task::none()
                };
                let next = if !manual && self.settings.check_updates {
                    let r = schedule::random();
                    self.schedule_update_check(match retry {
                        Some(after) => schedule::backoff(r, after),
                        None => schedule::next_delay(r),
                    })
                } else {
                    Task::none()
                };
                Task::batch([persist, next])
            }
            UpdateMsg::Dismiss => {
                if self.updater.dismiss() {
                    self.persist_updater()
                } else {
                    Task::none()
                }
            }
            UpdateMsg::Install => self.start_install(),
            UpdateMsg::Progress(run, stage) => {
                self.updater.progress(run, stage);
                Task::none()
            }
            UpdateMsg::Finished(run, result) => {
                self.updater.finish_install(run, result);
                Task::none()
            }
            UpdateMsg::Cancel => {
                self.updater.cancel();
                Task::none()
            }
            UpdateMsg::Later => {
                self.updater.later = true;
                Task::none()
            }
            UpdateMsg::Restart => self.restart_after_update(),
            UpdateMsg::ToggleAuto => {
                self.settings.check_updates = !self.settings.check_updates;
                self.updater.generation = self.updater.generation.wrapping_add(1);
                let persist = if self.cfg.mock {
                    Task::none()
                } else {
                    let (dir, settings) = (self.cfg.config_dir.clone(), self.settings.clone());
                    super::blocking(
                        move || settings.save(&dir).map_err(|e| e.to_string()),
                        |r| Message::done(Done::Stored(r)),
                    )
                };
                let arm = if self.settings.check_updates && self.updater.enabled() {
                    self.schedule_update_check(schedule::first_delay(schedule::random()))
                } else {
                    Task::none()
                };
                Task::batch([persist, arm])
            }
            UpdateMsg::CopyCommand => {
                self.toast("Command copied to the clipboard.");
                iced::clipboard::write(method::cargo_command())
            }
            UpdateMsg::OpenReleasePage => {
                Task::done(Message::Ui(Ui::OpenLink(self.updater.release_page())))
            }
        }
    }

    fn start_install(&mut self) -> Task<Message> {
        let layout = match &self.updater.method {
            InstallMethod::Script(l) => l.clone(),
            InstallMethod::Cargo => return self.on_update(UpdateMsg::CopyCommand),
            InstallMethod::Unmanaged(_) => return self.on_update(UpdateMsg::OpenReleasePage),
        };
        let Some((_, policy)) = self.updater.api.clone() else {
            self.toast("Updates cannot be installed in this mode.");
            return Task::none();
        };
        let Some((run, package, cancel)) = self.updater.begin_install() else {
            return Task::none();
        };
        tracing::info!(version = %package.version, "installing update");
        let (tx, rx) = futures::channel::mpsc::unbounded();
        let spawned = std::thread::Builder::new()
            .name("updater".into())
            .spawn(move || {
                let mut last = Instant::now();
                let mut report = |stage: Stage| {
                    // At most ~10 progress messages per second.
                    let downloading = matches!(stage, Stage::Downloading { .. });
                    if downloading && last.elapsed() < Duration::from_millis(100) {
                        return;
                    }
                    last = Instant::now();
                    let _ = tx.unbounded_send(UpdateMsg::Progress(run, stage));
                };
                let result = install::install(&layout, &package, &policy, &cancel, &mut report);
                if let Err(InstallError::Failed(e)) = &result {
                    tracing::warn!(error = %e, "update failed");
                }
                let _ = tx.unbounded_send(UpdateMsg::Finished(run, result));
            });
        if let Err(e) = spawned {
            self.updater
                .finish_install(run, Err(InstallError::Failed(e.to_string())));
            return Task::none();
        }
        Task::run(rx, wrap)
    }

    /// Starts the new binary (after `main` releases the instance lock) with
    /// the same arguments, `--minimized` if the window is hidden, and quits.
    fn restart_after_update(&mut self) -> Task<Message> {
        let (Phase::Installed(_), InstallMethod::Script(layout)) =
            (&self.updater.phase, &self.updater.method)
        else {
            return Task::none();
        };
        if self.cfg.mock {
            self.toast("Restart is not available in mock mode.");
            return Task::none();
        }
        restart::request(restart::Restart {
            exe: layout.bin(CRATE),
            args: restart::restart_args(std::env::args().skip(1), self.window_id.is_none()),
        });
        self.quit()
    }
}

/// Screenshot mode: pretend a newer release exists for an install.sh copy.
fn demo(u: &mut Updater) {
    let current = u.current;
    let version = update::Version::new(current.major, current.minor + 1, 0);
    let name = archive_name(u.platform, version).unwrap_or_default();
    u.method = InstallMethod::Script(ScriptLayout {
        bin_dir: "/nonexistent/bin".into(),
        data_dir: "/nonexistent/share".into(),
        windows: false,
    });
    u.persisted.release = Some(Release {
        version,
        html_url: tag_page(version),
        assets: vec![
            Asset {
                url: format!(
                    "https://github.com/{}/releases/download/v{version}/{name}",
                    update::REPO
                ),
                name: name.clone(),
                size: 0,
            },
            Asset {
                url: format!(
                    "https://github.com/{}/releases/download/v{version}/{name}.sha256",
                    update::REPO
                ),
                name: format!("{name}.sha256"),
                size: 0,
            },
        ],
    });
}
