//! Optional "start at login": an XDG autostart entry
//! `<config home>/autostart/dev.ferro.ferrochat.desktop` whose `Exec` line
//! carries `--minimized`, so the app comes up in the tray without a window.
//! On Windows it is a `Ferrochat` value under
//! `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` instead.
//! Off by default; the settings toggle writes and removes the entry.

use std::path::{Path, PathBuf};

use crate::config::APP_ID;

/// File name of the autostart entry.
pub fn file_name() -> String {
    format!("{APP_ID}.desktop")
}

/// `$XDG_CONFIG_HOME` or `~/.config`.
pub fn config_home() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| directories::BaseDirs::new().map(|d| d.home_dir().join(".config")))
}

/// Where the entry lives under `config_home`.
pub fn entry_path(config_home: &Path) -> PathBuf {
    config_home.join("autostart").join(file_name())
}

/// Quotes one `Exec` argument per the Desktop Entry spec: arguments with
/// spaces or reserved characters go in double quotes, with `"`, `` ` ``,
/// `$` and `\` backslash-escaped (and `%` doubled so it is not a field code).
pub fn exec_quote(arg: &str) -> String {
    let reserved = " \t\n\"'\\><~|&;$*?#()`";
    if arg.is_empty() || arg.contains(|c| reserved.contains(c)) || arg.contains('%') {
        let mut out = String::from("\"");
        for c in arg.chars() {
            match c {
                '"' | '`' | '$' | '\\' => {
                    out.push('\\');
                    out.push(c);
                }
                '%' => out.push_str("%%"),
                c => out.push(c),
            }
        }
        out.push('"');
        out
    } else {
        arg.to_string()
    }
}

/// The text of the autostart entry for the binary at `exe`.
pub fn entry_text(exe: &Path) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName=Ferrochat\n\
         Comment=Unofficial WhatsApp desktop client\n\
         Exec={} --minimized\nTerminal=false\nX-GNOME-Autostart-enabled=true\n\
         StartupNotify=false\n",
        exec_quote(&exe.to_string_lossy())
    )
}

/// Writes the entry under `config_home`.
pub fn enable_in(config_home: &Path, exe: &Path) -> std::io::Result<()> {
    let path = entry_path(config_home);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, entry_text(exe))
}

