//! Integration tests against the fake protocol layer: the same event
//! stream the app consumes, applied to the store and the reducers.

use std::time::Duration;

use ferrochat::app::state::{ChatList, Conversation};
use ferrochat::mock::{self, SHOWCASE_CHAT};
use ferrochat::net::fake::{self, FakeOptions, Sent};
use std::collections::HashSet;

use ferrochat::net::{Content, Event, Message, Origin, ReceiptStatus, WaClient, new_message_id};
use ferrochat::store::Store;
use futures::StreamExt;

/// Minimal headless consumer mirroring `App::on_net` for data events.
struct Driver {
    store: Store,
    chats: ChatList,
    open: Option<Conversation>,
    qr_codes: Vec<String>,
    connected: bool,
    logged_out: bool,
}

impl Driver {
    fn new() -> Self {
        Self {
            store: Store::open_in_memory().unwrap(),
            chats: ChatList::default(),
            open: None,
            qr_codes: vec![],
            connected: false,
            logged_out: false,
        }
    }

    fn apply(&mut self, ev: Event) {
        match ev {
            Event::Qr { code, .. } => self.qr_codes.push(code),
            Event::Connected { .. } => self.connected = true,
            Event::LoggedOut { .. } => self.logged_out = true,
            Event::History { chats, names, .. } => {
                self.store.upsert_chats(&chats).unwrap();
                self.store
                    .set_contact_names(&ferrochat::net::push_names(&names))
                    .unwrap();
                self.chats.apply_history(&chats, &names);
            }
            Event::Messages { messages, origin } => {
                // Same two phases as App: persist, then count what was new.
                let new_ids: HashSet<String> = self
                    .store
                    .insert_messages(&messages)
                    .unwrap()
                    .into_iter()
                    .collect();
                let open = self.open.as_ref().map(|c| c.chat.clone());
                let fresh = (origin != Origin::History).then_some(&new_ids);
                self.chats
                    .apply_messages(&messages, open.as_deref(), true, fresh);
                if let Some(c) = &mut self.open {
                    c.merge(messages);
                }
            }
            Event::Receipt {
                chat, ids, status, ..
            } => {
                self.store.apply_receipt(&chat, &ids, status).unwrap();
                self.chats.apply_receipt(&chat, &ids, status);
                if let Some(c) = &mut self.open {
                    c.apply_receipt(&ids, status);
                }
            }
            _ => {}
        }
    }
}

async fn drain(stream: &mut (impl futures::Stream<Item = Event> + Unpin), d: &mut Driver) {
    while let Ok(Some(ev)) = tokio::time::timeout(Duration::from_millis(50), stream.next()).await {
        d.apply(ev);
    }
}

fn data() -> mock::MockData {
    mock::mock_data(1_763_500_000)
}

#[tokio::test]
async fn paired_session_syncs_history_into_store() {
    let (_client, stream) = fake::start(FakeOptions::default(), data());
    let mut stream = Box::pin(stream);
    let mut d = Driver::new();
    drain(&mut stream, &mut d).await;
    assert!(d.connected);
    let rows = d.store.chat_rows().unwrap();
    assert_eq!(rows.len(), 12);
    // Pinned showcase chat first, with its newest message as preview.
    assert_eq!(rows[0].meta.id, SHOWCASE_CHAT);
    assert!(rows[0].last.is_some());
    // This driver only applies history (push names); saved names arrive as app-state events.
    assert_eq!(d.chats.display_name(SHOWCASE_CHAT), "~ Lu");
    let msgs = d.store.load_messages(SHOWCASE_CHAT, None, 100).unwrap();
    assert!(
        msgs.iter()
            .any(|m| matches!(m.content, Content::Voice { .. }))
    );
}

