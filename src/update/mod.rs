//! In-app updater: checks GitHub Releases, shows a banner, and updates an
//! `install.sh` install in place (see `docs/architecture.md` "Updates").
//!
//! - [`version`]: strict semver compare against `CARGO_PKG_VERSION`.
//! - [`release`]: the `releases/latest` document and asset selection.
//! - [`method`]: install-method detection (pure, from a probe).
//! - [`http`]: the one API request and policy-checked downloads (`ureq`).
//! - [`install`]: verify, safe extract, atomic replace with rollback.
//! - [`schedule`]: when to check.
//! - [`state`]: the state machine `App` owns.
//! - [`restart`]: "Restart now", run by `main` after the lock is released.
//!
//! No iced types here: the glue is `app/updates.rs`, the views `ui/update.rs`.

pub mod http;
pub mod install;
pub mod method;
pub mod release;
pub mod restart;
pub mod schedule;
pub mod state;
pub mod version;

pub use http::{CheckError, CheckOutcome};
pub use install::{InstallError, Stage};
pub use method::InstallMethod;
pub use state::{Banner, CheckState, Phase, Updater};
pub use version::Version;

/// GitHub repository the releases come from (this fork's own releases).
/// `Cargo.toml` `repository` and `install.sh` `REPO` must name the same one;
/// `tests::release_repo_is_consistent` checks it.
pub const REPO: &str = "Kartudev/Ferrochat-kartu";
/// Crate / binary name (release asset prefix).
pub const CRATE: &str = "ferrochat";
/// User-facing name.
pub const APP_NAME: &str = "Ferrochat";
/// Desktop entry / icon id.
pub const APP_ID: &str = crate::config::APP_ID;
/// Helper binaries shipped next to the app (the list install.sh installs).
pub const HELPERS: &[&str] = &["ferrochat-player"];

/// Updater events (wrapped in `app::Message::Update`).
#[derive(Debug, Clone)]
pub enum UpdateMsg {
    /// Timer of chain `generation` fired: automatic check.
    Due(u32),
    /// "Check for updates" in settings.
    CheckNow,
    /// A check finished.
    Checked {
        /// Started from settings.
        manual: bool,
        /// Outcome.
        result: Result<CheckOutcome, CheckError>,
    },
    /// Banner [x].
    Dismiss,
    /// [Update] / "Update now".
    Install,
    /// Progress of install run `run`.
    Progress(u32, Stage),
    /// Install run `run` ended.
    Finished(u32, Result<(), InstallError>),
    /// Cancel the download.
    Cancel,
    /// "Restart now".
    Restart,
    /// "Later" (after the install).
    Later,
    /// "Check for updates automatically" toggle.
    ToggleAuto,
    /// Copy the `cargo install` command.
    CopyCommand,
    /// Open the release page / notes.
    OpenReleasePage,
}

/// Runs `f` on a detached thread and resolves with its result (`None` if it
/// panicked). Not tokio's blocking pool: iced drops its runtime on quit and
/// that waits for every blocking task, so a slow download would hang Quit.
pub fn off_thread<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
) -> impl std::future::Future<Output = Option<T>> {
    let (tx, rx) = futures::channel::oneshot::channel();
    let spawned = std::thread::Builder::new()
        .name("updater".into())
        .spawn(move || {
            let _ = tx.send(f());
        });
    async move {
        match spawned {
            Ok(_) => rx.await.ok(),
            Err(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::REPO;

    #[test]
    fn release_repo_is_consistent() {
        assert_eq!(
            env!("CARGO_PKG_REPOSITORY"),
            format!("https://github.com/{REPO}")
        );
        let script = include_str!("../../install.sh");
        assert!(script.contains(&format!("REPO=\"{REPO}\"")));
        assert!(script.contains(&format!("raw.githubusercontent.com/{REPO}/main/install.sh")));
    }
}
