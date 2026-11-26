//! Entry point: logging, config, `iced::application(..).run()`.
#![deny(clippy::unwrap_used, clippy::expect_used)]
// Windows release builds are GUI programs: no console window behind the app.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::io::Write;

use ferrochat::app::App;
use ferrochat::config::{Cli, Config, USAGE};
use ferrochat::ui::{self, theme};

fn init_logging(cfg: &Config) {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::{EnvFilter, fmt};

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new("info,whatsapp_rust=warn,wacore=warn,iced=warn,wgpu=warn,naga=warn")
    });
    let file = (!cfg.mock)
        .then(|| ferrochat::config::open_log_file(&cfg.state_dir))
        .flatten();
    let registry = tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_writer(std::io::stderr).with_target(false));
    match file {
        Some(f) => registry
            .with(
                fmt::layer()
                    .with_ansi(false)
                    .with_writer(std::sync::Mutex::new(f)),
            )
            .init(),
        None => registry.init(),
    }
}

fn main() -> iced::Result {
    // Default to the CPU renderer: in measurements it idles at ~43 MB RSS
    // versus ~177 MB for wgpu (software Vulkan); ICED_BACKEND=wgpu opts in.
    if std::env::var_os("ICED_BACKEND").is_none() {
        // SAFETY: single-threaded at this point, before any thread is spawned.
        unsafe { std::env::set_var("ICED_BACKEND", "tiny-skia") };
    }
    let cfg = match Config::from_args(std::env::args().skip(1)) {
        Cli::Run(cfg) => *cfg,
        Cli::Help => {
            let _ = std::io::stdout().write_all(USAGE.as_bytes());
            return Ok(());
        }
        Cli::Invalid(msg) => {
            eprintln!("ferrochat: {msg}\n\n{USAGE}");
            std::process::exit(2);
        }
    };
    init_logging(&cfg);

    // One instance per data directory: two clients on one Signal store would
    // corrupt it and make WhatsApp replace each other's stream. A second
    // launch asks the first to show its window and exits.
    let _instance_lock = if cfg.mock {
        None
    } else {
        match ferrochat::config::acquire_instance_lock(&cfg.data_dir) {
            Ok(lock) => {
                if let Err(e) = ferrochat::instance::serve(&cfg.data_dir) {
                    tracing::warn!(error = %e, "single-instance socket unavailable");
                }
                Some(lock)
            }
            Err(ferrochat::config::LockError::AlreadyRunning) => {
                if cfg.minimized {
                    // e.g. autostart while already running: nothing to do.
                    return Ok(());
                }
                match ferrochat::instance::notify_running(&cfg.data_dir) {
                    Ok(()) => {
                        eprintln!("ferrochat: already running; showing its window.");
                        return Ok(());
                    }
                    Err(_) => {
                        eprintln!(
                            "ferrochat: {}",
                            ferrochat::config::LockError::AlreadyRunning
                        );
                        std::process::exit(1);
                    }
                }
            }
            Err(e) => {
                eprintln!("ferrochat: {e}");
                std::process::exit(1);
            }
        }
    };

    // A daemon keeps running with zero windows: closing to the tray frees the
    // window and its renderer while the connection stays up. The main window
    // is opened by `App::boot` (unless `--minimized`).
    let data_dir = cfg.data_dir.clone();
    let mock = cfg.mock;
    // Colour emoji everywhere, the composer included (see `ui::emoji_font`).
    ui::emoji_font::install();
    let result = iced::daemon(move || App::boot(cfg.clone()), App::update, ui::view_window)
        .title(App::title)
        .theme(App::iced_theme)
        .subscription(App::subscription)
        .font(theme::FONT_REGULAR)
        .font(theme::FONT_MEDIUM)
        .font(theme::FONT_SEMIBOLD)
        .default_font(theme::INTER)
        .antialiasing(true)
        .run();
    if !mock {
        ferrochat::instance::cleanup(&data_dir);
    }
    // "Restart now" after an update: the lock and socket are released first,
    // so the new process starts as the first instance.
    drop(_instance_lock);
    ferrochat::update::restart::run_pending();
    result
}
