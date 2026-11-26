//! "Restart now": the new binary is started by `main` after iced has quit
//! and the single-instance lock is released, so the new process becomes the
//! first instance instead of handing off to the old one.

use std::path::PathBuf;
use std::sync::Mutex;

/// What to start after quitting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Restart {
    /// The (new) binary.
    pub exe: PathBuf,
    /// Its arguments.
    pub args: Vec<String>,
}

static PENDING: Mutex<Option<Restart>> = Mutex::new(None);

/// The same arguments, with `--minimized` if (and only if) the window is
/// hidden in the tray.
pub fn restart_args(args: impl IntoIterator<Item = String>, hidden: bool) -> Vec<String> {
    let mut out: Vec<String> = args.into_iter().filter(|a| a != "--minimized").collect();
    if hidden {
        out.push("--minimized".into());
    }
    out
}

/// Records the restart; the app then quits through its normal path.
pub fn request(r: Restart) {
    if let Ok(mut p) = PENDING.lock() {
        *p = Some(r);
    }
}

/// Takes the recorded restart, if any.
pub fn take() -> Option<Restart> {
    PENDING.lock().ok().and_then(|mut p| p.take())
}

/// Called by `main` after the event loop ended and the instance lock was
/// dropped: starts the new binary, detached.
pub fn run_pending() {
    let Some(r) = take() else {
        return;
    };
    let mut cmd = std::process::Command::new(&r.exe);
    cmd.args(&r.args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP: outlives this process.
        cmd.creation_flags(0x0000_0008 | 0x0000_0200);
    }
    match cmd.spawn() {
        Ok(_) => tracing::info!("restarted into the updated version"),
        Err(e) => {
            tracing::error!(error = %e, "could not restart; the update runs on the next start");
            eprintln!(
                "ferrochat: could not restart ({e}); start Ferrochat again to finish the update."
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimized_follows_the_window() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(restart_args(a(&["--minimized"]), false), a(&[]));
        assert_eq!(restart_args(a(&[]), true), a(&["--minimized"]));
        assert_eq!(
            restart_args(a(&["--minimized", "-x", "--minimized"]), true),
            a(&["-x", "--minimized"])
        );
    }

    #[test]
    fn request_and_take() {
        request(Restart {
            exe: "/bin/true".into(),
            args: vec![],
        });
        assert_eq!(take().map(|r| r.exe), Some(PathBuf::from("/bin/true")));
        assert_eq!(take(), None);
    }
}