/// Removes the entry (already gone is fine).
pub fn disable_in(config_home: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(entry_path(config_home)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Whether the entry exists under `config_home`.
pub fn is_enabled_in(config_home: &Path) -> bool {
    entry_path(config_home).is_file()
}

#[cfg(not(windows))]
fn home() -> std::io::Result<PathBuf> {
    config_home().ok_or_else(|| std::io::Error::other("no config directory"))
}

/// Enables autostart for the running executable.
#[cfg(not(windows))]
pub fn enable() -> std::io::Result<()> {
    enable_in(&home()?, &std::env::current_exe()?)
}

/// Disables autostart.
#[cfg(not(windows))]
pub fn disable() -> std::io::Result<()> {
    disable_in(&home()?)
}

/// Whether autostart is on.
#[cfg(not(windows))]
pub fn is_enabled() -> bool {
    config_home().is_some_and(|h| is_enabled_in(&h))
}

/// The command line stored in the Run key: the quoted binary plus
/// `--minimized` (quotes are not allowed in Windows file names).
pub fn run_command(exe: &Path) -> String {
    format!("\"{}\" --minimized", exe.display())
}

/// Name of the Run value.
pub const RUN_VALUE: &str = "Ferrochat";

#[cfg(windows)]
pub use windows::{disable, enable, is_enabled};

/// The Run key, through the registry API (no `reg.exe` console window).
#[cfg(windows)]
pub mod windows {
    use std::path::Path;

    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows_sys::Win32::System::Registry::{
        HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
    };

    use super::{RUN_VALUE, run_command};

    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn check(code: u32) -> std::io::Result<()> {
        if code == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(std::io::Error::from_raw_os_error(code as i32))
        }
    }

    /// Writes value `name` = `"<exe>" --minimized`.
    pub fn enable_value(name: &str, exe: &Path) -> std::io::Result<()> {
        let key = wide(RUN_KEY);
        let name = wide(name);
        let data = wide(&run_command(exe));
        let bytes = u32::try_from(data.len() * 2).map_err(std::io::Error::other)?;
        // SAFETY: all pointers are to live, NUL-terminated UTF-16 buffers and
        // `bytes` is the exact size of `data` in bytes.
        check(unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                name.as_ptr(),
                REG_SZ,
                data.as_ptr().cast(),
                bytes,
            )
        })
    }

    /// Removes value `name` (already gone is fine).
    pub fn disable_value(name: &str) -> std::io::Result<()> {
        let key = wide(RUN_KEY);
        let name = wide(name);
        // SAFETY: both pointers are live NUL-terminated UTF-16 strings.
        let code = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr()) };
        if code == ERROR_FILE_NOT_FOUND {
            return Ok(());
        }
        check(code)
    }

    /// Whether value `name` exists (any string).
    pub fn value_exists(name: &str) -> bool {
        let key = wide(RUN_KEY);
        let name = wide(name);
        let mut size = 0u32;
        // SAFETY: a null data pointer only asks for the size.
        let code = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut size,
            )
        };
        code == ERROR_SUCCESS
    }

    /// Enables autostart for the running executable.
    pub fn enable() -> std::io::Result<()> {
        enable_value(RUN_VALUE, &std::env::current_exe()?)
    }

    /// Disables autostart.
    pub fn disable() -> std::io::Result<()> {
        disable_value(RUN_VALUE)
    }

    /// Whether autostart is on.
    pub fn is_enabled() -> bool {
        value_exists(RUN_VALUE)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn run_value_round_trip() {
            let name = format!("FerrochatTest{}", std::process::id());
            assert!(!value_exists(&name));
            enable_value(
                &name,
                Path::new(r"C:\Program Files\Ferrochat\ferrochat.exe"),
            )
            .unwrap();
            assert!(value_exists(&name));
            disable_value(&name).unwrap();
            assert!(!value_exists(&name));
            disable_value(&name).unwrap();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting() {
        assert_eq!(exec_quote("/usr/bin/ferrochat"), "/usr/bin/ferrochat");
        assert_eq!(
            exec_quote("/opt/My App/ferrochat"),
            "\"/opt/My App/ferrochat\""
        );
        assert_eq!(exec_quote("/a/$b"), "\"/a/\\$b\"");
        assert_eq!(exec_quote("/a/100%"), "\"/a/100%%\"");
    }

    #[test]
    fn run_command_quotes_the_path() {
        assert_eq!(
            run_command(Path::new(r"C:\Program Files\Ferrochat\ferrochat.exe")),
            r#""C:\Program Files\Ferrochat\ferrochat.exe" --minimized"#
        );
    }

    #[test]
    fn entry_has_minimized_exec() {
        let t = entry_text(Path::new("/usr/bin/ferrochat"));
        assert!(t.contains("Exec=/usr/bin/ferrochat --minimized\n"));
        assert!(t.starts_with("[Desktop Entry]\n"));
    }

    #[test]
    fn create_and_remove_in_a_temp_config_home() {
        let home = std::env::temp_dir().join(format!("ferrochat-autostart-{}", std::process::id()));
        assert!(!is_enabled_in(&home));
        enable_in(&home, Path::new("/x/ferrochat")).unwrap();
        assert!(is_enabled_in(&home));
        assert!(entry_path(&home).ends_with("autostart/dev.ferro.ferrochat.desktop"));
        disable_in(&home).unwrap();
        assert!(!is_enabled_in(&home));
        disable_in(&home).unwrap();
        let _ = std::fs::remove_dir_all(home);
    }
}
