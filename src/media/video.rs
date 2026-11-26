//! In-app video playback through the `ferrochat-player` helper process.
//!
//! The helper (OpenH264 + symphonia, no system codecs) decodes one file and
//! streams RGBA frames; this
//! module spawns it, parses its output on a reader thread and forwards
//! [`Event`]s over a channel. One [`Player`] exists at a time (the app keeps
//! it in the open viewer); dropping it ends the process, which releases the
//! decoder, its buffers and the audio stream.

use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Mutex;

use tokio::sync::mpsc;

use super::video_proto::{self as proto, Cmd, Msg, PlayState};

/// Name of the helper executable.
pub const HELPER_NAME: &str = "ferrochat-player";

/// The helper's file name on disk (`ferrochat-player.exe` on Windows).
#[cfg(windows)]
pub const HELPER_FILE: &str = "ferrochat-player.exe";
/// The helper's file name on disk (`ferrochat-player.exe` on Windows).
#[cfg(not(windows))]
pub const HELPER_FILE: &str = HELPER_NAME;

/// Windows `CREATE_NO_WINDOW`: the helper is a console program, and without
/// this flag every video would flash a console window over the app.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// What went wrong, in terms the user can act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// The helper is missing or could not start.
    Unavailable(String),
    /// The video uses a codec the built-in decoder does not handle.
    MissingCodec(String),
    /// The file could not be played.
    Broken(String),
}

impl Failure {
    /// The text shown in the viewer: which step failed, never a crash, never
    /// silent. Path-like words are removed from the helper's detail.
    pub fn user_message(&self) -> String {
        match self {
            Failure::Unavailable(d) => format!(
                "The video player could not start ({}). Choose \u{201c}Open externally\u{201d} \
                 to use another player.",
                scrub(d)
            ),
            Failure::MissingCodec(what) => format!(
                "This video uses {}, which the built-in player cannot decode. Choose \
                 \u{201c}Open externally\u{201d} to play it with another player.",
                scrub(what)
            ),
            Failure::Broken(d) => format!(
                "This video could not be played: {}. Choose \u{201c}Open externally\u{201d} \
                 to try another player.",
                scrub(d)
            ),
        }
    }

    /// Short detail for logs (no paths).
    pub fn detail(&self) -> String {
        match self {
            Failure::Unavailable(d) | Failure::MissingCodec(d) | Failure::Broken(d) => scrub(d),
        }
    }
}

/// Drops words that look like file paths and caps the length.
fn scrub(s: &str) -> String {
    let words: Vec<&str> = s
        .split_whitespace()
        .filter(|w| !w.contains('/') && !w.contains('\\'))
        .collect();
    let mut out: String = words.join(" ").chars().take(160).collect();
    if out.is_empty() {
        out = "unknown error".into();
    }
    out
}

/// What the helper reports.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Playback started; size and duration are known.
    Ready {
        /// Frame width (0 for audio-only files).
        width: u32,
        /// Frame height.
        height: u32,
        /// Duration in ms, 0 when unknown.
        duration_ms: u64,
    },
    /// A decoded frame (RGBA).
    Frame {
        /// Width.
        width: u32,
        /// Height.
        height: u32,
        /// Pixels.
        rgba: Vec<u8>,
    },
    /// Playback position and state.
    Time {
        /// Position in ms.
        position_ms: u64,
        /// Duration in ms (0 = unknown).
        duration_ms: u64,
        /// State.
        state: PlayState,
    },
    /// Playback failed; no more events follow.
    Failed(Failure),
}

/// The helper inside `dir`, if it is there.
pub fn helper_in(dir: &Path) -> Option<PathBuf> {
    let p = dir.join(HELPER_FILE);
    p.is_file().then_some(p)
}

/// The helper executable: next to this binary, else on `PATH`.
pub fn helper_path() -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        if let Some(p) = helper_in(dir) {
            return Some(p);
        }
        // `cargo test` runs from target/debug/deps.
        if let Some(p) = dir.parent().and_then(helper_in) {
            return Some(p);
        }
        // Newer Cargo (build-dir layout, Rust 1.100+) runs unit tests from
        // target/debug/build/<pkg>/<hash>/out: the helper is a few levels up.
        #[cfg(test)]
        if let Some(p) = dir.ancestors().skip(2).take(4).find_map(helper_in) {
            return Some(p);
        }
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(HELPER_FILE))
        .find(|p| p.is_file())
}

