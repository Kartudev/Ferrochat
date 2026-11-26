//! Protocol payload fixtures (protobuf-JSON) → domain conversion.

use ferrochat::net::convert;
use ferrochat::net::types::{Content, MediaKind, ReceiptStatus};
use whatsapp_rust::waproto::whatsapp as wa;

fn load<T: serde::de::DeserializeOwned>(name: &str) -> T {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn image_fixture_converts() {
    let msg: wa::Message = load("image_message.json");
    match convert::content_from_wa(&msg).unwrap() {
        Content::Image {
            caption,
            width,
            height,
            thumbnail,
            media,
        } => {
            assert_eq!(caption.as_deref(), Some("Sunset"));
            assert_eq!((width, height), (1920, 1080));
            assert_eq!(thumbnail.unwrap(), vec![0xff, 0xd8, 0xff, 0xd9]);
            let m = media.unwrap();
            assert_eq!(m.kind, MediaKind::Image);
            assert_eq!(m.media_key.len(), 32);
            assert_eq!(m.file_length, 48213);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn ephemeral_voice_note_fixture_converts() {
    let msg: wa::Message = load("voice_note_ephemeral.json");
    match convert::content_from_wa(&msg).unwrap() {
        Content::Voice {
            seconds,
            ptt,
            waveform,
            media,
        } => {
            assert_eq!(seconds, 9);
            assert!(ptt);
            assert_eq!(waveform.len(), 64);
            assert_eq!(media.unwrap().direct_path, "/v/t62.7117-24/voice.enc");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn document_with_caption_fixture_converts() {
    let msg: wa::Message = load("document_with_caption.json");
    match convert::content_from_wa(&msg).unwrap() {
        Content::Document {
            file_name,
            pages,
            caption,
            size,
            ..
        } => {
            assert_eq!(file_name, "invoice-2026.pdf");
            assert_eq!(pages, Some(3));
            assert_eq!(caption.as_deref(), Some("Here you go"));
            assert_eq!(size, 184320);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn history_sync_fixture_converts() {
    let hs: wa::HistorySync = load("history_sync.json");
    let convert::HistoryParts {
        chats,
        messages,
        names,
        ..
    } = convert::from_history(&hs, 1_700_001_000);
    // status@broadcast is skipped.
    assert_eq!(chats.len(), 2);
    let dm = &chats[0];
    assert_eq!(dm.unread, 2);
    assert!(dm.pinned);
    assert_eq!(dm.last_ts, 1_700_000_500);
    let group = &chats[1];
    assert_eq!(group.name.as_deref(), Some("Book club"));
    assert!(group.archived && group.is_muted(1_700_001_000));
    assert_eq!(group.mute_end, 4_000_000_000);
    assert_eq!(messages.len(), 2);
    let mine = messages.iter().find(|m| m.from_me).unwrap();
    assert_eq!(mine.status, ReceiptStatus::Read);
    assert_eq!(mine.content, Content::Text("See you tomorrow".into()));
    let theirs = messages.iter().find(|m| !m.from_me).unwrap();
    assert_eq!(theirs.sender, "34600111222@s.whatsapp.net");
    assert_eq!(theirs.sender_name.as_deref(), Some("Ana"));
    assert_eq!(
        names,
        vec![("34600111222@s.whatsapp.net".to_string(), "Ana".to_string())]
    );
}
