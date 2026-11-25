//! App-level tests of expired media (re-upload on 404 / 410, also before
//! forwarding old media) and of the "Message info" panel (real `App` + fake
//! client).

use super::Ui;
use super::forward::{Fwd, STALE_MEDIA_SECS};
use super::test_support::Harness;
use crate::mock::SHOWCASE_CHAT;
use crate::net::fake::Sent;
use crate::net::{Content, Event, ReceiptStatus};

const PABLO: &str = "34655555555@s.whatsapp.net";
const MARCOS: &str = "34622222222@s.whatsapp.net";
const FAMILY: &str = "120363000000000001@g.us";

/// Opens the showcase chat and returns the id and media path of its photo.
async fn photo(h: &mut Harness) -> (String, String) {
    h.ui(Ui::OpenChat(SHOWCASE_CHAT.into())).await;
    let m = h
        .app
        .open
        .as_ref()
        .unwrap()
        .messages
        .iter()
        .find(|m| matches!(m.content, Content::Image { media: Some(_), .. }))
        .unwrap();
    (m.id.clone(), m.content.media().unwrap().direct_path.clone())
}

fn reuploads(h: &Harness) -> usize {
    h.fake
        .log()
        .iter()
        .filter(|s| matches!(s, Sent::Reupload { .. }))
        .count()
}

fn cached_path(h: &Harness, chat: &str, id: &str) -> String {
    h.app
        .store
        .message(chat, id)
        .unwrap()
        .unwrap()
        .content
        .media()
        .unwrap()
        .direct_path
        .clone()
}

fn open_path(h: &Harness, id: &str) -> String {
    h.app
        .find_message(id)
        .unwrap()
        .content
        .media()
        .unwrap()
        .direct_path
        .clone()
}

#[tokio::test]
async fn expired_media_is_reuploaded_downloaded_and_remembered() {
    let mut h = Harness::new().await;
    let (id, path) = photo(&mut h).await;
    h.app.images.clear();
    h.fake.expire_media(&path, false);
    let t = h
        .app
        .request_media(&id, crate::media::MAX_IMAGE_PIXELS, true);
    h.pump(t).await;
    assert_eq!(reuploads(&h), 1);
    let key = h
        .app
        .find_message(&id)
        .unwrap()
        .content
        .media()
        .unwrap()
        .cache_key();
    assert!(h.app.images.contains(&key), "shown after the re-upload");
    let fresh = format!("{path}-re");
    assert_eq!(open_path(&h, &id), fresh, "the open chat uses the new path");
    assert_eq!(
        cached_path(&h, SHOWCASE_CHAT, &id),
        fresh,
        "and so does the cache"
    );
    // Next time it downloads straight away.
    h.app.images.clear();
    let t = h
        .app
        .request_media(&id, crate::media::MAX_IMAGE_PIXELS, true);
    h.pump(t).await;
    assert_eq!(reuploads(&h), 1);
    assert!(h.app.images.contains(&key));
}

#[tokio::test]
async fn media_nobody_has_any_more_stays_a_placeholder() {
    let mut h = Harness::new().await;
    let (id, path) = photo(&mut h).await;
    h.app.images.clear();
    h.fake.expire_media(&path, true);
    let t = h
        .app
        .request_media(&id, crate::media::MAX_IMAGE_PIXELS, true);
    h.pump(t).await;
    assert_eq!(reuploads(&h), 1);
    let key = h
        .app
        .find_message(&id)
        .unwrap()
        .content
        .media()
        .unwrap()
        .cache_key();
    assert!(!h.app.images.contains(&key));
    assert_eq!(open_path(&h, &id), path);
    assert!(h.app.downloading.is_empty(), "the slot is free again");
}

#[tokio::test]
async fn stale_media_is_reuploaded_once_before_it_is_forwarded() {
    let mut h = Harness::new().await;
    let (id, path) = photo(&mut h).await;
    // A month-old photo whose CDN copy is gone.
    if let Some(m) = h
        .app
        .open
        .as_mut()
        .unwrap()
        .messages
        .iter_mut()
        .find(|m| m.id == id)
    {
        m.timestamp = chrono::Utc::now().timestamp() - STALE_MEDIA_SECS * 2;
    }
    h.fake.expire_media(&path, false);
    h.ui(Ui::Forward(Fwd::One(id.clone()))).await;
    h.ui(Ui::Forward(Fwd::Target(PABLO.into()))).await;
    h.ui(Ui::Forward(Fwd::Target(MARCOS.into()))).await;
    h.ui(Ui::Forward(Fwd::Send)).await;
    assert_eq!(reuploads(&h), 1, "once for both copies");
    let fwds = h
        .fake
        .log()
        .iter()
        .filter(|s| matches!(s, Sent::Forward { .. }))
        .count();
    assert_eq!(fwds, 2);
    let fresh = format!("{path}-re");
    assert_eq!(cached_path(&h, SHOWCASE_CHAT, &id), fresh);
    assert_eq!(open_path(&h, &id), fresh);
}

