//! App-level tests of forwarding: the menu entry, selection mode, the chat
//! picker and the copies it sends (real `App` + fake client).

use super::forward::{Fwd, MAX_FORWARD_CHATS};
use super::test_support::Harness;
use super::{Popup, Ui};
use crate::mock::SHOWCASE_CHAT;
use crate::net::fake::{FakeOptions, Sent};
use crate::net::{Content, Message as ChatMessage, ReceiptStatus};

const MARCOS: &str = "34622222222@s.whatsapp.net";
const PABLO: &str = "34655555555@s.whatsapp.net";
const FAMILY: &str = "120363000000000001@g.us";

async fn fwd(h: &mut Harness, f: Fwd) {
    h.ui(Ui::Forward(f)).await;
}

/// Opens the showcase chat and returns the ids of a text, a photo and a
/// message that was itself forwarded, oldest first.
async fn open_showcase(h: &mut Harness) -> (String, String, String) {
    h.ui(Ui::OpenChat(SHOWCASE_CHAT.into())).await;
    let c = h.app.open.as_ref().unwrap();
    let find = |f: &dyn Fn(&ChatMessage) -> bool| {
        c.messages
            .iter()
            .find(|m| f(m))
            .map(|m| m.id.clone())
            .unwrap()
    };
    (
        find(&|m| matches!(&m.content, Content::Text(t) if t.contains("Te paso"))),
        find(&|m| matches!(m.content, Content::Image { .. })),
        find(&|m| m.forwarded.is_some()),
    )
}

fn forwards(h: &Harness) -> Vec<(String, String, String, u32)> {
    h.fake
        .log()
        .into_iter()
        .filter_map(|s| match s {
            Sent::Forward {
                chat,
                id,
                source,
                score,
            } => Some((chat, id, source, score)),
            _ => None,
        })
        .collect()
}

fn cached(h: &Harness, chat: &str) -> Vec<ChatMessage> {
    h.app.store.load_messages(chat, None, 500).unwrap()
}

#[tokio::test]
async fn forward_from_the_menu_sends_a_marked_copy() {
    let mut h = Harness::new().await;
    let (text, _, _) = open_showcase(&mut h).await;
    h.ui(Ui::ToggleMessageMenu(text.clone())).await;
    assert!(matches!(h.app.popup, Some((_, Popup::Menu))));
    fwd(&mut h, Fwd::One(text.clone())).await;
    assert!(h.app.popup.is_none(), "the menu closes");
    let d = h.app.forward.as_ref().unwrap();
    assert_eq!(d.msgs.len(), 1);
    // Sending without a chat does nothing.
    fwd(&mut h, Fwd::Send).await;
    assert!(h.app.forward.is_some());
    assert!(forwards(&h).is_empty());

    fwd(&mut h, Fwd::Target(MARCOS.into())).await;
    fwd(&mut h, Fwd::Send).await;
    assert!(h.app.forward.is_none());
    let sent = forwards(&h);
    assert_eq!(sent.len(), 1);
    let (chat, id, source, score) = &sent[0];
    assert_eq!((chat.as_str(), source, *score), (MARCOS, &text, 0));
    // The copy is ours, marked forwarded, sent, cached and in the list.
    let copy = cached(&h, MARCOS)
        .into_iter()
        .find(|m| &m.id == id)
        .unwrap();
    assert!(copy.from_me);
    assert_eq!(copy.forwarded, Some(0));
    assert_eq!(copy.content, Content::Text("Te paso el itinerario".into()));
    let last = h.app.chats.get(MARCOS).unwrap().last.clone().unwrap();
    assert_eq!(&last.id, id);
    assert_eq!(last.status, ReceiptStatus::Sent);
    // The open conversation is untouched.
    assert_eq!(h.app.open.as_ref().unwrap().chat, SHOWCASE_CHAT);
}

#[tokio::test]
async fn selected_messages_go_to_several_chats_in_order() {
    let mut h = Harness::new().await;
    let (text, photo, fwded) = open_showcase(&mut h).await;
    fwd(&mut h, Fwd::Select(photo.clone())).await;
    fwd(&mut h, Fwd::Toggle(text.clone())).await;
    fwd(&mut h, Fwd::Toggle(fwded.clone())).await;
    fwd(&mut h, Fwd::Toggle(fwded.clone())).await;
    fwd(&mut h, Fwd::Toggle(fwded.clone())).await;
    assert_eq!(h.app.selection.as_ref().unwrap().ids.len(), 3);
    fwd(&mut h, Fwd::Selected).await;
    // Oldest first, whatever the click order.
    let order: Vec<String> = h
        .app
        .forward
        .as_ref()
        .unwrap()
        .msgs
        .iter()
        .map(|m| m.id.clone())
        .collect();
    assert_eq!(order, vec![text.clone(), photo.clone(), fwded.clone()]);
    fwd(&mut h, Fwd::Target(MARCOS.into())).await;
    fwd(&mut h, Fwd::Target(FAMILY.into())).await;
    fwd(&mut h, Fwd::Send).await;
    assert!(h.app.selection.is_none(), "selection mode ends");

    let sent = forwards(&h);
    assert_eq!(sent.len(), 6);
    let to = |chat: &str| -> Vec<(String, u32)> {
        sent.iter()
            .filter(|s| s.0 == chat)
            .map(|s| (s.2.clone(), s.3))
            .collect()
    };
    // The last one was itself a forward: its score goes up.
    let expected = vec![(text.clone(), 0), (photo.clone(), 0), (fwded.clone(), 1)];
    assert_eq!(to(MARCOS), expected);
    assert_eq!(to(FAMILY), expected);
    // Copies keep the order in the target chat and keep the media.
    let copies: Vec<ChatMessage> = cached(&h, FAMILY)
        .into_iter()
        .filter(|m| m.forwarded.is_some())
        .collect();
    assert_eq!(copies.len(), 3);
    assert!(matches!(copies[0].content, Content::Text(_)));
    assert_eq!(copies[2].forwarded, Some(1));
    match (
        &copies[1].content,
        h.app.store.message(SHOWCASE_CHAT, &photo).unwrap(),
    ) {
        (Content::Image { media, .. }, Some(src)) => {
            assert_eq!(media.as_ref(), src.content.media());
        }
        c => panic!("{c:?}"),
    }
}

