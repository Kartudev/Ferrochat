//! How this copy of the app was installed, which decides what "update" does.
//!
//! [`probe`] reads the environment once (paths, markers); [`detect`] is a pure
//! function of that probe so every case is unit-tested.

use std::path::{Path, PathBuf};

use super::CRATE;

/// Where the files of an `install.sh` install live (same paths as the script).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptLayout {
    /// `${XDG_BIN_HOME:-~/.local/bin}`.
    pub bin_dir: PathBuf,
    /// `${XDG_DATA_HOME:-~/.local/share}`.
    pub data_dir: PathBuf,
}

impl ScriptLayout {
    /// `<bin_dir>/<name>`.
    pub fn bin(&self, name: &str) -> PathBuf {
        self.bin_dir.join(name)
    }
    /// `<data_dir>/<crate>/installed-version`.
    pub fn version_file(&self) -> PathBuf {
        self.data_dir.join(CRATE).join("installed-version")
    }
    /// `<data_dir>/applications`.
    pub fn apps_dir(&self) -> PathBuf {
        self.data_dir.join("applications")
    }
    /// `<data_dir>/icons/hicolor/scalable/apps`.
    pub fn icon_dir(&self) -> PathBuf {
        self.data_dir.join("icons/hicolor/scalable/apps")
    }
}

/// Why the app can only point at the release page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unmanaged {
    /// Running from a cargo `target/` directory.
    DevBuild,
    /// Flatpak, AppImage or Snap: the sandbox owns the files.
    Sandboxed,
    /// A system path (`/usr`, `/opt`, ...), e.g. a distro package.
    System,
    /// The install folder is not writable.
    ReadOnly,
    /// Somewhere else (copied by hand, unknown layout).
    Unknown,
}

/// The install method.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallMethod {
    /// `install.sh`: self-update supported.
    Script(ScriptLayout),
    /// `cargo install`: never self-update; show the cargo command.
    Cargo,
    /// Anything else: notify only, open the release page.
    Unmanaged(Unmanaged),
}

impl InstallMethod {
    /// The app can download and install the update itself.
    pub fn self_updates(&self) -> bool {
        matches!(self, Self::Script(_))
    }

    /// Settings label.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Script(_) => "install script (updates in the app)",
            Self::Cargo => "cargo install",
            Self::Unmanaged(Unmanaged::DevBuild) => "development build",
            Self::Unmanaged(Unmanaged::Sandboxed) => "Flatpak / AppImage / Snap",
            Self::Unmanaged(Unmanaged::System) => "system package",
            Self::Unmanaged(Unmanaged::ReadOnly) => "read-only folder",
            Self::Unmanaged(Unmanaged::Unknown) => "manual install",
        }
    }
}

/// The command that updates a `cargo install`.
pub fn cargo_command() -> String {
    format!(
        "cargo install --git https://github.com/{} --locked --force",
        super::REPO
    )
}

/// Everything [`detect`] needs, read from the environment by [`probe`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Probe {
    /// `current_exe()`, canonicalized.
    pub exe: Option<PathBuf>,
    /// install.sh's bin dir, canonicalized when it exists.
    pub script_bin_dir: Option<PathBuf>,
    /// install.sh's data dir.
    pub script_data_dir: Option<PathBuf>,
    /// `<data_dir>/<crate>/installed-version` exists.
    pub version_marker: bool,
    /// The bin dir looks writable.
    pub bin_dir_writable: bool,
    /// `${CARGO_HOME:-~/.cargo}/bin`, canonicalized when it exists.
    pub cargo_bin_dir: Option<PathBuf>,
    /// Flatpak / AppImage / Snap environment.
    pub sandboxed: bool,
}

/// Decides the install method from a probe.
pub fn detect(p: &Probe) -> InstallMethod {
    if p.sandboxed {
        return InstallMethod::Unmanaged(Unmanaged::Sandboxed);
    }
    let Some(exe) = p.exe.as_deref() else {
        return InstallMethod::Unmanaged(Unmanaged::Unknown);
    };
    if is_cargo_target(exe) {
        return InstallMethod::Unmanaged(Unmanaged::DevBuild);
    }
    if let (Some(bin), Some(data)) = (&p.script_bin_dir, &p.script_data_dir)
        && exe == bin.join(CRATE)
        && p.version_marker
    {
        return if p.bin_dir_writable {
            InstallMethod::Script(ScriptLayout {
                bin_dir: bin.clone(),
                data_dir: data.clone(),
            })
        } else {
            InstallMethod::Unmanaged(Unmanaged::ReadOnly)
        };
    }
    if p.cargo_bin_dir
        .as_deref()
        .is_some_and(|c| exe.parent() == Some(c))
    {
        return InstallMethod::Cargo;
    }
    const SYSTEM: [&str; 7] = ["/usr", "/opt", "/bin", "/sbin", "/nix", "/snap", "/var/lib"];
    if SYSTEM.iter().any(|s| exe.starts_with(s)) {
        return InstallMethod::Unmanaged(Unmanaged::System);
    }
    InstallMethod::Unmanaged(Unmanaged::Unknown)
}

/// `.../target/<profile>/<exe>` (also `deps/` and target triples).
fn is_cargo_target(exe: &Path) -> bool {
    let Some(dir) = exe.parent() else {
        return false;
    };
    dir.ancestors()
        .skip(1)
        .take(3)
        .any(|a| a.file_name().is_some_and(|n| n == "target"))
}

