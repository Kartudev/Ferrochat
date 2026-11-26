//! The video helper against real files (the mock clip is a 3 s H.264 High
//! profile + AAC MP4 with B-frames, like a phone video)
//! and the app-side `Player` against scripted fake helpers. No network, no
//! window: frames are just bytes.

use std::path::{Path, PathBuf};
use std::time::Duration;

use ferrochat::media::video::{self, Event, Failure, Player, spawn_with};
use ferrochat::media::video_proto::{Cmd, PlayState};
use tokio::sync::mpsc::UnboundedReceiver;

const HELPER: &str = env!("CARGO_BIN_EXE_ferrochat-player");
const WAIT: Duration = Duration::from_secs(20);

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ferrochat-vt-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn clip() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/mock-clip.mp4")
}

async fn next(rx: &mut UnboundedReceiver<Event>) -> Option<Event> {
    tokio::time::timeout(WAIT, rx.recv()).await.ok().flatten()
}

#[tokio::test]
async fn the_helper_plays_a_real_clip_and_reports_everything() {
    let (player, mut rx) = spawn_with(Path::new(HELPER), &clip()).expect("spawn");
    let mut ready = None;
    let mut frames = 0;
    let mut last_pos = 0;
    let mut ended = false;
    while let Some(ev) = next(&mut rx).await {
        match ev {
            Event::Ready {
                width,
                height,
                duration_ms,
            } => ready = Some((width, height, duration_ms)),
            Event::Frame {
                width,
                height,
                rgba,
            } => {
                assert_eq!((width, height), (320, 180));
                assert_eq!(rgba.len(), 320 * 180 * 4);
                frames += 1;
            }
            Event::Time {
                position_ms, state, ..
            } => {
                last_pos = last_pos.max(position_ms);
                if state == PlayState::Ended {
                    ended = true;
                    break;
                }
            }
            Event::Failed(f) => panic!("playback failed: {f:?}"),
        }
    }
    let (w, h, dur) = ready.expect("ready message");
    assert_eq!((w, h), (320, 180));
    assert!((2500..=3500).contains(&dur), "duration {dur}");
    assert!(frames >= 10, "only {frames} frames");
    assert!(ended, "never reached the end");
    assert!(last_pos >= 2500, "position stopped at {last_pos}");

    // Replaying from the end works, and a seek lands where asked.
    player.send(Cmd::Play);
    let mut replayed = false;
    while let Some(ev) = next(&mut rx).await {
        if let Event::Time {
            position_ms, state, ..
        } = ev
            && state == PlayState::Playing
            && position_ms < 1500
        {
            replayed = true;
            break;
        }
    }
    assert!(replayed, "Play after the end did not restart");
    player.send(Cmd::Pause);
    player.send(Cmd::Seek(2000));
    let mut sought = false;
    while let Some(ev) = next(&mut rx).await {
        if let Event::Time {
            position_ms, state, ..
        } = ev
            && state == PlayState::Paused
            && (1700..=2300).contains(&position_ms)
        {
            sought = true;
            break;
        }
    }
    assert!(sought, "seek did not land near 2 s");

    // Closing frees the decoder: the helper exits and the channel closes.
    drop(player);
    let closed = tokio::time::timeout(Duration::from_secs(5), async {
        while rx.recv().await.is_some() {}
    })
    .await;
    assert!(closed.is_ok(), "the helper outlived its player");
}