#[tokio::test]
async fn forwarding_into_the_open_chat_shows_the_copies_there() {
    let mut h = Harness::new().await;
    let (text, _, _) = open_showcase(&mut h).await;
    fwd(&mut h, Fwd::One(text.clone())).await;
    fwd(&mut h, Fwd::Target(SHOWCASE_CHAT.into())).await;
    fwd(&mut h, Fwd::Send).await;
    let c = h.app.open.as_ref().unwrap();
    let newest = c.messages.back().unwrap();
    assert_eq!(newest.forwarded, Some(0));
    assert!(newest.from_me);
}

#[tokio::test]
async fn at_most_five_chats_and_one_for_frequently_forwarded() {
    let mut h = Harness::new().await;
    let (text, _, _) = open_showcase(&mut h).await;
    fwd(&mut h, Fwd::One(text.clone())).await;
    let chats: Vec<String> = super::forward::targets(&h.app.chats, "")
        .iter()
        .map(|e| e.meta.id.clone())
        .collect();
    assert!(chats.len() > MAX_FORWARD_CHATS);
    for c in &chats {
        fwd(&mut h, Fwd::Target(c.clone())).await;
    }
    assert_eq!(
        h.app.forward.as_ref().unwrap().targets.len(),
        MAX_FORWARD_CHATS
    );
    fwd(&mut h, Fwd::Close).await;
    assert!(h.app.forward.is_none());

    // A message forwarded many times goes to one chat.
    if let Some(c) = &mut h.app.open
        && let Some(m) = c.messages.iter_mut().find(|m| m.id == text)
    {
        m.forwarded = Some(127);
    }
    fwd(&mut h, Fwd::One(text.clone())).await;
    fwd(&mut h, Fwd::Target(MARCOS.into())).await;
    fwd(&mut h, Fwd::Target(PABLO.into())).await;
    assert_eq!(
        h.app.forward.as_ref().unwrap().targets,
        vec![MARCOS.to_string()]
    );
    fwd(&mut h, Fwd::Send).await;
    assert_eq!(forwards(&h)[0].3, 127);
}

#[tokio::test]
async fn deleted_and_pending_messages_cannot_be_forwarded() {
    let mut h = Harness::new().await;
    let (text, _, _) = open_showcase(&mut h).await;
    fwd(&mut h, Fwd::Select(text.clone())).await;
    if let Some(c) = &mut h.app.open
        && let Some(m) = c.messages.iter_mut().find(|m| m.id == text)
    {
        m.content = Content::Deleted;
    }
    // Deselecting works, selecting a deleted message does not.
    fwd(&mut h, Fwd::Toggle(text.clone())).await;
    fwd(&mut h, Fwd::Toggle(text.clone())).await;
    assert!(h.app.selection.as_ref().unwrap().ids.is_empty());
    fwd(&mut h, Fwd::Selected).await;
    assert!(h.app.forward.is_none(), "nothing to forward");
    fwd(&mut h, Fwd::One(text)).await;
    assert!(h.app.forward.is_none());
}

#[tokio::test]
async fn escape_closes_the_dialog_then_the_selection_then_the_chat() {
    let mut h = Harness::new().await;
    let (text, _, _) = open_showcase(&mut h).await;
    fwd(&mut h, Fwd::Select(text)).await;
    fwd(&mut h, Fwd::Selected).await;
    assert!(h.app.forward.is_some());
    h.ui(Ui::CloseChat).await;
    assert!(h.app.forward.is_none());
    assert!(h.app.selection.is_some());
    h.ui(Ui::CloseChat).await;
    assert!(h.app.selection.is_none());
    assert!(h.app.open.is_some());
    h.ui(Ui::CloseChat).await;
    assert!(h.app.open.is_none());
}

#[tokio::test]
async fn opening_another_chat_leaves_selection_mode() {
    let mut h = Harness::new().await;
    let (text, _, _) = open_showcase(&mut h).await;
    fwd(&mut h, Fwd::Select(text)).await;
    h.ui(Ui::OpenChat(MARCOS.into())).await;
    assert!(h.app.selection.is_none());
}

#[tokio::test]
async fn a_failed_forward_marks_the_copy_failed() {
    let mut h = Harness::with(FakeOptions {
        auto_receipts: false,
        fail_sends: true,
        ..FakeOptions::default()
    })
    .await;
    let (text, _, _) = open_showcase(&mut h).await;
    fwd(&mut h, Fwd::One(text)).await;
    fwd(&mut h, Fwd::Target(MARCOS.into())).await;
    fwd(&mut h, Fwd::Send).await;
    let last = h.app.chats.get(MARCOS).unwrap().last.clone().unwrap();
    assert_eq!(last.forwarded, Some(0));
    assert_eq!(last.status, ReceiptStatus::Failed);
}
