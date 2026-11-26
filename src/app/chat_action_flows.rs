//! App-level tests of the chat menu actions: mark read / unread, mute for a
//! while, clear chat and delete chat (real `App` + fake client).

use super::chat_actions::{ChatAct, ChatDialog, MuteFor};
use super::nav::NavMsg;
use super::test_support::Harness;
use super::{MenuTarget, Ui};
use crate::net::fake::{FakeOptions, Sent};
use crate::net::{ChatFlag, Event, MUTE_FOREVER, MUTE_OFF};

const MARCOS: &str = "34622222222@s.whatsapp.net";
const PABLO: &str = "34655555555@s.whatsapp.net";

async fn act(h: &mut Harness, a: ChatAct) {
    h.ui(Ui::ChatAct(a)).await;
}

fn unread(h: &Harness, chat: &str) -> u32 {
    h.app.chats.get(chat).map(|e| e.meta.unread).unwrap_or(0)
}

fn cached_unread(h: &Harness, chat: &str) -> u32 {
    h.app
        .store
        .chat_rows()
        .unwrap()
        .into_iter()
        .find(|r| r.meta.id == chat)
        .map(|r| r.meta.unread)
        .unwrap_or(0)
}

fn failing() -> FakeOptions {
    FakeOptions {
        auto_receipts: false,
        fail_sends: true,
        ..FakeOptions::default()
    }
}

#[tokio::test]
async fn mark_unread_and_read_sync_with_the_phone() {
    let mut h = Harness::new().await;
    assert_eq!(unread(&h, PABLO), 0);
    let last = h.app.chats.get(PABLO).unwrap().last.clone().unwrap().id;
    // The menu closes when an action is picked.
    h.ui(Ui::ChatMenu(PABLO.into())).await;
    assert!(matches!(
        h.app.sidebar_menu.as_ref().map(|m| &m.target),
        Some(MenuTarget::Chat(_))
    ));
    act(&mut h, ChatAct::MarkUnread(PABLO.into())).await;
    assert!(h.app.sidebar_menu.is_none());
    assert_eq!(unread(&h, PABLO), 1);
    assert_eq!(cached_unread(&h, PABLO), 1);
    assert!(h.fake.log().contains(&Sent::MarkChat {
        chat: PABLO.into(),
        read: false,
        last: Some(last.clone()),
    }));
    act(&mut h, ChatAct::MarkRead(PABLO.into())).await;
    assert_eq!(unread(&h, PABLO), 0);
    assert_eq!(cached_unread(&h, PABLO), 0);
    assert!(h.fake.log().contains(&Sent::MarkChat {
        chat: PABLO.into(),
        read: true,
        last: Some(last),
    }));
}

#[tokio::test]
async fn a_failed_mark_unread_restores_the_badge() {
    let mut h = Harness::with(failing()).await;
    act(&mut h, ChatAct::MarkUnread(PABLO.into())).await;
    assert_eq!(unread(&h, PABLO), 0, "reverted");
    assert_eq!(cached_unread(&h, PABLO), 0);
    assert!(h.app.toast.is_some());
}

#[tokio::test]
async fn reading_a_chat_marked_unread_tells_the_phone() {
    let mut h = Harness::new().await;
    act(&mut h, ChatAct::MarkUnread(PABLO.into())).await;
    let t = h.app.update(super::Message::Focus(true));
    h.pump(t).await;
    h.ui(Ui::OpenChat(PABLO.into())).await;
    assert_eq!(unread(&h, PABLO), 0);
    let reads: Vec<_> = h
        .fake
        .log()
        .into_iter()
        .filter(|s| matches!(s, Sent::MarkChat { read: true, .. }))
        .collect();
    assert_eq!(reads.len(), 1, "one app-state read for the manual mark");
    // A chat that only had new messages does not send one.
    h.ui(Ui::OpenChat(MARCOS.into())).await;
    let reads = h
        .fake
        .log()
        .into_iter()
        .filter(|s| matches!(s, Sent::MarkChat { read: true, .. }))
        .count();
    assert_eq!(reads, 1);
}

#[tokio::test]
async fn mute_for_a_while_ends_on_its_own() {
    let mut h = Harness::new().await;
    act(&mut h, ChatAct::AskMute(MARCOS.into())).await;
    assert_eq!(
        h.app.chat_dialog,
        Some(ChatDialog::Mute {
            chat: MARCOS.into(),
            choice: MuteFor::Hours8,
        })
    );
    act(&mut h, ChatAct::PickMute(MuteFor::Week)).await;
    let before = chrono::Utc::now().timestamp();
    act(&mut h, ChatAct::ConfirmMute).await;
    assert!(h.app.chat_dialog.is_none());
    let end = h.app.chats.get(MARCOS).unwrap().meta.mute_end;
    assert!((before + 604_800..=before + 604_810).contains(&end));
    assert!(h.app.is_muted(MARCOS));
    assert!(h.fake.log().contains(&Sent::Mute {
        chat: MARCOS.into(),
        end,
    }));
    let cached = h.app.store.chat_rows().unwrap();
    let cached = cached.iter().find(|r| r.meta.id == MARCOS).unwrap();
    assert_eq!(cached.meta.mute_end, end);
    // Muted chats do not count on the rail badge.
    let before_badge = h.app.unread_chats();
    act(&mut h, ChatAct::Unmute(MARCOS.into())).await;
    assert!(!h.app.is_muted(MARCOS));
    assert!(h.fake.log().contains(&Sent::Mute {
        chat: MARCOS.into(),
        end: MUTE_OFF,
    }));
    assert_eq!(h.app.unread_chats(), before_badge + 1);
    // A mute from the phone that already ended does not mute.
    h.net(Event::ChatFlag {
        chat: MARCOS.into(),
        flag: ChatFlag::Muted(1_000),
    })
    .await;
    assert!(!h.app.is_muted(MARCOS));
    h.net(Event::ChatFlag {
        chat: MARCOS.into(),
        flag: ChatFlag::Muted(MUTE_FOREVER),
    })
    .await;
    assert!(h.app.is_muted(MARCOS));
}

