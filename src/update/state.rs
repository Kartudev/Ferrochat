//! The updater's state machine (pure; the iced glue is `app/updates.rs`).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use serde::{Deserialize, Serialize};

use super::http::{CheckError, CheckOutcome, Policy};
use super::install::{InstallError, Stage};
use super::method::InstallMethod;
use super::release::{Package, Platform, Release, select_package};
use super::version::Version;

/// `<state_dir>/update.json`: dismissed version and the last answer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Persisted {
    /// The banner [x] was clicked for this version.
    pub dismissed: Option<Version>,
    /// `ETag` of the last 200 answer.
    pub etag: Option<String>,
    /// The release that answer described.
    pub release: Option<Release>,
}

/// File name inside the state directory.
pub const FILE: &str = "update.json";

impl Persisted {
    /// Reads the file; defaults when missing or corrupt.
    pub fn load(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    /// Atomic write (temp + rename).
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        let json = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, path)
    }
}

/// Result of the last check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckState {
    /// No check yet.
    Idle,
    /// In flight.
    Checking {
        /// Started from settings (results are shown, errors too).
        manual: bool,
    },
    /// Latest release is not newer.
    UpToDate,
    /// The check failed.
    Failed(String),
    /// GitHub is rate-limiting us.
    RateLimited,
    /// No network checks in this run (mock / screenshot / debug).
    Disabled,
}

/// The install's progress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    /// Nothing running.
    Idle,
    /// Downloading / verifying / installing this version.
    Running {
        /// Version being installed.
        version: Version,
        /// Current stage.
        stage: Stage,
    },
    /// Installed; restart to finish.
    Installed(Version),
    /// The install failed (the current install is untouched).
    Failed(String),
}

/// What the banner shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Banner {
    /// "Ferrochat X is available".
    Available(Version),
    /// Download / verify / install progress.
    Progress(Version, Stage),
    /// "Restart to finish".
    Restart(Version),
    /// The install failed.
    Failed(String),
}

/// Updater state, owned by `App`.
#[derive(Debug)]
pub struct Updater {
    /// The running version.
    pub current: Version,
    /// How this copy was installed.
    pub method: InstallMethod,
    /// Release API URL and fetch policy; `None`: no network in this run.
    pub api: Option<(String, Policy)>,
    /// Where [`Persisted`] lives (`None` in mock mode: no disk writes).
    pub path: Option<PathBuf>,
    /// Dismissal and cached answer.
    pub persisted: Persisted,
    /// Last check.
    pub check: CheckState,
    /// Install progress.
    pub phase: Phase,
    /// Timer chain generation (stale `Due` messages are ignored).
    pub generation: u32,
    /// Install run id (stale progress is ignored).
    pub run: u32,
    /// Cancels the running download.
    pub cancel: Option<Arc<AtomicBool>>,
    /// "Later" was chosen after the install: the banner stays hidden.
    pub later: bool,
    /// Platform for asset selection.
    pub platform: Platform,
}

impl Updater {
    /// A new updater. `api: None` disables every network check.
    pub fn new(
        method: InstallMethod,
        api: Option<(String, Policy)>,
        path: Option<PathBuf>,
    ) -> Self {
        let persisted = path.as_deref().map(Persisted::load).unwrap_or_default();
        let check = if api.is_some() {
            CheckState::Idle
        } else {
            CheckState::Disabled
        };
        Self {
            current: Version::current(),
            method,
            api,
            path,
            persisted,
            check,
            phase: Phase::Idle,
            generation: 0,
            run: 0,
            cancel: None,
            later: false,
            platform: Platform::current(),
        }
    }

    /// Network checks are possible in this run.
    pub fn enabled(&self) -> bool {
        self.api.is_some()
    }

    /// The newest known release, if it is newer than this build.
    pub fn available(&self) -> Option<&Release> {
        self.persisted
            .release
            .as_ref()
            .filter(|r| r.version > self.current)
    }

    /// The archive to install, when this install can update itself.
    pub fn package(&self) -> Option<Package> {
        if !self.method.self_updates() {
            return None;
        }
        select_package(self.available()?, self.platform)
    }

    /// The banner, if any.
    pub fn banner(&self) -> Option<Banner> {
        match &self.phase {
            Phase::Running { version, stage } => Some(Banner::Progress(*version, *stage)),
            Phase::Installed(v) if !self.later => Some(Banner::Restart(*v)),
            Phase::Installed(_) => None,
            Phase::Failed(m) => Some(Banner::Failed(m.clone())),
            Phase::Idle => {
                let r = self.available()?;
                (self.persisted.dismissed != Some(r.version))
                    .then_some(Banner::Available(r.version))
            }
        }
    }

    /// Starts a check. `false` if one is running or checks are off.
    pub fn begin_check(&mut self, manual: bool) -> bool {
        if !self.enabled() || matches!(self.check, CheckState::Checking { .. }) {
            return false;
        }
        self.check = CheckState::Checking { manual };
        true
    }

