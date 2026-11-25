//! Integration tests for the feature-parity work (replies, reactions,
//! edits, deletes, typing / presence, group receipts, search, avatars,
//! video and stickers) against the fake protocol layer. The headless
//! `Driver` applies events the same way `App::on_net` does: store first,
//! then the in-memory reducers.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use ferrochat::app::live::{PresenceBook, TypingTracker, typing_label};
use ferrochat::app::state::{ChatList, Conversation, group_status};
use ferrochat::media::avatar;
use ferrochat::mock::{self, OWN_JID, SHOWCASE_CHAT};
use ferrochat::net::fake::{self, FakeClient, FakeOptions, Sent};
use ferrochat::net::{
    ChatState, Content, Event, Message, MsgKey, Origin, Quote, ReceiptStatus, WaClient,
};
use ferrochat::store::Store;
use futures::StreamExt;

const FAMILY: &str = "120363000000000001@g.us";

struct Driver {
    store: Store,
    chats: ChatList,
    open: Option<Conversation>,
    typing: TypingTracker,
    presence: PresenceBook,
    group_size: Option<usize>,
}

impl Driver {
    fn new() -> Self {
        Self {
            store: Store::open_in_memory().unwrap(),
            chats: ChatList::default(),
            open: None,
            typing: TypingTracker::default(),
            presence: PresenceBook::default(),
            group_size: Some(3),
        }
    }

    fn open_chat(&mut self, chat: &str) {
        let msgs = self.store.load_messages(chat, None, 200).unwrap();
        self.open = Some(Conversation::new(chat.into(), msgs, false));
    }

    fn apply(&mut self, ev: Event) {
        match ev {
            Event::History { chats, names, .. } => {
                self.store.upsert_chats(&chats).unwrap();
                self.store
                    .set_contact_names(&ferrochat::net::push_names(&names))
                    .unwrap();
                self.chats.apply_history(&chats, &names);
            }
            Event::Messages { messages, origin } => {
                let new_ids: HashSet<String> = self
                    .store
                    .insert_messages(&messages)
                    .unwrap()
                    .into_iter()
                    .collect();
                let fresh = (origin != Origin::History).then_some(&new_ids);
                let open = self.open.as_ref().map(|c| c.chat.clone());
                self.chats
                    .apply_messages(&messages, open.as_deref(), true, fresh);
                if origin == Origin::Live {
                    for m in messages.iter().filter(|m| !m.from_me) {
                        self.typing.clear_sender(&m.chat, &m.sender);
                    }
                }
                if let Some(c) = &mut self.open {
                    c.merge(messages);
                }
            }
            Event::Reaction {
                chat,
                target,
                reactor,
                emoji,
                ts,
            } => {
                if let Some(c) = &mut self.open
                    && c.chat == chat
                {
                    c.apply_reaction(&target, &reactor, &emoji, ts);
                }
                self.store
                    .apply_reaction(&chat, &target, &reactor, &emoji, ts)
                    .unwrap();
            }
            Event::Edited {
                chat,
                id,
                sender,
                from_me,
                content,
            } => {
                if let Some(c) = &mut self.open
                    && c.chat == chat
                {
                    c.apply_edit(&id, &sender, from_me, &content);
                }
                self.chats
                    .apply_edit(&chat, &id, &sender, from_me, &content);
                self.store
                    .apply_edit(&chat, &id, &sender, from_me, &content)
                    .unwrap();
            }
            Event::DeletedForMe { chat, id } => {
                if let Some(c) = &mut self.open {
                    c.remove(&id);
                }
                self.store.delete_message(&chat, &id).unwrap();
            }
            Event::ChatState {
                chat,
                participant,
                state,
            } => {
                self.typing
                    .set(&chat, participant.as_deref(), state, Instant::now());
            }
            Event::Presence {
                jid,
                online,
                last_seen,
            } => self.presence.update(&jid, online, last_seen, 1_000),
            Event::Receipt {
                chat,
                ids,
                status,
                participant: Some(p),
                ..
            } => {
                let changed = self
                    .store
                    .apply_group_receipt(&chat, &ids, Some((&p, status)), self.group_size)
                    .unwrap();
                for (id, st) in changed {
                    if let Some(c) = &mut self.open {
                        c.set_status(&id, st);
                    }
                    self.chats.set_status(&chat, &id, st);
                }
            }
            Event::Receipt {
                chat, ids, status, ..
            } => {
                self.store.apply_receipt(&chat, &ids, status).unwrap();
                if let Some(c) = &mut self.open {
                    c.apply_receipt(&ids, status);
                }
            }
            _ => {}
        }
    }
}