#[tokio::test]
async fn a_failed_mute_is_reverted() {
    let mut h = Harness::with(failing()).await;
    act(&mut h, ChatAct::AskMute(MARCOS.into())).await;
    act(&mut h, ChatAct::ConfirmMute).await;
    assert!(!h.app.is_muted(MARCOS), "reverted");
    assert!(h.app.toast.is_some());
}

#[tokio::test]
async fn clear_chat_keeps_the_row_and_the_starred_messages() {
    let mut h = Harness::new().await;
    h.ui(Ui::OpenChat(PABLO.into())).await;
    let conv = h.app.open.as_ref().unwrap();
    assert!(conv.messages.len() >= 2, "mock chat has history");
    let keep = conv.messages[0].id.clone();
    h.ui(Ui::Nav(NavMsg::ToggleStar(keep.clone()))).await;
    act(&mut h, ChatAct::AskClear(PABLO.into())).await;
    assert_eq!(
        h.app.chat_dialog,
        Some(ChatDialog::Clear {
            chat: PABLO.into(),
            keep_starred: true,
        })
    );
    act(&mut h, ChatAct::ConfirmClear).await;
    assert!(h.fake.log().contains(&Sent::ClearChat {
        chat: PABLO.into(),
        keep_starred: true,
    }));
    let ids: Vec<_> = h
        .app
        .open
        .as_ref()
        .unwrap()
        .messages
        .iter()
        .map(|m| m.id.clone())
        .collect();
    assert_eq!(ids, vec![keep.clone()]);
    assert!(h.app.chats.get(PABLO).is_some(), "the chat stays");
    let cached = h.app.store.load_messages(PABLO, None, 100).unwrap();
    assert_eq!(cached.len(), 1);
    // Clearing again without keeping the stars empties it.
    act(&mut h, ChatAct::AskClear(PABLO.into())).await;
    act(&mut h, ChatAct::KeepStarred(false)).await;
    act(&mut h, ChatAct::ConfirmClear).await;
    assert!(h.app.open.as_ref().unwrap().messages.is_empty());
    assert!(
        h.app
            .store
            .load_messages(PABLO, None, 100)
            .unwrap()
            .is_empty()
    );
    assert!(!h.app.is_starred(PABLO, &keep));
}

#[tokio::test]
async fn delete_chat_closes_and_removes_it() {
    let mut h = Harness::new().await;
    h.ui(Ui::OpenChat(PABLO.into())).await;
    act(&mut h, ChatAct::AskDelete(PABLO.into())).await;
    // Esc cancels the dialog first, the chat stays open.
    h.ui(Ui::CloseChat).await;
    assert!(h.app.chat_dialog.is_none());
    assert!(h.app.open.is_some());
    act(&mut h, ChatAct::AskDelete(PABLO.into())).await;
    act(&mut h, ChatAct::ConfirmDelete).await;
    assert!(
        h.fake
            .log()
            .contains(&Sent::DeleteChat { chat: PABLO.into() })
    );
    assert!(h.app.open.is_none());
    assert!(h.app.chats.get(PABLO).is_none());
    assert!(
        !h.app
            .store
            .chat_rows()
            .unwrap()
            .iter()
            .any(|r| r.meta.id == PABLO)
    );
    assert!(
        h.app
            .store
            .load_messages(PABLO, None, 100)
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn a_failed_delete_keeps_the_chat() {
    let mut h = Harness::with(failing()).await;
    act(&mut h, ChatAct::AskDelete(PABLO.into())).await;
    act(&mut h, ChatAct::ConfirmDelete).await;
    assert!(h.app.chats.get(PABLO).is_some());
    assert!(
        !h.app
            .store
            .load_messages(PABLO, None, 100)
            .unwrap()
            .is_empty()
    );
    assert!(h.app.toast.is_some());
}

#[tokio::test]
async fn clear_and_delete_from_the_phone_apply_here() {
    let mut h = Harness::new().await;
    h.net(Event::ChatCleared {
        chat: PABLO.into(),
        keep_starred: false,
    })
    .await;
    assert!(h.app.chats.get(PABLO).unwrap().last.is_none());
    assert!(
        h.app
            .store
            .load_messages(PABLO, None, 100)
            .unwrap()
            .is_empty()
    );
    h.net(Event::ChatDeleted {
        chat: MARCOS.into(),
    })
    .await;
    assert!(h.app.chats.get(MARCOS).is_none());
    // Nothing went back to the phone.
    assert!(
        !h.fake
            .log()
            .iter()
            .any(|s| matches!(s, Sent::ClearChat { .. } | Sent::DeleteChat { .. }))
    );
}