    /// ETag to send (only with a stored answer to fall back on).
    pub fn etag(&self) -> Option<String> {
        self.persisted.etag.clone()
    }

    /// Applies a check result. Returns whether [`Persisted`] changed.
    pub fn finish_check(&mut self, result: Result<CheckOutcome, CheckError>) -> bool {
        let mut changed = false;
        match result {
            Ok(CheckOutcome::Fresh { release, etag }) => {
                if self.persisted.release != release || self.persisted.etag != etag {
                    self.persisted.release = release;
                    self.persisted.etag = etag;
                    changed = true;
                }
                self.after_answer();
            }
            Ok(CheckOutcome::NotModified) => self.after_answer(),
            Err(CheckError::RateLimited { .. }) => self.check = CheckState::RateLimited,
            Err(CheckError::Failed(m)) => {
                tracing::info!(error = %m, "update check failed");
                self.check = CheckState::Failed(m);
            }
        }
        changed
    }

    fn after_answer(&mut self) {
        self.check = if self.available().is_some() {
            CheckState::Idle
        } else {
            CheckState::UpToDate
        };
    }

    /// Banner [x]: hide this version until a newer one appears; also
    /// closes a failure notice. Returns whether [`Persisted`] changed.
    pub fn dismiss(&mut self) -> bool {
        if matches!(self.phase, Phase::Failed(_)) {
            self.phase = Phase::Idle;
        }
        match self.available().map(|r| r.version) {
            Some(v) if self.persisted.dismissed != Some(v) => {
                self.persisted.dismissed = Some(v);
                true
            }
            _ => false,
        }
    }

    /// Starts an install run; returns its id and the cancel flag.
    pub fn begin_install(&mut self) -> Option<(u32, Package, Arc<AtomicBool>)> {
        if matches!(self.phase, Phase::Running { .. } | Phase::Installed(_)) {
            return None;
        }
        let package = self.package()?;
        self.run = self.run.wrapping_add(1);
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Some(cancel.clone());
        self.phase = Phase::Running {
            version: package.version,
            stage: Stage::Downloading {
                received: 0,
                total: None,
            },
        };
        Some((self.run, package, cancel))
    }

    /// Progress from run `run`.
    pub fn progress(&mut self, run: u32, stage: Stage) {
        if run != self.run {
            return;
        }
        if let Phase::Running { stage: s, .. } = &mut self.phase {
            *s = stage;
        }
    }

    /// The download can still be cancelled.
    pub fn cancellable(&self) -> bool {
        matches!(
            self.phase,
            Phase::Running {
                stage: Stage::Downloading { .. },
                ..
            }
        )
    }