fn env_dir(var: &str) -> Option<PathBuf> {
    std::env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn canonical(p: PathBuf) -> PathBuf {
    std::fs::canonicalize(&p).unwrap_or(p)
}

/// install.sh's `BIN_DIR` / `DATA_DIR` (`${XDG_BIN_HOME:-$HOME/.local/bin}`,
/// `${XDG_DATA_HOME:-$HOME/.local/share}`), not canonicalized.
pub fn script_dirs() -> Option<(PathBuf, PathBuf)> {
    let home = env_dir("HOME");
    let bin = env_dir("XDG_BIN_HOME").or_else(|| home.as_ref().map(|h| h.join(".local/bin")))?;
    let data =
        env_dir("XDG_DATA_HOME").or_else(|| home.as_ref().map(|h| h.join(".local/share")))?;
    Some((bin, data))
}

/// Reads the environment. Cheap: a few `stat`s, no writes.
pub fn probe() -> Probe {
    let exe = std::env::current_exe().ok().map(canonical);
    let (bin, data) = match script_dirs() {
        Some((b, d)) => (Some(canonical(b)), Some(d)),
        None => (None, None),
    };
    let version_marker = data
        .as_ref()
        .is_some_and(|d| d.join(CRATE).join("installed-version").is_file());
    let bin_dir_writable = bin
        .as_ref()
        .and_then(|b| std::fs::metadata(b).ok())
        .is_some_and(|m| m.is_dir() && !m.permissions().readonly());
    let cargo_bin_dir = env_dir("CARGO_HOME")
        .or_else(|| env_dir("HOME").map(|h| h.join(".cargo")))
        .map(|c| canonical(c.join("bin")));
    let sandboxed = ["FLATPAK_ID", "APPIMAGE", "SNAP"]
        .iter()
        .any(|v| env_dir(v).is_some())
        || Path::new("/.flatpak-info").exists();
    Probe {
        exe,
        script_bin_dir: bin,
        script_data_dir: data,
        version_marker,
        bin_dir_writable,
        cargo_bin_dir,
        sandboxed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn script_probe() -> Probe {
        Probe {
            exe: Some("/home/u/.local/bin/ferrochat".into()),
            script_bin_dir: Some("/home/u/.local/bin".into()),
            script_data_dir: Some("/home/u/.local/share".into()),
            version_marker: true,
            bin_dir_writable: true,
            cargo_bin_dir: Some("/home/u/.cargo/bin".into()),
            sandboxed: false,
        }
    }

    #[test]
    fn install_script_layout_self_updates() {
        let m = detect(&script_probe());
        let InstallMethod::Script(l) = &m else {
            panic!("{m:?}")
        };
        assert!(m.self_updates());
        assert_eq!(
            l.bin("ferrochat-player"),
            Path::new("/home/u/.local/bin/ferrochat-player")
        );
        assert_eq!(
            l.version_file(),
            Path::new("/home/u/.local/share/ferrochat/installed-version")
        );
        assert_eq!(
            l.icon_dir(),
            Path::new("/home/u/.local/share/icons/hicolor/scalable/apps")
        );
    }

    #[test]
    fn script_needs_the_version_marker_and_a_writable_dir() {
        let mut p = script_probe();
        p.version_marker = false;
        assert_eq!(detect(&p), InstallMethod::Unmanaged(Unmanaged::Unknown));
        let mut p = script_probe();
        p.bin_dir_writable = false;
        assert_eq!(detect(&p), InstallMethod::Unmanaged(Unmanaged::ReadOnly));
    }

    #[test]
    fn custom_xdg_bin_home() {
        let mut p = script_probe();
        p.exe = Some("/data/bin/ferrochat".into());
        p.script_bin_dir = Some("/data/bin".into());
        assert!(detect(&p).self_updates());
        // Same name, other folder: not the script's copy.
        p.exe = Some("/elsewhere/ferrochat".into());
        assert_eq!(detect(&p), InstallMethod::Unmanaged(Unmanaged::Unknown));
    }

    #[test]
    fn cargo_install_never_self_updates() {
        let mut p = script_probe();
        p.exe = Some("/home/u/.cargo/bin/ferrochat".into());
        let m = detect(&p);
        assert_eq!(m, InstallMethod::Cargo);
        assert!(!m.self_updates());
        p.exe = Some("/opt/cargo/bin/ferrochat".into());
        p.cargo_bin_dir = Some("/opt/cargo/bin".into());
        assert_eq!(detect(&p), InstallMethod::Cargo);
        assert!(cargo_command().ends_with("Kartudev/Ferrochat-kartu --locked --force"));
    }

    #[test]
    fn dev_builds_system_paths_and_sandboxes_notify_only() {
        let mut p = script_probe();
        for exe in [
            "/home/u/src/Ferrochat/target/debug/ferrochat",
            "/home/u/src/Ferrochat/target/release/ferrochat",
            "/home/u/src/Ferrochat/target/x86_64-unknown-linux-gnu/release/ferrochat",
            "/home/u/src/Ferrochat/target/debug/deps/ferrochat-0123",
        ] {
            p.exe = Some(exe.into());
            assert_eq!(
                detect(&p),
                InstallMethod::Unmanaged(Unmanaged::DevBuild),
                "{exe}"
            );
        }
        p.exe = Some("/usr/bin/ferrochat".into());
        assert_eq!(detect(&p), InstallMethod::Unmanaged(Unmanaged::System));
        p.exe = Some("/opt/ferrochat/ferrochat".into());
        assert_eq!(detect(&p), InstallMethod::Unmanaged(Unmanaged::System));
        let mut p = script_probe();
        p.sandboxed = true;
        assert_eq!(detect(&p), InstallMethod::Unmanaged(Unmanaged::Sandboxed));
        let p = Probe::default();
        assert_eq!(detect(&p), InstallMethod::Unmanaged(Unmanaged::Unknown));
    }
}
