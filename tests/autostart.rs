//! Autostart entry creation and removal through the environment-driven API
//! (`XDG_CONFIG_HOME`). One test only: it mutates the process environment.

#![cfg(not(windows))]

use ferrochat::autostart;

#[test]
fn toggle_writes_and_removes_the_desktop_entry_in_xdg_config_home() {
    let home = std::env::temp_dir().join(format!("ferrochat-xdg-{}", std::process::id()));
    // SAFETY: this test binary has a single test and spawns no threads yet.
    unsafe { std::env::set_var("XDG_CONFIG_HOME", &home) };

    assert!(!autostart::is_enabled());
    autostart::enable().expect("enable");
    assert!(autostart::is_enabled());

    let path = home.join("autostart/dev.ferro.ferrochat.desktop");
    let text = std::fs::read_to_string(&path).expect("entry exists");
    let exec = text
        .lines()
        .find_map(|l| l.strip_prefix("Exec="))
        .expect("Exec line");
    assert!(exec.ends_with(" --minimized"), "Exec was: {exec}");
    assert!(text.contains("Name=Ferrochat"));

    autostart::disable().expect("disable");
    assert!(!path.exists());
    assert!(!autostart::is_enabled());
    autostart::disable().expect("disabling twice is fine");
    let _ = std::fs::remove_dir_all(home);
}