/// A running helper. Dropping it stops playback and frees the decoder.
pub struct Player {
    child: Mutex<Option<Child>>,
    stdin: Mutex<Option<ChildStdin>>,
}

impl std::fmt::Debug for Player {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Player")
    }
}

/// Starts `helper` on `file`. Events arrive on the returned channel; the
/// channel closes after the last event.
pub fn spawn_with(
    helper: &Path,
    file: &Path,
) -> Result<(Player, mpsc::UnboundedReceiver<Event>), Failure> {
    let mut cmd = Command::new(helper);
    cmd.arg(file)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| Failure::Unavailable(format!("cannot start the video helper: {e}")))?;
    let (Some(stdout), Some(stderr), stdin) =
        (child.stdout.take(), child.stderr.take(), child.stdin.take())
    else {
        let _ = child.kill();
        return Err(Failure::Unavailable("no pipes to the video helper".into()));
    };
    let (tx, rx) = mpsc::unbounded_channel();
    // stderr must be drained: a chatty helper with a full pipe would
    // stall the helper. Only the last line is kept (for load failures).
    let last_err = std::sync::Arc::new(Mutex::new(String::new()));
    {
        let last_err = last_err.clone();
        std::thread::spawn(move || {
            use std::io::BufRead;
            for line in BufReader::new(stderr.take(1 << 20)).lines() {
                let Ok(line) = line else { break };
                if let Ok(mut g) = last_err.lock() {
                    *g = line.chars().take(300).collect();
                }
            }
        });
    }
    std::thread::spawn(move || {
        let mut reader = BufReader::with_capacity(1 << 16, stdout);
        let mut announced = false;
        #[allow(clippy::while_let_loop)]
        loop {
            match proto::read_msg(&mut reader) {
                Ok(Some(msg)) => {
                    let ev = match msg {
                        Msg::Ready {
                            width,
                            height,
                            duration_ms,
                        } => {
                            announced = true;
                            Event::Ready {
                                width,
                                height,
                                duration_ms,
                            }
                        }
                        Msg::Frame {
                            width,
                            height,
                            rgba,
                        } => Event::Frame {
                            width,
                            height,
                            rgba,
                        },
                        Msg::Time {
                            position_ms,
                            duration_ms,
                            state,
                        } => Event::Time {
                            position_ms,
                            duration_ms,
                            state,
                        },
                        Msg::Failed { missing, text } => {
                            announced = true;
                            Event::Failed(if missing {
                                Failure::MissingCodec(text)
                            } else {
                                Failure::Broken(text)
                            })
                        }
                    };
                    if tx.send(ev).is_err() {
                        return;
                    }
                }
                Ok(None) | Err(_) => break,
            }
        }
        if !announced {
            // The helper died before saying anything: the dynamic loader
            // refused it, or it crashed on start.
            // Give the stderr thread a moment to see the loader's message.
            std::thread::sleep(std::time::Duration::from_millis(100));
            let line = last_err
                .lock()
                .map(|g| g.clone())
                .ok()
                .filter(|l| !l.is_empty())
                .unwrap_or_else(|| "the helper exited".into());
            let _ = tx.send(Event::Failed(Failure::Unavailable(line)));
        }
    });
    Ok((
        Player {
            child: Mutex::new(Some(child)),
            stdin: Mutex::new(stdin),
        },
        rx,
    ))
}

/// What the helper read from a file before its first picture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    /// Frame width as decoded.
    pub width: u32,
    /// Frame height.
    pub height: u32,
    /// Duration in ms, 0 when the container does not say.
    pub duration_ms: u64,
    /// The first picture (RGBA, possibly scaled down by the helper).
    pub frame: (u32, u32, Vec<u8>),
}

