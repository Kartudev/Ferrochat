//! App-level tests of sending media: videos and GIFs from the attachment
//! preview, and files dropped on the window (real `App` + fake client; the
//! video tests run the real `ferrochat-player` helper).

use std::path::{Path, PathBuf};

use super::Ui;
use super::drop::DropMsg;
use super::message::{Done, Message};
use super::paste;
use super::test_support::Harness;
use crate::net::fake::Sent;
use crate::net::{Content, OutgoingKind};

const PABLO: &str = "34655555555@s.whatsapp.net";

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ferrochat-mf-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn clip() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/mock-clip.mp4")
}

fn sent_media(h: &Harness) -> Vec<Content> {
    h.fake
        .log()
        .into_iter()
        .filter_map(|s| match s {
            Sent::Media { content, .. } => Some(content),
            _ => None,
        })
        .collect()
}

async fn stage(h: &mut Harness, paths: Vec<PathBuf>) {
    let staged = paste::stage_paths(paths, false).await.map(Box::new);
    let t = h.app.update(Message::done(Done::Staged {
        chat: PABLO.into(),
        staged,
    }));
    h.pump(t).await;
}

#[cfg(feature = "video")]
#[tokio::test]
async fn an_mp4_is_sent_as_a_video_with_its_duration_and_thumbnail() {
    let mut h = Harness::new().await;
    h.ui(Ui::OpenChat(PABLO.into())).await;
    stage(&mut h, vec![clip()]).await;
    let preview = h.app.attach.as_ref().expect("preview open");
    assert!(preview.handle.is_some(), "first frame shown in the preview");
    assert!(matches!(
        preview.items[0].kind,
        OutgoingKind::Video {
            seconds: 3,
            width: 320,
            height: 180,
            gif: false,
            ..
        }
    ));
    h.ui(Ui::AttachCaption("el viaje".into())).await;
    h.ui(Ui::AttachSend).await;
    let sent = sent_media(&h);
    assert_eq!(sent.len(), 1);
    match &sent[0] {
        Content::Video {
            caption,
            seconds,
            thumbnail,
            media,
            gif,
            ..
        } => {
            assert_eq!(caption.as_deref(), Some("el viaje"));
            assert_eq!(*seconds, 3);
            assert!(thumbnail.as_ref().is_some_and(|t| !t.is_empty()));
            assert_eq!(media.as_ref().unwrap().mimetype, "video/mp4");
            assert!(!gif);
        }
        other => panic!("not a video: {other:?}"),
    }
    // The bubble is a video with its poster.
    let last = h
        .app
        .open
        .as_ref()
        .unwrap()
        .messages
        .back()
        .unwrap()
        .clone();
    assert!(matches!(last.content, Content::Video { .. }));
    assert!(h.app.thumbs.contains_key(&last.id));
}

#[cfg(feature = "video")]
#[tokio::test]
async fn a_gif_is_converted_and_sent_as_a_looping_gif() {
    let dir = tmp("gif");
    let gif = dir.join("baile.gif");
    std::fs::write(&gif, crate::mock::sample_gif()).unwrap();
    let mut h = Harness::new().await;
    h.ui(Ui::OpenChat(PABLO.into())).await;
    stage(&mut h, vec![gif]).await;
    let preview = h.app.attach.as_ref().expect("preview open");
    assert_eq!(preview.items[0].file_name, "baile.mp4");
    assert_eq!(preview.items[0].mimetype, "video/mp4");
    assert!(
        preview.items[0].bytes.get(4..8) == Some(b"ftyp".as_slice()),
        "the GIF went out as MP4"
    );
    h.ui(Ui::AttachSend).await;
    match sent_media(&h).as_slice() {
        [
            Content::Video {
                gif: true,
                width: 320,
                height: 200,
                ..
            },
        ] => {}
        other => panic!("not a GIF: {other:?}"),
    }
    let last = h.app.open.as_ref().unwrap().messages.back().unwrap();
    assert_eq!(last.content.preview(), "GIF");
    let _ = std::fs::remove_dir_all(dir);
}

#[cfg(feature = "video")]
#[tokio::test]
async fn an_unreadable_video_still_goes_as_a_file() {
    let dir = tmp("badmp4");
    let bad = dir.join("broken.mp4");
    std::fs::write(&bad, b"definitely not an mp4").unwrap();
    let mut h = Harness::new().await;
    h.ui(Ui::OpenChat(PABLO.into())).await;
    stage(&mut h, vec![bad]).await;
    assert!(
        h.app
            .toast
            .as_deref()
            .is_some_and(|t| t.contains("sent as a file")),
        "{:?}",
        h.app.toast
    );
    let preview = h.app.attach.as_ref().expect("preview open");
    assert_eq!(preview.items[0].kind, OutgoingKind::Document);
    h.ui(Ui::AttachSend).await;
    assert!(matches!(
        sent_media(&h).as_slice(),
        [Content::Document { .. }]
    ));
    let _ = std::fs::remove_dir_all(dir);
}

fn png_file(dir: &Path, name: &str) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, crate::mock::sunset_png(60, 40)).unwrap();
    p
}

#[tokio::test]
async fn dropped_files_open_one_preview_together() {
    let dir = tmp("drop");
    let a = png_file(&dir, "a.png");
    let b = dir.join("notes.txt");
    std::fs::write(&b, b"hola").unwrap();
    let mut h = Harness::new().await;
    h.ui(Ui::OpenChat(PABLO.into())).await;
    h.ui(Ui::Drop(DropMsg::Hovered)).await;
    assert!(h.app.drop.hovering);
    // Both files arrive back to back; only the first waits for the others.
    let first = h.app.update(Message::Ui(Ui::Drop(DropMsg::Dropped(a))));
    let second = h.app.update(Message::Ui(Ui::Drop(DropMsg::Dropped(b))));
    assert!(!h.app.drop.hovering);
    h.pump(second).await;
    assert!(
        h.app.attach.is_none(),
        "nothing staged before the drop ends"
    );
    h.pump(first).await;
    let preview = h.app.attach.as_ref().expect("preview open");
    assert_eq!(preview.items.len(), 2);
    assert!(matches!(preview.items[0].kind, OutgoingKind::Image { .. }));
    assert_eq!(preview.items[1].kind, OutgoingKind::Document);
    h.ui(Ui::AttachSend).await;
    assert_eq!(sent_media(&h).len(), 2);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_drop_without_an_open_chat_says_so_and_a_cancelled_drag_hides_the_overlay() {
    let dir = tmp("drop-nochat");
    let a = png_file(&dir, "a.png");
    let mut h = Harness::new().await;
    h.ui(Ui::Drop(DropMsg::Hovered)).await;
    h.ui(Ui::Drop(DropMsg::Left)).await;
    assert!(!h.app.drop.hovering);
    h.ui(Ui::Drop(DropMsg::Dropped(a))).await;
    assert!(h.app.attach.is_none());
    assert_eq!(h.app.toast.as_deref(), Some("Open a chat to send files."));
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_drop_while_recording_is_ignored() {
    let dir = tmp("drop-rec");
    let a = png_file(&dir, "a.png");
    let mut h = Harness::new().await;
    h.ui(Ui::OpenChat(PABLO.into())).await;
    h.app.recording_since = Some(std::time::Instant::now());
    h.ui(Ui::Drop(DropMsg::Dropped(a))).await;
    assert!(h.app.attach.is_none());
    h.app.recording_since = None;
    let _ = std::fs::remove_dir_all(dir);
}