async fn setup(
    opts: FakeOptions,
) -> (
    std::sync::Arc<FakeClient>,
    std::pin::Pin<Box<dyn futures::Stream<Item = Event> + Send>>,
    Driver,
) {
    let (client, stream) = fake::start(opts, mock::mock_data(1_763_500_000));
    let mut stream: std::pin::Pin<Box<dyn futures::Stream<Item = Event> + Send>> = Box::pin(stream);
    let mut d = Driver::new();
    drain(&mut stream, &mut d).await;
    (client, stream, d)
}

async fn drain(stream: &mut (impl futures::Stream<Item = Event> + Unpin), d: &mut Driver) {
    while let Ok(Some(ev)) = tokio::time::timeout(Duration::from_millis(50), stream.next()).await {
        d.apply(ev);
    }
}

fn first_text(d: &Driver, chat: &str, from_me: bool) -> Message {
    d.store
        .load_messages(chat, None, 200)
        .unwrap()
        .into_iter()
        .find(|m| m.from_me == from_me && matches!(m.content, Content::Text(_)))
        .unwrap()
}

fn live_msg(id: &str, chat: &str, sender: &str, text: &str, ts: i64) -> Message {
    Message {
        id: id.into(),
        chat: chat.into(),
        sender: sender.into(),
        sender_name: None,
        from_me: false,
        timestamp: ts,
        content: Content::Text(text.into()),
        status: ReceiptStatus::Read,
        expires_at: None,
        reply: None,
        edited: false,
        forwarded: None,
        mentions: Vec::new(),
        reactions: Vec::new(),
    }
}

#[tokio::test]
async fn history_brings_replies_reactions_video_and_stickers() {
    let (_c, _s, d) = setup(FakeOptions::default()).await;
    let msgs = d.store.load_messages(SHOWCASE_CHAT, None, 200).unwrap();
    let reply = msgs.iter().find(|m| m.reply.is_some()).unwrap();
    assert!(reply.reply.as_ref().unwrap().preview.contains("estación"));
    // The quoted message exists, so a click can jump to it.
    let target = &reply.reply.as_ref().unwrap().id;
    assert!(d.store.message(SHOWCASE_CHAT, target).unwrap().is_some());
    assert!(msgs.iter().any(|m| !m.reactions.is_empty()));
    assert!(msgs.iter().any(|m| m.edited));
    let video = msgs
        .iter()
        .find_map(|m| match &m.content {
            Content::Video {
                seconds, thumbnail, ..
            } => Some((*seconds, thumbnail.is_some())),
            _ => None,
        })
        .unwrap();
    assert_eq!(video, (3, true), "duration and thumbnail survive the cache");
    assert!(
        msgs.iter()
            .any(|m| matches!(m.content, Content::Sticker { .. }))
    );
}

#[tokio::test]
async fn replies_are_sent_with_their_quote() {
    let (client, _s, d) = setup(FakeOptions::default()).await;
    let orig = first_text(&d, SHOWCASE_CHAT, false);
    let q = Quote::of(&orig);
    client
        .send_text(SHOWCASE_CHAT, "R1", "answer", Some(&q))
        .await
        .unwrap();
    assert!(client.log().iter().any(|s| matches!(
        s,
        Sent::Text { id, reply_to: Some(r), .. } if id == "R1" && *r == orig.id
    )));
}