#[tokio::test(start_paused = true)]
async fn qr_codes_rotate_then_exhaust_and_scan_links() {
    let opts = FakeOptions {
        paired: false,
        qr_codes: 3,
        qr_interval: Duration::from_secs(20),
        ..Default::default()
    };
    let (client, stream) = fake::start(opts, data());
    let mut stream = Box::pin(stream);
    let mut d = Driver::new();
    let mut exhausted = false;
    while let Some(ev) = stream.next().await {
        if matches!(ev, Event::QrExhausted) {
            exhausted = true;
            break;
        }
        d.apply(ev);
    }
    assert!(exhausted);
    assert_eq!(d.qr_codes.len(), 3);
    assert_ne!(d.qr_codes[0], d.qr_codes[1], "codes refresh");
    assert!(!d.connected);

    client.scan();
    drain(&mut stream, &mut d).await;
    assert!(d.connected);
    assert_eq!(d.store.chat_rows().unwrap().len(), 12);
}

#[tokio::test(start_paused = true)]
async fn sending_text_walks_the_receipt_state_machine() {
    let opts = FakeOptions {
        receipt_delay: Duration::from_millis(500),
        ..Default::default()
    };
    let (client, stream) = fake::start(opts, data());
    let mut stream = Box::pin(stream);
    let mut d = Driver::new();
    drain(&mut stream, &mut d).await;
    d.open = Some(Conversation::new(
        SHOWCASE_CHAT.into(),
        d.store.load_messages(SHOWCASE_CHAT, None, 60).unwrap(),
        false,
    ));

    let id = new_message_id();
    let local = Message {
        id: id.clone(),
        chat: SHOWCASE_CHAT.into(),
        sender: mock::OWN_JID.into(),
        sender_name: None,
        from_me: true,
        timestamp: 1_763_600_000,
        content: Content::Text("hola".into()),
        status: ReceiptStatus::Pending,
        expires_at: None,
        reply: None,
        edited: false,
        forwarded: None,
        mentions: Vec::new(),
        reactions: Vec::new(),
    };
    d.apply(Event::Messages {
        messages: vec![local],
        origin: Origin::Live,
    });
    client
        .send_text(SHOWCASE_CHAT, &id, "hola", None)
        .await
        .unwrap();

    let status = |d: &Driver| d.store.message(SHOWCASE_CHAT, &id).unwrap().unwrap().status;
    // Sent is emitted synchronously by the send call.
    drain(&mut stream, &mut d).await;
    assert_eq!(status(&d), ReceiptStatus::Sent);
    tokio::time::sleep(Duration::from_millis(510)).await;
    drain(&mut stream, &mut d).await;
    assert_eq!(status(&d), ReceiptStatus::Delivered);
    tokio::time::sleep(Duration::from_millis(510)).await;
    drain(&mut stream, &mut d).await;
    assert_eq!(status(&d), ReceiptStatus::Read);
    assert_eq!(
        d.open.as_ref().unwrap().get(&id).unwrap().status,
        ReceiptStatus::Read
    );
    assert_eq!(
        d.chats
            .get(SHOWCASE_CHAT)
            .unwrap()
            .last
            .as_ref()
            .unwrap()
            .status,
        ReceiptStatus::Read
    );
    assert!(
        client
            .log()
            .iter()
            .any(|s| matches!(s, Sent::Text { text, .. } if text == "hola"))
    );
}

