//! Formatted log output never contains known secret fixtures.

use std::io::Write;
use std::sync::{Arc, Mutex};

use ferrochat::auth::secrets::Secret;
use ferrochat::error::{MaskedJid, Redacted};
use ferrochat::net::{
    ChatState, Content, Event, MediaKind, MediaRef, Message, MsgKey, Origin, Quote, Reaction,
    ReceiptStatus,
};

const BODY: &str = "my bank PIN is 4711";
const PHONE: &str = "34612345678";
const TOKEN: &str = "s3cr3t-signal-key-0123456789abcdef";
const FILE: &str = "medical-report.pdf";

#[derive(Clone, Default)]
struct Buf(Arc<Mutex<Vec<u8>>>);

impl Write for Buf {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn logs_do_not_leak_secrets() {
    let buf = Buf::default();
    let writer = buf.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .without_time()
        .with_writer(move || writer.clone())
        .finish();
    let jid = format!("{PHONE}@s.whatsapp.net");
    let msg = Message {
        id: "ID1".into(),
        chat: jid.clone(),
        sender: jid.clone(),
        sender_name: Some("Ana".into()),
        from_me: false,
        timestamp: 1,
        content: Content::Text(BODY.into()),
        status: ReceiptStatus::Read,
        expires_at: None,
        reply: None,
        edited: false,
        forwarded: None,
        mentions: Vec::new(),
        reactions: Vec::new(),
    };
    let doc = Content::Document {
        file_name: FILE.into(),
        mimetype: "application/pdf".into(),
        size: 1,
        pages: None,
        caption: Some(BODY.into()),
        media: Some(MediaRef {
            direct_path: "/p".into(),
            media_key: TOKEN.as_bytes().to_vec(),
            file_sha256: vec![],
            file_enc_sha256: vec![],
            file_length: 1,
            kind: MediaKind::Document,
            mimetype: "application/pdf".into(),
        }),
    };
    let quote = Quote {
        id: "ID0".into(),
        sender: jid.clone(),
        preview: BODY.into(),
    };
    let reaction = Reaction {
        reactor: jid.clone(),
        emoji: "👍".into(),
        ts: 1,
    };
    let key = MsgKey {
        chat: jid.clone(),
        id: "ID1".into(),
        from_me: false,
        sender: jid.clone(),
    };
    let mut replying = msg.clone();
    replying.reply = Some(quote.clone());
    replying.reactions = vec![reaction.clone()];
    let video = Content::Video {
        caption: Some(BODY.into()),
        seconds: 3,
        width: 1,
        height: 1,
        thumbnail: None,
        media: None,
        gif: false,
    };
    tracing::subscriber::with_default(subscriber, || {
        tracing::debug!(?quote, ?reaction, ?key, message = ?replying, video = ?video, "parity types");
        tracing::debug!(
            e1 = ?Event::Reaction { chat: jid.clone(), target: "ID1".into(), reactor: jid.clone(), emoji: BODY.into(), ts: 1 },
            e2 = ?Event::Edited { chat: jid.clone(), id: "ID1".into(), sender: jid.clone(), from_me: false, content: Box::new(Content::Text(BODY.into())) },
            e3 = ?Event::ChatState { chat: jid.clone(), participant: Some(jid.clone()), state: ChatState::Composing },
            e4 = ?Event::Presence { jid: jid.clone(), online: true, last_seen: Some(5) },
            e5 = ?Event::PictureChanged { jid: jid.clone() },
            e6 = ?Event::Receipt { chat: jid.clone(), ids: vec!["ID1".into()], status: ReceiptStatus::Read, participant: Some(jid.clone()), timestamp: 0 },
            e7 = ?Event::DeletedForMe { chat: jid.clone(), id: "ID1".into() },
            "net events"
        );
        tracing::info!(body = %Redacted(BODY), chat = %MaskedJid(&jid), "sending");
        tracing::info!(secret = ?Secret::new(TOKEN), "keyring");
        tracing::debug!(content = ?msg.content, doc = ?doc, "converted");
        tracing::debug!(event = ?Event::Messages { messages: vec![msg.clone()], origin: Origin::Live }, "net");
        tracing::debug!(event = ?Event::Qr { code: TOKEN.into(), valid_for_secs: 20 }, "net");
        tracing::debug!(err = %ferrochat::error::Error::Net(TOKEN.into()).user_message(), "user");
    });
    let out = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    assert!(out.contains("sending"), "logging worked: {out}");
    for needle in [BODY, PHONE, TOKEN, FILE, "4711"] {
        assert!(!out.contains(needle), "leaked {needle:?} in:\n{out}");
    }
}