#[tokio::test]
async fn incoming_reaction_lifecycle() {
    let (client, mut stream, mut d) = setup(FakeOptions::default()).await;
    d.open_chat(SHOWCASE_CHAT);
    let target = first_text(&d, SHOWCASE_CHAT, true);
    let count = |d: &Driver| {
        d.open
            .as_ref()
            .unwrap()
            .get(&target.id)
            .unwrap()
            .reactions
            .len()
    };
    let before = count(&d);
    client.simulate_reaction(SHOWCASE_CHAT, &target.id, SHOWCASE_CHAT, "🎉");
    drain(&mut stream, &mut d).await;
    assert_eq!(count(&d), before + 1);
    // Persisted: a fresh load shows it.
    let stored = d.store.message(SHOWCASE_CHAT, &target.id).unwrap().unwrap();
    assert!(stored.reactions.iter().any(|r| r.emoji == "🎉"));
    // Changing and removing.
    client.simulate_reaction(SHOWCASE_CHAT, &target.id, SHOWCASE_CHAT, "😮");
    drain(&mut stream, &mut d).await;
    assert_eq!(count(&d), before + 1);
    client.simulate_reaction(SHOWCASE_CHAT, &target.id, SHOWCASE_CHAT, "");
    drain(&mut stream, &mut d).await;
    assert_eq!(count(&d), before);
}

#[tokio::test]
async fn our_reactions_go_through_the_client() {
    let (client, _s, d) = setup(FakeOptions::default()).await;
    let m = first_text(&d, SHOWCASE_CHAT, false);
    client.send_reaction(&m.key(), "👍").await.unwrap();
    client.send_reaction(&m.key(), "").await.unwrap();
    let log = client.log();
    assert!(
        log.iter()
            .any(|s| matches!(s, Sent::Reaction { emoji, .. } if emoji == "👍"))
    );
    assert!(
        log.iter()
            .any(|s| matches!(s, Sent::Reaction { emoji, .. } if emoji.is_empty()))
    );
    let failing = fake::start(
        FakeOptions {
            fail_sends: true,
            ..Default::default()
        },
        mock::mock_data(1),
    )
    .0;
    assert!(failing.send_reaction(&m.key(), "👍").await.is_err());
}

#[tokio::test]
async fn edits_from_the_original_sender_only() {
    let (client, mut stream, mut d) = setup(FakeOptions::default()).await;
    d.open_chat(SHOWCASE_CHAT);
    client.simulate_incoming(live_msg(
        "E1",
        SHOWCASE_CHAT,
        SHOWCASE_CHAT,
        "teh",
        1_763_600_000,
    ));
    drain(&mut stream, &mut d).await;
    let edit = |sender: &str, from_me: bool, text: &str| Event::Edited {
        chat: SHOWCASE_CHAT.into(),
        id: "E1".into(),
        sender: sender.into(),
        from_me,
        content: Box::new(Content::Text(text.into())),
    };
    // A stranger and "me" cannot rewrite their message.
    d.apply(edit("someone@s.whatsapp.net", false, "hax"));
    d.apply(edit(OWN_JID, true, "hax"));
    let get = |d: &Driver| d.open.as_ref().unwrap().get("E1").unwrap().clone();
    assert_eq!(get(&d).content, Content::Text("teh".into()));
    assert!(!get(&d).edited);
    // The author can.
    d.apply(edit(SHOWCASE_CHAT, false, "the"));
    assert_eq!(get(&d).content, Content::Text("the".into()));
    assert!(get(&d).edited);
    let stored = d.store.message(SHOWCASE_CHAT, "E1").unwrap().unwrap();
    assert!(stored.edited);
    assert_eq!(stored.content, Content::Text("the".into()));
    // The chat list preview follows the edit.
    assert_eq!(
        d.chats
            .get(SHOWCASE_CHAT)
            .unwrap()
            .last
            .as_ref()
            .unwrap()
            .content,
        Content::Text("the".into())
    );
    // Our own edits and revokes reach the client.
    client
        .edit_text(SHOWCASE_CHAT, "MINE", "fixed")
        .await
        .unwrap();
    client.revoke(SHOWCASE_CHAT, "MINE").await.unwrap();
    let log = client.log();
    assert!(
        log.iter()
            .any(|s| matches!(s, Sent::Edit { text, .. } if text == "fixed"))
    );
    assert!(
        log.iter()
            .any(|s| matches!(s, Sent::Revoke { id, .. } if id == "MINE"))
    );
}