#[tokio::test]
async fn a_broken_file_fails_cleanly() {
    let dir = tmp("broken");
    let file = dir.join("broken.mp4");
    std::fs::write(
        &file,
        b"this is not a video at all, just text padding".repeat(8),
    )
    .unwrap();
    let (_player, mut rx) = spawn_with(Path::new(HELPER), &file).expect("spawn");
    match next(&mut rx).await {
        Some(Event::Failed(Failure::Broken(_) | Failure::MissingCodec(_))) => {}
        other => panic!("expected a clean failure, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_missing_file_fails_cleanly() {
    let (_player, mut rx) =
        spawn_with(Path::new(HELPER), Path::new("/nonexistent/clip.mp4")).expect("spawn");
    assert!(matches!(next(&mut rx).await, Some(Event::Failed(_))));
}

/// Writes an executable shell script acting as the helper.
#[cfg(unix)]
fn script(dir: &Path, body: &str) -> PathBuf {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    let p = dir.join("helper.sh");
    let tmp = dir.join("helper.sh.tmp");
    let mut f = std::fs::File::create(&tmp).unwrap();
    f.write_all(format!("#!/bin/sh\n{body}\n").as_bytes())
        .unwrap();
    f.set_permissions(std::fs::Permissions::from_mode(0o755))
        .unwrap();
    f.sync_all().unwrap();
    drop(f);
    std::fs::rename(&tmp, &p).unwrap();
    p
}

/// `spawn_with` for scripted helpers. Another test thread can fork while our
/// script's write fd is still open, which makes exec fail with ETXTBSY (os
/// error 26) for a moment; retry that only, here and not in production.
#[cfg(unix)]
fn spawn_script(helper: &Path, file: &Path) -> Result<(Player, UnboundedReceiver<Event>), Failure> {
    let mut last = None;
    for _ in 0..20 {
        match spawn_with(helper, file) {
            Err(Failure::Unavailable(m)) if m.contains("os error 26") => {
                last = Some(Failure::Unavailable(m));
                std::thread::sleep(Duration::from_millis(50));
            }
            other => return other,
        }
    }
    Err(last.unwrap())
}

#[tokio::test]
#[cfg(unix)]
async fn player_parses_helper_output_and_delivers_commands() {
    let dir = tmp("script");
    // Ready(2x1, 1000 ms), a 2x1 frame, a time message, then record stdin.
    let helper = script(
        &dir,
        r#"printf 'R\002\000\000\000\001\000\000\000\350\003\000\000\000\000\000\000'
printf 'F\002\000\000\000\001\000\000\000\001\002\003\004\005\006\007\010'
printf 'T\364\001\000\000\000\000\000\000\350\003\000\000\000\000\000\000\001'
cat > "$1.cmds""#,
    );
    let target = dir.join("clip.mp4");
    let (player, mut rx) = spawn_script(&helper, &target).expect("spawn");
    assert_eq!(
        next(&mut rx).await,
        Some(Event::Ready {
            width: 2,
            height: 1,
            duration_ms: 1000
        })
    );
    assert_eq!(
        next(&mut rx).await,
        Some(Event::Frame {
            width: 2,
            height: 1,
            rgba: vec![1, 2, 3, 4, 5, 6, 7, 8]
        })
    );
    assert_eq!(
        next(&mut rx).await,
        Some(Event::Time {
            position_ms: 500,
            duration_ms: 1000,
            state: PlayState::Playing
        })
    );
    player.send(Cmd::Pause);
    player.send(Cmd::Seek(750));
    player.send(Cmd::Volume(40));
    player.send(Cmd::Mute(true));
    drop(player);
    // The script exits once stdin closes, which ends the stream.
    assert!(next(&mut rx).await.is_none());
    let cmds = std::fs::read_to_string(dir.join("clip.mp4.cmds")).unwrap();
    assert_eq!(cmds, "pause\nseek 750\nvolume 40\nmute 1\nquit\n");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
#[cfg(unix)]
async fn a_helper_the_loader_refuses_is_reported_with_the_loader_message() {
    let dir = tmp("noloader");
    let helper = script(
        &dir,
        "echo 'ferrochat-player: error while loading shared libraries: libfoo.so.1: cannot open shared object file' >&2; exit 127",
    );
    let (_player, mut rx) = spawn_script(&helper, &dir.join("x.mp4")).expect("spawn");
    match next(&mut rx).await {
        Some(Event::Failed(f @ Failure::Unavailable(_))) => {
            let msg = f.user_message();
            assert!(msg.contains("libfoo.so.1"), "{msg}");
            assert!(msg.contains("Open externally"));
        }
        other => panic!("{other:?}"),
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_portrait_phone_video_is_shown_upright() {
    // 160x96 stored, 90° display matrix (how phones record portrait).
    let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rotated-portrait.mp4");
    let (_player, mut rx) = spawn_with(Path::new(HELPER), &clip).expect("spawn");
    match next(&mut rx).await {
        Some(Event::Ready { width, height, .. }) => assert_eq!((width, height), (96, 160)),
        other => panic!("{other:?}"),
    }
    match next(&mut rx).await {
        Some(Event::Frame {
            width,
            height,
            rgba,
        }) => {
            assert_eq!((width, height), (96, 160));
            assert_eq!(rgba.len(), 96 * 160 * 4);
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn an_unsupported_codec_is_named() {
    // A WebM/VP8 file is not MP4: the failure says so instead of "GStreamer".
    let dir = tmp("webm");
    let file = dir.join("clip.webm");
    std::fs::write(
        &file,
        [0x1A, 0x45, 0xDF, 0xA3, 0x9F, 0x42, 0x86, 0x81, 0x01].repeat(20),
    )
    .unwrap();
    let (_player, mut rx) = spawn_with(Path::new(HELPER), &file).expect("spawn");
    match next(&mut rx).await {
        Some(Event::Failed(f)) => {
            let m = f.user_message();
            assert!(m.contains("MP4") || m.contains("cannot decode"), "{m}");
        }
        other => panic!("{other:?}"),
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_helper_is_found_in_its_directory() {
    let dir = Path::new(HELPER).parent().unwrap();
    assert_eq!(video::helper_in(dir).as_deref(), Some(Path::new(HELPER)));
    assert!(video::helper_in(Path::new("/nonexistent")).is_none());
}

#[allow(dead_code)]
fn _player_is_send(p: Player) -> impl Send {
    p
}

/// A video bubble without an inline thumbnail gets its poster from the
/// first decoded picture, scaled and JPEG-encoded like a thumbnail.
#[tokio::test]
async fn the_first_frame_makes_a_poster() {
    let (w, h, rgba) = video::first_frame_with(Path::new(HELPER), &clip(), WAIT)
        .await
        .expect("first frame");
    assert!(w > 0 && h > 0);
    assert_eq!(rgba.len(), (w * h * 4) as usize);
    let jpeg = ferrochat::media::image::poster_jpeg(w, h, rgba).expect("encode");
    let img = image::load_from_memory(&jpeg).expect("decodable poster");
    assert!(img.width() <= ferrochat::media::image::POSTER_SIDE);
    assert!(img.height() <= ferrochat::media::image::POSTER_SIDE);
    // A file that is not a video fails with a reason, never hangs.
    let dir = tmp("poster");
    let bad = dir.join("x.mp4");
    std::fs::write(&bad, b"not a video").unwrap();
    assert!(
        video::first_frame_with(Path::new(HELPER), &bad, WAIT)
            .await
            .is_err()
    );
}

/// A small animated GIF: `frames` frames of `w`x`h`, `delay_ms` each, with a
/// moving bar and a transparent corner.
fn write_gif(path: &Path, w: u32, h: u32, frames: u32, delay_ms: u32) {
    use image::codecs::gif::{GifEncoder, Repeat};
    use image::{Delay, Frame, Rgba, RgbaImage};
    let file = std::fs::File::create(path).unwrap();
    let mut enc = GifEncoder::new(file);
    enc.set_repeat(Repeat::Infinite).unwrap();
    for i in 0..frames {
        let img = RgbaImage::from_fn(w, h, |x, y| {
            if x < 4 && y < 4 {
                Rgba([0, 0, 0, 0])
            } else if x / 8 == i % (w / 8) {
                Rgba([220, 40, 40, 255])
            } else {
                Rgba([20, 90, 200, 255])
            }
        });
        let frame = Frame::from_parts(img, 0, 0, Delay::from_numer_denom_ms(delay_ms, 1));
        enc.encode_frame(frame).unwrap();
    }
}

#[tokio::test]
async fn a_gif_becomes_a_playable_silent_mp4() {
    let dir = tmp("gif");
    let gif = dir.join("in.gif");
    let mp4 = dir.join("out.mp4");
    // Odd sizes on purpose: the MP4 needs even ones.
    write_gif(&gif, 101, 61, 12, 80);
    video::gif_to_mp4_with(Path::new(HELPER), &gif, &mp4, WAIT)
        .await
        .expect("converted");
    let probe = video::probe_with(Path::new(HELPER), &mp4, WAIT)
        .await
        .expect("the converted file plays");
    assert_eq!((probe.width, probe.height), (100, 60));
    assert!(
        (800..=1100).contains(&probe.duration_ms),
        "duration {}",
        probe.duration_ms
    );
    assert_eq!(probe.frame.2.len(), 100 * 60 * 4);
    // The transparent corner is white, the rest keeps its colour.
    let px = |x: usize, y: usize| {
        let i = (y * 100 + x) * 4;
        probe.frame.2[i..i + 3].to_vec()
    };
    assert!(px(1, 1).iter().all(|&c| c > 200), "corner {:?}", px(1, 1));
    let blue = px(50, 50);
    assert!(blue[2] > 150 && blue[0] < 90, "body {blue:?}");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_broken_gif_fails_cleanly_and_leaves_no_file() {
    let dir = tmp("badgif");
    let gif = dir.join("in.gif");
    let mp4 = dir.join("out.mp4");
    std::fs::write(&gif, b"GIF89a not really").unwrap();
    let err = video::gif_to_mp4_with(Path::new(HELPER), &gif, &mp4, WAIT)
        .await
        .unwrap_err();
    assert!(matches!(err, Failure::Broken(_)), "{err:?}");
    assert!(!mp4.exists());
    let _ = std::fs::remove_dir_all(dir);
}