/// Decodes only the first picture of `file` with the helper at `helper`
/// (a poster frame, or the probe of a video about to be sent), muted, then
/// stops it. Gives up after `wait`.
pub async fn probe_with(
    helper: &Path,
    file: &Path,
    wait: std::time::Duration,
) -> Result<Probe, Failure> {
    let (player, mut rx) = spawn_with(helper, file)?;
    // Probing must never make a sound.
    player.send(Cmd::Mute(true));
    let deadline = tokio::time::Instant::now() + wait;
    let mut size = (0, 0);
    let mut duration_ms = 0;
    let out = loop {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Some(Event::Ready {
                width,
                height,
                duration_ms: d,
            })) => {
                size = (width, height);
                duration_ms = d;
            }
            Ok(Some(Event::Frame {
                width,
                height,
                rgba,
            })) => {
                if size == (0, 0) {
                    size = (width, height);
                }
                break Ok(Probe {
                    width: size.0,
                    height: size.1,
                    duration_ms,
                    frame: (width, height, rgba),
                });
            }
            Ok(Some(Event::Failed(f))) => break Err(f),
            Ok(Some(_)) => {}
            Ok(None) => break Err(Failure::Broken("the helper ended without a picture".into())),
            Err(_) => break Err(Failure::Broken("no picture in time".into())),
        }
    };
    // Dropping the player ends the helper process.
    drop(player);
    out
}

/// The first picture of `file` (a poster frame), see [`probe_with`].
pub async fn first_frame_with(
    helper: &Path,
    file: &Path,
    wait: std::time::Duration,
) -> Result<(u32, u32, Vec<u8>), Failure> {
    probe_with(helper, file, wait).await.map(|p| p.frame)
}

fn find_helper() -> Result<PathBuf, Failure> {
    helper_path().ok_or_else(|| {
        Failure::Unavailable(format!(
            "the {HELPER_NAME} helper was not found next to Ferrochat"
        ))
    })
}

/// [`first_frame_with`] using the helper found by [`helper_path`].
pub async fn first_frame(
    file: &Path,
    wait: std::time::Duration,
) -> Result<(u32, u32, Vec<u8>), Failure> {
    first_frame_with(&find_helper()?, file, wait).await
}

/// [`probe_with`] using the helper found by [`helper_path`].
pub async fn probe(file: &Path, wait: std::time::Duration) -> Result<Probe, Failure> {
    probe_with(&find_helper()?, file, wait).await
}

/// Flag that turns the helper into a GIF → MP4 converter.
pub const GIF_TO_MP4_FLAG: &str = "--gif-to-mp4";

/// Converts an animated GIF into a silent H.264 MP4 the way WhatsApp sends
/// GIFs, with the helper at `helper` (the main binary links no encoder).
/// Gives up after `wait`.
pub async fn gif_to_mp4_with(
    helper: &Path,
    gif: &Path,
    mp4: &Path,
    wait: std::time::Duration,
) -> Result<(), Failure> {
    let mut cmd = tokio::process::Command::new(helper);
    cmd.arg(GIF_TO_MP4_FLAG)
        .arg(gif)
        .arg(mp4)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    let child = cmd
        .spawn()
        .map_err(|e| Failure::Unavailable(format!("cannot start the video helper: {e}")))?;
    let out = tokio::time::timeout(wait, child.wait_with_output())
        .await
        .map_err(|_| Failure::Broken("the GIF took too long to convert".into()))?
        .map_err(|e| Failure::Broken(e.to_string()))?;
    if out.status.success() {
        return Ok(());
    }
    let line = String::from_utf8_lossy(&out.stderr)
        .lines()
        .last()
        .unwrap_or("the converter failed")
        .chars()
        .take(300)
        .collect::<String>();
    Err(Failure::Broken(line))
}

/// [`gif_to_mp4_with`] using the helper found by [`helper_path`].
pub async fn gif_to_mp4(gif: &Path, mp4: &Path, wait: std::time::Duration) -> Result<(), Failure> {
    gif_to_mp4_with(&find_helper()?, gif, mp4, wait).await
}

/// Starts the helper found by [`helper_path`].
pub fn spawn(file: &Path) -> Result<(Player, mpsc::UnboundedReceiver<Event>), Failure> {
    let helper = helper_path().ok_or_else(|| {
        Failure::Unavailable(format!(
            "the {HELPER_NAME} helper was not found next to Ferrochat"
        ))
    })?;
    spawn_with(&helper, file)
}