#[tokio::test]
async fn delete_for_me_removes_locally_and_syncs() {
    let (client, _s, mut d) = setup(FakeOptions::default()).await;
    d.open_chat(SHOWCASE_CHAT);
    let m = first_text(&d, SHOWCASE_CHAT, false);
    // Local removal (what the dialog does) plus the sync to other devices.
    d.apply(Event::DeletedForMe {
        chat: SHOWCASE_CHAT.into(),
        id: m.id.clone(),
    });
    client.delete_for_me(&m.key(), m.timestamp).await.unwrap();
    assert!(d.open.as_ref().unwrap().get(&m.id).is_none());
    assert!(d.store.message(SHOWCASE_CHAT, &m.id).unwrap().is_none());
    assert!(
        client
            .log()
            .iter()
            .any(|s| matches!(s, Sent::DeleteForMe { key } if *key == MsgKey { ..m.key() }))
    );
}

#[tokio::test]
async fn typing_presence_and_message_arrival() {
    let (client, mut stream, mut d) = setup(FakeOptions::default()).await;
    // The mock announces Carmen typing and Lucía online right after history.
    let carmen = "34633333333@s.whatsapp.net";
    assert_eq!(
        d.typing.in_chat(carmen),
        vec![(String::new(), ChatState::Composing)]
    );
    assert!(d.presence.get(SHOWCASE_CHAT).unwrap().online);
    // Group: names are prefixed.
    client.simulate_chat_state(FAMILY, Some("ana@s.whatsapp.net"), ChatState::Recording);
    drain(&mut stream, &mut d).await;
    let names: Vec<_> = d
        .typing
        .in_chat(FAMILY)
        .into_iter()
        .map(|(_, s)| ("Ana".to_string(), s))
        .collect();
    assert_eq!(
        typing_label(&names, true).unwrap(),
        "Ana is recording audio…"
    );
    // A live message from the typist ends the indicator.
    client.simulate_incoming(live_msg("T1", carmen, carmen, "ya", 1_763_600_000));
    drain(&mut stream, &mut d).await;
    assert!(d.typing.in_chat(carmen).is_empty());
    // Going offline records last seen.
    client.simulate_presence(SHOWCASE_CHAT, false, Some(1_763_000_000));
    drain(&mut stream, &mut d).await;
    let p = d.presence.get(SHOWCASE_CHAT).unwrap();
    assert!(!p.online);
    assert_eq!(p.last_seen, Some(1_763_000_000));
    // Indicators expire on their own even if `paused` is lost.
    let later = Instant::now() + Duration::from_secs(11);
    assert!(d.typing.expire(later));
    assert!(!d.typing.is_active());
}

#[tokio::test]
async fn our_typing_state_and_presence_subscription_are_sent() {
    let (client, _s, _d) = setup(FakeOptions::default()).await;
    client
        .send_chat_state(SHOWCASE_CHAT, ChatState::Composing)
        .await
        .unwrap();
    client
        .send_chat_state(SHOWCASE_CHAT, ChatState::Paused)
        .await
        .unwrap();
    client.subscribe_presence(SHOWCASE_CHAT).await.unwrap();
    let log = client.log();
    assert!(log.iter().any(|s| matches!(
        s,
        Sent::ChatState {
            state: ChatState::Composing,
            ..
        }
    )));
    assert!(log.iter().any(|s| matches!(
        s,
        Sent::ChatState {
            state: ChatState::Paused,
            ..
        }
    )));
    assert!(
        log.iter()
            .any(|s| matches!(s, Sent::Subscribed { jid } if jid == SHOWCASE_CHAT))
    );
}