    /// Cancel button: stops the download, back to "available".
    pub fn cancel(&mut self) {
        if !self.cancellable() {
            return;
        }
        if let Some(c) = self.cancel.take() {
            c.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.run = self.run.wrapping_add(1);
        self.phase = Phase::Idle;
    }

    /// The end of run `run`.
    pub fn finish_install(&mut self, run: u32, result: Result<(), InstallError>) {
        if run != self.run {
            return;
        }
        self.cancel = None;
        let version = match &self.phase {
            Phase::Running { version, .. } => *version,
            _ => return,
        };
        self.phase = match result {
            Ok(()) => Phase::Installed(version),
            Err(InstallError::Cancelled) => Phase::Idle,
            Err(InstallError::Failed(m)) => Phase::Failed(m),
        };
    }

    /// The release page to open: the available release, else this version's.
    pub fn release_page(&self) -> String {
        self.available()
            .map(|r| r.html_url.clone())
            .unwrap_or_else(|| super::release::tag_page(self.current))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::method::{ScriptLayout, Unmanaged};
    use crate::update::release::Asset;

    fn release(v: Version) -> Release {
        let name = format!("ferrochat-v{v}-linux-x86_64.tar.gz");
        Release {
            version: v,
            html_url: super::super::release::tag_page(v),
            assets: vec![
                Asset {
                    name: name.clone(),
                    url: format!("https://github.com/x/{name}"),
                    size: 10,
                },
                Asset {
                    name: format!("{name}.sha256"),
                    url: format!("https://github.com/x/{name}.sha256"),
                    size: 1,
                },
            ],
        }
    }

    fn script() -> InstallMethod {
        InstallMethod::Script(ScriptLayout {
            bin_dir: "/b".into(),
            data_dir: "/d".into(),
            windows: false,
        })
    }

    fn updater() -> Updater {
        let mut u = Updater::new(script(), Some(("http://x".into(), Policy::github())), None);
        u.current = Version::new(1, 0, 0);
        u.platform = Platform::LinuxX86_64;
        u
    }

    fn fresh(v: Version) -> Result<CheckOutcome, CheckError> {
        Ok(CheckOutcome::Fresh {
            release: Some(release(v)),
            etag: Some("\"e\"".into()),
        })
    }

    #[test]
    fn newer_release_shows_the_banner_older_does_not() {
        let mut u = updater();
        assert!(u.begin_check(false));
        assert!(!u.begin_check(true), "one check at a time");
        assert!(u.finish_check(fresh(Version::new(1, 1, 0))));
        assert_eq!(u.banner(), Some(Banner::Available(Version::new(1, 1, 0))));
        assert_eq!(u.etag().as_deref(), Some("\"e\""));
        u.begin_check(true);
        assert!(u.finish_check(fresh(Version::new(1, 0, 0))));
        assert_eq!(u.banner(), None);
        assert_eq!(u.check, CheckState::UpToDate);
        u.begin_check(true);
        u.finish_check(Ok(CheckOutcome::NotModified));
        assert_eq!(u.check, CheckState::UpToDate);
    }

    #[test]
    fn dismissal_hides_until_a_newer_version() {
        let mut u = updater();
        u.finish_check(fresh(Version::new(1, 1, 0)));
        assert!(u.dismiss());
        assert_eq!(u.banner(), None);
        assert!(!u.dismiss());
        u.finish_check(fresh(Version::new(1, 2, 0)));
        assert_eq!(u.banner(), Some(Banner::Available(Version::new(1, 2, 0))));
    }

    #[test]
    fn dismissal_persists() {
        let dir = std::env::temp_dir().join(format!("ferrochat-upd-state-{}", std::process::id()));
        let path = dir.join(FILE);
        let _ = std::fs::remove_dir_all(&dir);
        let mut u = Updater::new(
            script(),
            Some(("http://x".into(), Policy::github())),
            Some(path.clone()),
        );
        u.current = Version::new(1, 0, 0);
        u.finish_check(fresh(Version::new(1, 1, 0)));
        u.dismiss();
        u.persisted.save(&path).unwrap();
        let mut again = Updater::new(script(), None, Some(path.clone()));
        again.current = Version::new(1, 0, 0);
        assert_eq!(again.persisted.dismissed, Some(Version::new(1, 1, 0)));
        assert_eq!(again.banner(), None);
        assert_eq!(again.check, CheckState::Disabled);
        // Corrupt file: defaults.
        std::fs::write(&path, b"{").unwrap();
        assert_eq!(Persisted::load(&path), Persisted::default());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn errors_and_rate_limits() {
        let mut u = updater();
        u.begin_check(true);
        u.finish_check(Err(CheckError::RateLimited { retry_after: None }));
        assert_eq!(u.check, CheckState::RateLimited);
        u.begin_check(true);
        u.finish_check(Err(CheckError::Failed("x".into())));
        assert_eq!(u.check, CheckState::Failed("x".into()));
        let mut off = Updater::new(script(), None, None);
        assert!(!off.begin_check(true));
    }

    #[test]
    fn install_run_progress_cancel_and_finish() {
        let mut u = updater();
        assert!(u.begin_install().is_none(), "nothing available");
        u.finish_check(fresh(Version::new(1, 1, 0)));
        let (run, pkg, cancel) = u.begin_install().unwrap();
        assert_eq!(pkg.version, Version::new(1, 1, 0));
        assert!(u.begin_install().is_none(), "one run at a time");
        u.progress(
            run,
            Stage::Downloading {
                received: 5,
                total: Some(10),
            },
        );
        assert!(u.cancellable());
        u.cancel();
        assert!(cancel.load(std::sync::atomic::Ordering::Relaxed));
        assert_eq!(u.banner(), Some(Banner::Available(Version::new(1, 1, 0))));
        // The cancelled run's late messages are ignored.
        u.progress(run, Stage::Installing);
        u.finish_install(run, Err(InstallError::Cancelled));
        assert_eq!(u.phase, Phase::Idle);

        let (run, _, _) = u.begin_install().unwrap();
        u.progress(run, Stage::Verifying);
        assert!(!u.cancellable());
        u.finish_install(run, Err(InstallError::Failed("bad".into())));
        assert_eq!(u.banner(), Some(Banner::Failed("bad".into())));
        u.dismiss();
        assert_eq!(u.phase, Phase::Idle);

        let mut u = updater();
        u.finish_check(fresh(Version::new(1, 1, 0)));
        let (run, _, _) = u.begin_install().unwrap();
        u.finish_install(run, Ok(()));
        assert_eq!(u.banner(), Some(Banner::Restart(Version::new(1, 1, 0))));
        u.later = true;
        assert_eq!(u.banner(), None);
        assert!(u.begin_install().is_none(), "already installed");
    }

    #[test]
    fn only_script_installs_get_a_package() {
        let mut u = updater();
        u.finish_check(fresh(Version::new(1, 1, 0)));
        assert!(u.package().is_some());
        u.method = InstallMethod::Cargo;
        assert!(u.package().is_none());
        u.method = InstallMethod::Unmanaged(Unmanaged::System);
        assert!(u.package().is_none());
        assert!(u.release_page().ends_with("/releases/tag/v1.1.0"));
    }
}