#[tokio::test]
async fn recent_media_is_forwarded_without_a_reupload() {
    let mut h = Harness::new().await;
    let (id, _) = photo(&mut h).await;
    // Mock data is dated 2023: make the photo recent.
    if let Some(m) = h
        .app
        .open
        .as_mut()
        .unwrap()
        .messages
        .iter_mut()
        .find(|m| m.id == id)
    {
        m.timestamp = chrono::Utc::now().timestamp();
    }
    h.ui(Ui::Forward(Fwd::One(id))).await;
    h.ui(Ui::Forward(Fwd::Target(PABLO.into()))).await;
    h.ui(Ui::Forward(Fwd::Send)).await;
    assert_eq!(reuploads(&h), 0);
}

// ------------------------------------------------------------ message info

fn own_text(h: &Harness, needle: &str) -> String {
    h.app
        .open
        .as_ref()
        .unwrap()
        .messages
        .iter()
        .find(|m| m.from_me && matches!(&m.content, Content::Text(t) if t.contains(needle)))
        .map(|m| m.id.clone())
        .unwrap()
}

async fn receipt(
    h: &mut Harness,
    chat: &str,
    id: &str,
    status: ReceiptStatus,
    p: Option<&str>,
    ts: i64,
) {
    h.net(Event::Receipt {
        chat: chat.into(),
        ids: vec![id.into()],
        status,
        participant: p.map(str::to_string),
        timestamp: ts,
    })
    .await;
}

#[tokio::test]
async fn message_info_shows_receipt_times_and_follows_new_receipts() {
    let mut h = Harness::new().await;
    h.ui(Ui::OpenChat(SHOWCASE_CHAT.into())).await;
    let id = own_text(&h, "Llevo yo el café");
    receipt(
        &mut h,
        SHOWCASE_CHAT,
        &id,
        ReceiptStatus::Delivered,
        None,
        1_000,
    )
    .await;
    h.ui(Ui::MessageInfo(id.clone())).await;
    let s = h.app.msg_info_summary().expect("loaded");
    assert_eq!(s.delivered[0].at, Some(1_000));
    assert!(s.read.is_empty());
    // A read receipt while the panel is open shows up in it.
    receipt(&mut h, SHOWCASE_CHAT, &id, ReceiptStatus::Read, None, 1_500).await;
    let s = h.app.msg_info_summary().unwrap();
    assert_eq!(s.read[0].at, Some(1_500));
    assert_eq!(s.delivered[0].at, Some(1_000));
    // Esc closes the panel, not the chat.
    h.ui(Ui::CloseChat).await;
    assert!(h.app.msg_info.is_none());
    assert!(h.app.open.is_some());
}

#[tokio::test]
async fn message_info_is_only_for_our_messages_and_closes_with_the_chat() {
    let mut h = Harness::new().await;
    h.ui(Ui::OpenChat(SHOWCASE_CHAT.into())).await;
    let theirs = h
        .app
        .open
        .as_ref()
        .unwrap()
        .messages
        .iter()
        .find(|m| !m.from_me)
        .map(|m| m.id.clone())
        .unwrap();
    h.ui(Ui::MessageInfo(theirs)).await;
    assert!(h.app.msg_info.is_none());
    let id = own_text(&h, "Llevo yo el café");
    h.ui(Ui::ToggleInfo).await;
    h.ui(Ui::MessageInfo(id)).await;
    assert!(h.app.msg_info.is_some());
    assert!(h.app.info.is_none(), "one panel at a time");
    h.ui(Ui::OpenChat(PABLO.into())).await;
    assert!(h.app.msg_info.is_none());
}

#[tokio::test]
async fn group_message_info_lists_readers_and_receivers() {
    let mut h = Harness::new().await;
    h.ui(Ui::OpenChat(FAMILY.into())).await;
    let id = own_text(&h, "Yo llevo las bebidas");
    let (mum, rosa) = ("34677777777@s.whatsapp.net", "34688888888@s.whatsapp.net");
    receipt(
        &mut h,
        FAMILY,
        &id,
        ReceiptStatus::Delivered,
        Some(mum),
        100,
    )
    .await;
    receipt(&mut h, FAMILY, &id, ReceiptStatus::Read, Some(mum), 300).await;
    receipt(
        &mut h,
        FAMILY,
        &id,
        ReceiptStatus::Delivered,
        Some(rosa),
        200,
    )
    .await;
    h.ui(Ui::MessageInfo(id)).await;
    let s = h.app.msg_info_summary().unwrap();
    assert_eq!(s.read.len(), 1);
    assert_eq!(s.read[0].participant, mum);
    assert_eq!(s.read[0].at, Some(300));
    assert_eq!(s.delivered.len(), 1);
    assert_eq!(s.delivered[0].participant, rosa);
    assert_eq!(s.waiting, Some(1), "3 other members in the fake group");
}