#[tokio::test]
async fn group_ticks_need_every_participant() {
    let (client, mut stream, mut d) = setup(FakeOptions::default()).await;
    d.open_chat(FAMILY);
    d.group_size = Some(client.group_size(FAMILY).await.unwrap());
    assert_eq!(d.group_size, Some(3));
    let mine = d
        .store
        .load_messages(FAMILY, None, 50)
        .unwrap()
        .into_iter()
        .find(|m| m.from_me)
        .unwrap();
    let status = |d: &Driver| d.store.message(FAMILY, &mine.id).unwrap().unwrap().status;
    // Make it a fresh "Sent" message first (history gave it a final state).
    d.store
        .apply_group_receipt(FAMILY, std::slice::from_ref(&mine.id), None, None)
        .unwrap();
    let start = status(&d);
    for (i, who) in ["a@s.whatsapp.net", "b@s.whatsapp.net"].iter().enumerate() {
        client.simulate_group_receipt(FAMILY, &[&mine.id], ReceiptStatus::Read, who);
        drain(&mut stream, &mut d).await;
        assert_eq!(status(&d), start, "{} of 3 read", i + 1);
    }
    client.simulate_group_receipt(FAMILY, &[&mine.id], ReceiptStatus::Read, "c@s.whatsapp.net");
    drain(&mut stream, &mut d).await;
    assert_eq!(status(&d), ReceiptStatus::Read);
    assert_eq!(
        d.open.as_ref().unwrap().get(&mine.id).unwrap().status,
        ReceiptStatus::Read
    );
    assert_eq!(group_status(3, 3, 3), Some(ReceiptStatus::Read));
}

#[tokio::test]
async fn search_and_jump_to_result() {
    let (_c, _s, d) = setup(FakeOptions::default()).await;
    let hits = d.store.search_messages(None, "CAFÉ", 20).unwrap();
    assert!(hits.len() >= 2, "{hits:?}");
    assert!(
        hits.iter()
            .all(|h| h.snippet.to_lowercase().contains("café"))
    );
    let in_chat = d
        .store
        .search_messages(Some(SHOWCASE_CHAT), "café", 20)
        .unwrap();
    assert!(in_chat.iter().all(|h| h.chat == SHOWCASE_CHAT));
    // Jumping loads the window around the oldest hit and keeps both ends honest.
    let oldest = in_chat.last().unwrap();
    let w = d
        .store
        .load_around(SHOWCASE_CHAT, oldest.ts, &oldest.id, 3, 0)
        .unwrap();
    assert!(w.messages.iter().any(|m| m.id == oldest.id));
    let mut conv = Conversation::new(SHOWCASE_CHAT.into(), vec![], false);
    conv.replace_window(w.messages, w.has_older, w.has_newer);
    assert!(conv.get(&oldest.id).is_some());
    assert!(conv.has_newer, "the window is not at the newest end");
}

#[tokio::test]
async fn avatars_fetch_decode_and_fall_back() {
    let (client, _s, _d) = setup(FakeOptions::default()).await;
    let bytes = client
        .profile_picture(SHOWCASE_CHAT)
        .await
        .unwrap()
        .unwrap();
    let tile = avatar::prepare(&bytes).unwrap();
    assert_eq!(tile.side, avatar::TILE_PX);
    // A contact without a picture keeps the grey default.
    assert_eq!(
        client
            .profile_picture("34644444444@s.whatsapp.net")
            .await
            .unwrap(),
        None
    );
    // Garbage from a hostile server never panics.
    assert!(avatar::prepare(&[0xff; 64]).is_err());
}

#[tokio::test]
async fn stickers_and_videos_are_downloadable() {
    let (client, _s, d) = setup(FakeOptions::default()).await;
    let msgs = d.store.load_messages(SHOWCASE_CHAT, None, 200).unwrap();
    let sticker = msgs
        .iter()
        .find_map(|m| match &m.content {
            Content::Sticker { media: Some(r), .. } => Some(r.clone()),
            _ => None,
        })
        .unwrap();
    let webp = client.download(&sticker).await.unwrap();
    let (w, h, rgba) = ferrochat::media::image::decode_sticker(&webp).unwrap();
    assert_eq!((w, h), (256, 256));
    assert_eq!(rgba.len(), 256 * 256 * 4);
    assert!(rgba.chunks(4).any(|p| p[3] == 0), "transparent corners");
    let video = msgs
        .iter()
        .find_map(|m| match &m.content {
            Content::Video { media: Some(r), .. } => Some(r.clone()),
            _ => None,
        })
        .unwrap();
    assert!(!client.download(&video).await.unwrap().is_empty());
}