#[tokio::test]
async fn failed_send_reports_error() {
    let opts = FakeOptions {
        fail_sends: true,
        ..Default::default()
    };
    let (client, _stream) = fake::start(opts, data());
    assert!(
        client
            .send_text(SHOWCASE_CHAT, "X", "x", None)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn media_roundtrip_through_fake_cdn() {
    let (client, _stream) = fake::start(FakeOptions::default(), data());
    let png = mock::sunset_png(64, 32);
    let media = ferrochat::media::image::outgoing_from_file(
        std::path::Path::new("/tmp/pic.png"),
        png.clone(),
        Some("look".into()),
    );
    let content = client
        .send_media(SHOWCASE_CHAT, "IMG1", media)
        .await
        .unwrap();
    let Content::Image {
        media: Some(r),
        width,
        caption,
        ..
    } = &content
    else {
        panic!("{content:?}");
    };
    assert_eq!(*width, 64);
    assert_eq!(caption.as_deref(), Some("look"));
    assert_eq!(client.download(r).await.unwrap(), png);

    // Mock voice notes download and decode.
    let d = data();
    let voice = d
        .messages
        .iter()
        .find_map(|m| match &m.content {
            Content::Voice { media: Some(r), .. } => Some(r.clone()),
            _ => None,
        })
        .unwrap();
    let ogg = client.download(&voice).await.unwrap();
    let pcm = ferrochat::media::audio::decode_ogg_opus(&ogg).unwrap();
    assert!(pcm.duration_ms() > 1000);
}

#[tokio::test]
async fn read_receipts_and_logout_are_recorded() {
    let (client, stream) = fake::start(FakeOptions::default(), data());
    let mut stream = Box::pin(stream);
    let mut d = Driver::new();
    drain(&mut stream, &mut d).await;
    let incoming: Vec<Message> = d
        .store
        .load_messages("120363000000000001@g.us", None, 10)
        .unwrap()
        .into_iter()
        .filter(|m| !m.from_me)
        .collect();
    for (sender, ids) in
        ferrochat::app::state::read_receipt_batches("120363000000000001@g.us", &incoming)
    {
        assert!(sender.is_some(), "group receipts carry the participant");
        client
            .mark_read("120363000000000001@g.us", sender.as_deref(), &ids)
            .await
            .unwrap();
    }
    client.logout().await.unwrap();
    drain(&mut stream, &mut d).await;
    assert!(d.logged_out);
    let log = client.log();
    assert!(log.iter().any(|s| matches!(s, Sent::Read { .. })));
    assert_eq!(log.last(), Some(&Sent::Logout));
}

#[tokio::test]
async fn live_incoming_message_bumps_unread() {
    let (client, stream) = fake::start(FakeOptions::default(), data());
    let mut stream = Box::pin(stream);
    let mut d = Driver::new();
    drain(&mut stream, &mut d).await;
    let before = d.chats.get(SHOWCASE_CHAT).unwrap().meta.unread;
    client.simulate_incoming(Message {
        id: "LIVE1".into(),
        chat: SHOWCASE_CHAT.into(),
        sender: SHOWCASE_CHAT.into(),
        sender_name: Some("Lucía".into()),
        from_me: false,
        timestamp: 1_763_600_100,
        content: Content::Text("¿Estás?".into()),
        status: ReceiptStatus::Read,
        expires_at: None,
        reply: None,
        edited: false,
        forwarded: None,
        mentions: Vec::new(),
        reactions: Vec::new(),
    });
    drain(&mut stream, &mut d).await;
    assert_eq!(d.chats.get(SHOWCASE_CHAT).unwrap().meta.unread, before + 1);
    assert_eq!(d.chats.entries[0].meta.id, SHOWCASE_CHAT);
}

#[tokio::test]
async fn offline_drain_counts_unread_once() {
    let (client, stream) = fake::start(FakeOptions::default(), data());
    let mut stream = Box::pin(stream);
    let mut d = Driver::new();
    drain(&mut stream, &mut d).await;
    let before = d.chats.get(SHOWCASE_CHAT).unwrap().meta.unread;
    let msg = |id: &str| Message {
        id: id.into(),
        chat: SHOWCASE_CHAT.into(),
        sender: SHOWCASE_CHAT.into(),
        sender_name: None,
        from_me: false,
        timestamp: 1_763_600_200,
        content: Content::Text("while you were away".into()),
        status: ReceiptStatus::Read,
        expires_at: None,
        reply: None,
        edited: false,
        forwarded: None,
        mentions: Vec::new(),
        reactions: Vec::new(),
    };
    client.simulate_offline_drain(vec![msg("OFF1"), msg("OFF2")]);
    drain(&mut stream, &mut d).await;
    assert_eq!(d.chats.get(SHOWCASE_CHAT).unwrap().meta.unread, before + 2);
    // Redelivery of the same drain does not count twice.
    client.simulate_offline_drain(vec![msg("OFF1"), msg("OFF2")]);
    drain(&mut stream, &mut d).await;
    assert_eq!(d.chats.get(SHOWCASE_CHAT).unwrap().meta.unread, before + 2);
}