impl Player {
    /// Sends a command; errors (the helper already exited) are ignored.
    pub fn send(&self, cmd: Cmd) {
        if let Ok(mut g) = self.stdin.lock()
            && let Some(s) = g.as_mut()
            && (s.write_all(cmd.to_line().as_bytes()).is_err() || s.flush().is_err())
        {
            *g = None;
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.send(Cmd::Quit);
        if let Ok(mut g) = self.stdin.lock() {
            *g = None;
        }
        let child = self.child.lock().ok().and_then(|mut c| c.take());
        if let Some(mut child) = child {
            // Give it a moment to exit cleanly, then make sure it is gone;
            // reaping happens off the UI thread.
            std::thread::spawn(move || {
                for _ in 0..20 {
                    if matches!(child.try_wait(), Ok(Some(_))) {
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(25));
                }
                let _ = child.kill();
                let _ = child.wait();
            });
        }
    }
}

/// `65_000` ms -> `1:05`; `3_725_000` -> `1:02:05`.
pub fn format_clock(ms: u64) -> String {
    let s = ms / 1000;
    let (h, m, sec) = (s / 3600, (s / 60) % 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{sec:02}")
    } else {
        format!("{m}:{sec:02}")
    }
}

/// Seek target for a slider fraction in `0.0..=1.0`.
pub fn seek_target(fraction: f32, duration_ms: u64) -> u64 {
    (f64::from(fraction.clamp(0.0, 1.0)) * duration_ms as f64) as u64
}

/// The largest rectangle of aspect `w:h` inside `avail_w x avail_h`.
pub fn fit_inside(w: u32, h: u32, avail_w: f32, avail_h: f32) -> (f32, f32) {
    if w == 0 || h == 0 || avail_w <= 0.0 || avail_h <= 0.0 {
        return (avail_w.max(0.0), avail_h.max(0.0));
    }
    let scale = (avail_w / w as f32).min(avail_h / h as f32);
    (w as f32 * scale, h as f32 * scale)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_formats() {
        assert_eq!(format_clock(0), "0:00");
        assert_eq!(format_clock(65_000), "1:05");
        assert_eq!(format_clock(3_725_000), "1:02:05");
        assert_eq!(format_clock(999), "0:00");
    }

    #[test]
    fn seek_fraction_is_clamped() {
        assert_eq!(seek_target(0.5, 10_000), 5_000);
        assert_eq!(seek_target(-1.0, 10_000), 0);
        assert_eq!(seek_target(7.0, 10_000), 10_000);
        assert_eq!(seek_target(0.5, 0), 0);
    }

    #[test]
    fn fit_keeps_the_aspect_ratio() {
        assert_eq!(fit_inside(1920, 1080, 960.0, 960.0), (960.0, 540.0));
        assert_eq!(fit_inside(1080, 1920, 960.0, 540.0), (303.75, 540.0));
        assert_eq!(fit_inside(0, 0, 100.0, 50.0), (100.0, 50.0));
    }

    #[test]
    fn failures_say_which_step_failed() {
        let m = Failure::Unavailable("the helper exited".into()).user_message();
        assert!(m.contains("could not start (the helper exited)") && m.contains("Open externally"));
        let m = Failure::MissingCodec("H.265 (HEVC)".into()).user_message();
        assert!(m.contains("uses H.265 (HEVC)") && m.contains("cannot decode"));
        let m = Failure::Broken("H.264 decoding failed at 1.0 s: bitstream".into()).user_message();
        assert!(m.contains("H.264 decoding failed at 1.0 s"));
        // Paths (which may name files) are never shown.
        let m = Failure::Broken("cannot open /secret/path now".into()).user_message();
        assert!(!m.contains("secret") && m.contains("cannot open now"));
        assert!(
            Failure::Broken(String::new())
                .user_message()
                .contains("unknown error")
        );
    }

    #[test]
    fn a_missing_helper_is_a_clear_failure_not_a_crash() {
        let r = spawn_with(
            Path::new("/nonexistent/ferrochat-player"),
            Path::new("/tmp/x"),
        );
        assert!(matches!(r, Err(Failure::Unavailable(_))));
    }
}
