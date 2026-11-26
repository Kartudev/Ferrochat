//! Mock data used by tests, `--mock` mode and screenshot mode.
//!
//! All names, numbers and messages are invented. Media is generated
//! procedurally (no external files).

use std::collections::HashMap;
use std::io::Cursor;

use crate::media::audio;
use crate::net::social::{CommunityInfo, StatusBody, StatusItem, SubGroup};
use crate::net::types::{
    ChatMeta, ChatState, Content, LinkPreview, ListDef, ListKind, MediaKind, MediaRef, Message,
    Quote, Reaction, ReceiptStatus,
};

/// Own (fake) JID in mock mode.
pub const OWN_JID: &str = "34600000000@s.whatsapp.net";
/// The chat opened in the `chat` screenshot.
pub const SHOWCASE_CHAT: &str = "34611111111@s.whatsapp.net";

/// Everything the fake protocol client serves.
pub struct MockData {
    /// Own JID.
    pub own_jid: String,
    /// Chats.
    pub chats: Vec<ChatMeta>,
    /// Push names (what contacts call themselves).
    pub names: Vec<(String, String)>,
    /// Names saved in the (fake) phone's address book, sent as app-state
    /// contact updates after the history.
    pub saved: Vec<(String, String)>,
    /// Business verified names.
    pub business: Vec<(String, String)>,
    /// Chat lists defined on the (fake) phone.
    pub lists: Vec<ListDef>,
    /// `(list id, chat)` memberships.
    pub list_members: Vec<(String, String)>,
    /// Messages, any order.
    pub messages: Vec<Message>,
    /// Downloadable media keyed by `direct_path`.
    pub media: HashMap<String, Vec<u8>>,
    /// Profile pictures (PNG) by JID; chats without an entry have none.
    pub avatars: HashMap<String, Vec<u8>>,
    /// Typing indicators sent right after the history: `(chat, participant, state)`.
    pub typing: Vec<(String, Option<String>, ChatState)>,
    /// Presence updates sent right after the history: `(jid, online, last_seen)`.
    pub presence: Vec<(String, bool, Option<i64>)>,
    /// Statuses contacts posted (sent after the history).
    pub statuses: Vec<crate::net::social::StatusItem>,
    /// Communities the (fake) account belongs to.
    pub communities: Vec<crate::net::social::CommunityInfo>,
    /// Phone numbers (digits) that have a WhatsApp account.
    pub registered: Vec<String>,
    /// Messages starred on the (fake) phone, sent after the history.
    pub starred: Vec<crate::net::types::MsgKey>,
}

/// The "About" text a mock contact shows in the info drawer.
pub fn about_for(jid: &str) -> &'static str {
    match jid {
        SHOWCASE_CHAT => "Viajando 🌍✈️ | café ☕",
        "34622222222@s.whatsapp.net" => "Disponible",
        _ => "Hey there! I am using WhatsApp.",
    }
}

/// The description of a mock group.
pub fn group_description(jid: &str) -> &'static str {
    match jid {
        "120363000000000001@g.us" => {
            "Grupo de la familia 🏡 Cumpleaños, comidas y fotos de los peques."
        }
        _ => "Planes, fotos y avisos del grupo.",
    }
}

/// A procedurally drawn portrait-like avatar (original artwork): a coloured
/// disc background with a head and shoulders silhouette.
pub fn avatar_png(seed: u32, group: bool) -> Vec<u8> {
    let side = 160u32;
    let hue = (seed.wrapping_mul(2_654_435_761) >> 8) % 360;
    let hsv = |h: f32, s: f32, v: f32| -> [u8; 3] {
        let c = v * s;
        let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
        let m = v - c;
        let (r, g, b) = match (h / 60.0) as u32 {
            0 => (c, x, 0.0),
            1 => (x, c, 0.0),
            2 => (0.0, c, x),
            3 => (0.0, x, c),
            4 => (x, 0.0, c),
            _ => (c, 0.0, x),
        };
        [
            ((r + m) * 255.0) as u8,
            ((g + m) * 255.0) as u8,
            ((b + m) * 255.0) as u8,
        ]
    };
    let h = hue as f32;
    let img = image::RgbImage::from_fn(side, side, |x, y| {
        let (fx, fy) = (x as f32 / side as f32, y as f32 / side as f32);
        let bg = hsv(h, 0.45, 0.55 + 0.25 * fy);
        let fg = hsv(h, 0.2, 0.93);
        let dots: &[(f32, f32, f32)] = if group {
            &[(0.3, 0.42, 0.13), (0.7, 0.42, 0.13), (0.5, 0.58, 0.15)]
        } else {
            &[(0.5, 0.38, 0.17)]
        };
        let in_head = dots
            .iter()
            .any(|(cx, cy, r)| (fx - cx).powi(2) + (fy - cy).powi(2) < r * r);
        let in_body =
            !group && ((fx - 0.5).powi(2) / 0.17 + (fy - 1.0).powi(2) / 0.2) < 1.0 && fy > 0.62;
        image::Rgb(if in_head || in_body { fg } else { bg })
    });
    let mut out = Vec::new();
    let _ = img.write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png);
    out
}

/// A procedurally drawn smiley sticker as lossless WebP (original artwork).
pub fn sticker_webp() -> Vec<u8> {
    use image::ImageEncoder;
    let side = 256u32;
    let img = image::RgbaImage::from_fn(side, side, |x, y| {
        let (fx, fy) = (x as f32 / side as f32 - 0.5, y as f32 / side as f32 - 0.5);
        let r = (fx * fx + fy * fy).sqrt();
        let eye = |ex: f32| ((fx - ex).powi(2) + (fy + 0.12).powi(2)).sqrt() < 0.055;
        let mouth = {
            let d = (fx * fx + (fy - 0.02) * (fy - 0.02)).sqrt();
            (0.24..0.29).contains(&d) && fy > 0.06
        };
        if r > 0.46 {
            image::Rgba([0, 0, 0, 0])
        } else if r > 0.43 {
            image::Rgba([0xc9, 0x8a, 0x00, 255])
        } else if eye(-0.15) || eye(0.15) || mouth {
            image::Rgba([0x4a, 0x2f, 0x00, 255])
        } else {
            image::Rgba([0xff, 0xcc, 0x33, 255])
        }
    });
    let mut out = Vec::new();
    let _ = image::codecs::webp::WebPEncoder::new_lossless(&mut out).write_image(
        img.as_raw(),
        side,
        side,
        image::ExtendedColorType::Rgba8,
    );
    out
}

fn media_ref(path: &str, kind: MediaKind, mimetype: &str, bytes: &[u8]) -> MediaRef {
    MediaRef {
        direct_path: path.into(),
        media_key: vec![0x42; 32],
        file_sha256: path.as_bytes().to_vec(),
        file_enc_sha256: vec![0; 32],
        file_length: bytes.len() as u64,
        kind,
        mimetype: mimetype.into(),
    }
}

/// A procedurally drawn "sunset over the sea" PNG (original artwork).
pub fn sunset_png(w: u32, h: u32) -> Vec<u8> {
    let img = image::RgbImage::from_fn(w, h, |x, y| {
        let fy = y as f32 / h as f32;
        let fx = x as f32 / w as f32;
        let horizon = 0.62;
        let sun = ((fx - 0.5).powi(2) * 2.2 + (fy - 0.55).powi(2) * 4.0).sqrt();
        if fy < horizon {
            let t = fy / horizon;
            let mut r = 255.0 * (0.98 - 0.25 * t);
            let mut g = 120.0 + 80.0 * t;
            let mut b = 90.0 + 60.0 * (1.0 - t);
            if sun < 0.09 {
                r = 255.0;
                g = 236.0;
                b = 170.0;
            } else if sun < 0.2 {
                let k = 1.0 - (sun - 0.09) / 0.11;
                r += (255.0 - r) * k * 0.6;
                g += (210.0 - g) * k * 0.6;
            }
            image::Rgb([r as u8, g.min(255.0) as u8, b.min(255.0) as u8])
        } else {
            let t = (fy - horizon) / (1.0 - horizon);
            let wave = ((fx * 60.0 + fy * 140.0).sin() * 0.5 + 0.5) * 18.0;
            let glow = if (fx - 0.5).abs() < 0.08 * (1.0 - t) + 0.02 {
                60.0
            } else {
                0.0
            };
            image::Rgb([
                (30.0 + 60.0 * (1.0 - t) + glow + wave * 0.5) as u8,
                (50.0 + 50.0 * (1.0 - t) + glow * 0.7 + wave * 0.4) as u8,
                (90.0 + 50.0 * (1.0 - t) + wave) as u8,
            ])
        }
    });
    let mut out = Vec::new();
    let _ = img.write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png);
    out
}

fn thumbnail_of(png: &[u8]) -> Option<Vec<u8>> {
    crate::media::image::probe_and_thumbnail(png)
        .ok()
        .map(|(_, _, t)| t)
}

/// A synthetic, speech-like voice note (amplitude-modulated tones).
pub fn voice_note(seconds: f32) -> (Vec<u8>, Vec<u8>) {
    let n = (audio::OPUS_RATE as f32 * seconds) as usize;
    let rate = audio::OPUS_RATE as f32;
    let pcm: Vec<f32> = (0..n)
        .map(|i| {
            let t = i as f32 / rate;
            let syllable = ((t * 3.1).sin() * (t * 1.3).cos()).abs();
            let pitch = 180.0 + 40.0 * (t * 0.7).sin();
            let s = (t * pitch * std::f32::consts::TAU).sin() * 0.6
                + (t * pitch * 2.0 * std::f32::consts::TAU).sin() * 0.25;
            s * syllable * 0.5
        })
        .collect();
    let wf = audio::waveform(&pcm);
    let ogg = audio::encode_ogg_opus(&pcm).unwrap_or_default();
    (ogg, wf)
}

/// A tiny valid PDF.
pub fn sample_pdf() -> Vec<u8> {
    b"%PDF-1.4\n1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj\n3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 612 792]>>endobj\ntrailer<</Root 1 0 R>>\n%%EOF\n".to_vec()
}

struct B {
    msgs: Vec<Message>,
    n: u32,
}

impl B {
    fn push(
        &mut self,
        chat: &str,
        from: Option<(&str, &str)>,
        ts: i64,
        c: Content,
        st: ReceiptStatus,
    ) {
        self.n += 1;
        let (sender, name, from_me) = match from {
            Some((jid, name)) => (jid.to_string(), Some(name.to_string()), false),
            None => (OWN_JID.to_string(), None, true),
        };
        self.msgs.push(Message {
            id: format!("MOCK{:04}", self.n),
            chat: chat.into(),
            sender,
            sender_name: name,
            from_me,
            timestamp: ts,
            content: c,
            status: if from_me { st } else { ReceiptStatus::Read },
            expires_at: None,
            reply: None,
            edited: false,
            forwarded: None,
            mentions: Vec::new(),
            reactions: Vec::new(),
        });
    }

    /// Marks the message pushed last as forwarded with this score.
    fn forwarded(&mut self, score: u32) {
        if let Some(m) = self.msgs.last_mut() {
            m.forwarded = Some(score);
        }
    }

    /// Id of the message pushed last.
    fn last_id(&self) -> String {
        self.msgs.last().map(|m| m.id.clone()).unwrap_or_default()
    }

    /// Quote of an earlier message (by id) for the one pushed last.
    fn reply_to_earlier(&mut self, id: &str) {
        let q = self.msgs.iter().find(|m| m.id == id).map(Quote::of);
        if let Some(m) = self.msgs.last_mut() {
            m.reply = q;
        }
    }

    /// Adds a reaction to the message pushed last.
    fn react(&mut self, reactor: &str, emoji: &str, ts: i64) {
        if let Some(m) = self.msgs.last_mut() {
            m.reactions.push(Reaction {
                reactor: reactor.into(),
                emoji: emoji.into(),
                ts,
            });
        }
    }

    fn mark_edited(&mut self) {
        if let Some(m) = self.msgs.last_mut() {
            m.edited = true;
        }
    }
}

fn text(s: &str) -> Content {
    Content::Text(s.into())
}

/// Builds the mock dataset relative to `now` (unix seconds).
pub fn mock_data(now: i64) -> MockData {
    use ReceiptStatus::*;
    let mut media = HashMap::new();
    let lucia = SHOWCASE_CHAT;
    let family = "120363000000000001@g.us";
    let marcos = "34622222222@s.whatsapp.net";
    let design = "120363000000000002@g.us";
    let carmen = "34633333333@s.whatsapp.net";
    let clinic = "34644444444@s.whatsapp.net";
    let pablo = "34655555555@s.whatsapp.net";
    let climbing = "120363000000000003@g.us";
    let elena = "34666666666@s.whatsapp.net";
    let mum = "34677777777@s.whatsapp.net";
    let archived_ex = "34688000111@s.whatsapp.net";
    let archived_group = "120363000000000004@g.us";

    // What the contacts call themselves (push names) ...
    let names = vec![
        (lucia.to_string(), "Lu".to_string()),
        (marcos.to_string(), "Marcos".to_string()),
        (carmen.to_string(), "Carmen R.".to_string()),
        (pablo.to_string(), "Pablo".to_string()),
        (elena.to_string(), "Ele".to_string()),
        (mum.to_string(), "Mama".to_string()),
        (
            "34612121212@s.whatsapp.net".to_string(),
            "Jorge".to_string(),
        ),
        (archived_ex.to_string(), "Álvaro".to_string()),
    ];
    // ... and what the user saved them as on the phone (app-state sync).
    let saved = vec![
        (lucia.to_string(), "Lucía Fernández".to_string()),
        (marcos.to_string(), "Marcos Díaz".to_string()),
        (carmen.to_string(), "Carmen Ruiz".to_string()),
        (elena.to_string(), "Elena Martín".to_string()),
        (mum.to_string(), "Mamá".to_string()),
        (
            "34688888888@s.whatsapp.net".to_string(),
            "Tía Rosa".to_string(),
        ),
        (
            "34699999999@s.whatsapp.net".to_string(),
            "Irene (UX)".to_string(),
        ),
    ];
    let business = vec![(clinic.to_string(), "Clínica Dental Sonrisa".to_string())];
    let list = |id: &str, name: &str, kind: ListKind, order: i32| ListDef {
        id: id.into(),
        name: name.into(),
        color: order,
        kind,
        order,
    };
    let lists = vec![
        list("fav", "", ListKind::Favourites, 0),
        list("1", "Familia", ListKind::Custom, 1),
        list("2", "Trabajo", ListKind::Custom, 2),
        list("3", "Viajes", ListKind::Custom, 3),
    ];
    let list_members = vec![
        ("fav".to_string(), lucia.to_string()),
        ("fav".to_string(), mum.to_string()),
        ("1".to_string(), family.to_string()),
        ("1".to_string(), mum.to_string()),
        ("2".to_string(), design.to_string()),
        ("3".to_string(), lucia.to_string()),
    ];

    let png = sunset_png(640, 420);
    let img_ref = media_ref("/mock/sunset.png", MediaKind::Image, "image/png", &png);
    let thumb = thumbnail_of(&png);
    media.insert(img_ref.direct_path.clone(), png);

    let pdf = sample_pdf();
    let pdf_ref = media_ref(
        "/mock/itinerary.pdf",
        MediaKind::Document,
        "application/pdf",
        &pdf,
    );
    let pdf_len = pdf.len() as u64;
    media.insert(pdf_ref.direct_path.clone(), pdf);

    let (ogg_in, wf_in) = voice_note(9.0);
    let voice_in = media_ref(
        "/mock/voice-in.ogg",
        MediaKind::Audio,
        "audio/ogg; codecs=opus",
        &ogg_in,
    );
    media.insert(voice_in.direct_path.clone(), ogg_in);
    let (ogg_out, wf_out) = voice_note(4.0);
    let voice_out = media_ref(
        "/mock/voice-out.ogg",
        MediaKind::Audio,
        "audio/ogg; codecs=opus",
        &ogg_out,
    );
    media.insert(voice_out.direct_path.clone(), ogg_out);

    // A real 3-second clip shaped like a phone video (H.264 High profile with
    // B-frames + AAC in MP4, 40 kB) so the in-app player works in mock mode.
    let clip = include_bytes!("../assets/mock-clip.mp4").to_vec();
    let clip_ref = media_ref("/mock/clip.mp4", MediaKind::Video, "video/mp4", &clip);
    media.insert(clip_ref.direct_path.clone(), clip);
    let clip_thumb = thumbnail_of(&sunset_png(320, 180));
    let webp = sticker_webp();
    let sticker_ref = media_ref("/mock/smile.webp", MediaKind::Sticker, "image/webp", &webp);
    media.insert(sticker_ref.direct_path.clone(), webp);

    let today = now - now.rem_euclid(86_400) + 9 * 3600;
    let mut b = B {
        msgs: Vec::new(),
        n: 0,
    };
    let l = Some((lucia, "Lucía"));
    // Showcase conversation: every bubble kind and every tick state.
    b.push(
        lucia,
        l,
        now - 86_400 - 3000,
        text("Hola! ¿Ya tienes los billetes para Lisboa? ✈️"),
        Read,
    );
    b.push(
        lucia,
        None,
        now - 86_400 - 2900,
        text("Sí, salimos el viernes a las 8:15"),
        Read,
    );
    b.push(
        lucia,
        None,
        now - 86_400 - 2890,
        text("Te paso el itinerario"),
        Read,
    );
    b.push(
        lucia,
        None,
        now - 86_400 - 2880,
        Content::Document {
            file_name: "Itinerario Lisboa.pdf".into(),
            mimetype: "application/pdf".into(),
            size: pdf_len.max(184_320),
            pages: Some(3),
            caption: None,
            media: Some(pdf_ref.clone()),
        },
        Read,
    );
    b.push(
        lucia,
        l,
        now - 3600,
        text("Perfecto, ¡gracias! Mira qué atardecer hubo ayer en la playa"),
        Read,
    );
    b.push(
        lucia,
        l,
        now - 3580,
        Content::Image {
            caption: Some("Desde el paseo marítimo 🌅".into()),
            width: 640,
            height: 420,
            thumbnail: thumb.clone(),
            media: Some(img_ref.clone()),
        },
        Read,
    );
    b.push(
        lucia,
        l,
        now - 3500,
        Content::Voice {
            seconds: 9,
            ptt: true,
            waveform: wf_in,
            media: Some(voice_in),
        },
        Read,
    );
    b.push(
        lucia,
        None,
        now - 1800,
        text("¡Qué pasada! Me encanta 😍"),
        Read,
    );
    b.push(
        lucia,
        None,
        now - 1700,
        Content::Voice {
            seconds: 4,
            ptt: true,
            waveform: wf_out,
            media: Some(voice_out),
        },
        Delivered,
    );
    b.push(
        lucia,
        None,
        now - 240,
        text("Nos vemos mañana en la estación, llevo yo el café ☕"),
        Sent,
    );

    // Replies, reactions, video, sticker and an edited message.
    let coffee_id = b.last_id();
    b.push(
        lucia,
        l,
        now - 200,
        text("Perfecto, yo llevo los bollos 🥐"),
        Read,
    );
    b.reply_to_earlier(&coffee_id);
    b.react(OWN_JID, "❤️", now - 150);
    b.push(
        lucia,
        l,
        now - 3450,
        Content::Video {
            caption: None,
            seconds: 3,
            width: 320,
            height: 180,
            thumbnail: clip_thumb.clone(),
            media: Some(clip_ref.clone()),
            gif: false,
        },
        Read,
    );
    b.react(OWN_JID, "😂", now - 3400);
    b.push(
        lucia,
        l,
        now - 1650,
        Content::Sticker {
            width: 256,
            height: 256,
            animated: false,
            media: Some(sticker_ref),
        },
        Read,
    );
    b.push(
        lucia,
        None,
        now - 130,
        text("Llevo yo el café, entonces. ¡Hasta mañana! ☕"),
        Delivered,
    );
    b.mark_edited();
    b.react(lucia, "👍", now - 100);

    b.push(
        lucia,
        l,
        now - 90,
        text("Mira esto https://www.rust-lang.org/learn y también www.example.com."),
        Read,
    );
    b.forwarded(0);
    b.push(
        lucia,
        None,
        now - 80,
        Content::Link {
            text: "Esta guía de viaje es buena: https://example.com/lisboa".into(),
            preview: LinkPreview {
                url: "https://example.com/lisboa".into(),
                title: "Qué ver en Lisboa en tres días".into(),
                description: "Barrios, miradores y los mejores pastéis de nata de la ciudad."
                    .into(),
                thumbnail: thumbnail_of(&sunset_png(320, 180)),
            },
        },
        Sent,
    );

    let f1 = Some(("34688888888@s.whatsapp.net", "Tía Rosa"));
    let f2 = Some((mum, "Mamá"));
    b.push(
        family,
        f2,
        now - 7200,
        text("¿Quién viene el domingo a comer?"),
        Read,
    );
    b.push(family, None, now - 7000, text("Yo sí 🙋"), Read);
    b.push(family, f1, now - 900, text("Yo llevo el postre"), Read);
    b.push(family, f2, now - 600, text("Genial, hago paella"), Read);
    b.push(family, f1, now - 420, text("🥘🥘🥘"), Read);
    b.react(mum, "😋", now - 400);
    b.react(OWN_JID, "😋", now - 390);
    b.react("34688888888@s.whatsapp.net", "❤️", now - 380);
    let rice_id = b
        .msgs
        .iter()
        .rev()
        .nth(1)
        .map(|m| m.id.clone())
        .unwrap_or_default();
    b.push(
        family,
        None,
        now - 300,
        text("¡Yo llevo las bebidas!"),
        Read,
    );
    b.reply_to_earlier(&rice_id);

    b.push(
        marcos,
        Some((marcos, "Marcos")),
        today - 86_400 * 2,
        text("¿Partido el jueves?"),
        Read,
    );
    b.push(
        marcos,
        None,
        today - 86_400 * 2 + 60,
        text("Claro, a las 20:00"),
        Read,
    );
    b.push(
        marcos,
        Some((marcos, "Marcos")),
        now - 5400,
        Content::Image {
            caption: None,
            width: 640,
            height: 420,
            thumbnail: thumb,
            media: Some(img_ref),
        },
        Read,
    );

    let d1 = Some(("34699999999@s.whatsapp.net", "Irene (UX)"));
    b.push(
        design,
        d1,
        now - 86_400 - 600,
        text("He subido los nuevos mockups al drive"),
        Read,
    );
    b.push(
        design,
        None,
        now - 86_400 - 500,
        text("Los reviso esta tarde 👀"),
        Delivered,
    );

    b.push(
        carmen,
        Some((carmen, "Carmen")),
        now - 86_400 * 3,
        text("Te llamo luego"),
        Read,
    );
    b.push(carmen, None, now - 86_400 * 3 + 30, text("Vale 👍"), Read);

    b.push(
        clinic,
        Some((clinic, "Clínica Dental Sonrisa")),
        now - 86_400 * 4,
        text("Le recordamos su cita el martes a las 10:30."),
        Read,
    );
    b.push(
        pablo,
        None,
        now - 86_400 * 5,
        text("¡Feliz cumpleaños, Pablo! 🎂"),
        Read,
    );
    b.push(
        pablo,
        Some((pablo, "Pablo")),
        now - 86_400 * 5 + 300,
        text("¡Muchas gracias!"),
        Read,
    );
    b.push(
        climbing,
        Some(("34612121212@s.whatsapp.net", "Jorge")),
        now - 86_400 * 6,
        text("Sábado en el rocódromo a las 11"),
        Read,
    );
    b.push(
        elena,
        None,
        now - 86_400 * 8,
        text("Te devuelvo el libro la semana que viene"),
        Read,
    );
    b.push(
        mum,
        Some((mum, "Mamá")),
        now - 86_400 * 9,
        text("Llámame cuando puedas ❤️"),
        Read,
    );

    b.push(
        archived_ex,
        Some((archived_ex, "Álvaro")),
        now - 86_400 * 30,
        text("Nos vemos en la boda 👋"),
        Read,
    );
    b.push(
        archived_group,
        Some(("34612121212@s.whatsapp.net", "Jorge")),
        now - 86_400 * 40,
        text("Recordad pagar el alquiler antes del día 5"),
        Read,
    );

    let last_of = |chat: &str| {
        b.msgs
            .iter()
            .filter(|m| m.chat == chat)
            .map(|m| m.timestamp)
            .max()
            .unwrap_or(now)
    };
    let chat = |id: &str, name: Option<&str>, unread: u32, pinned: bool, muted: bool| ChatMeta {
        id: id.into(),
        name: name.map(Into::into),
        unread,
        last_ts: last_of(id),
        pinned,
        archived: false,
        mute_end: if muted { crate::net::MUTE_FOREVER } else { 0 },
    };
    let mut chats = vec![
        chat(lucia, None, 0, true, false),
        chat(family, Some("Familia 🏡"), 3, false, false),
        chat(marcos, None, 1, false, false),
        chat(design, Some("Equipo de Diseño"), 0, false, true),
        chat(carmen, None, 0, false, false),
        chat(clinic, None, 0, false, false),
        chat(pablo, None, 0, false, false),
        chat(climbing, Some("Escalada los sábados 🧗"), 12, false, true),
        chat(elena, None, 0, false, false),
        chat(mum, None, 0, false, false),
        chat(archived_ex, None, 0, false, false),
        chat(
            archived_group,
            Some("Piso Madrid (antiguo)"),
            0,
            false,
            true,
        ),
    ];
    for c in &mut chats {
        c.archived = c.id == archived_ex || c.id == archived_group;
    }

    let mut avatars = HashMap::new();
    for (i, jid) in [lucia, marcos, mum, carmen, elena, family, climbing]
        .into_iter()
        .enumerate()
    {
        avatars.insert(
            jid.to_string(),
            avatar_png(i as u32 + 3, jid.ends_with("@g.us")),
        );
    }
    avatars.insert(OWN_JID.to_string(), avatar_png(99, false));

    // Statuses of the last day.
    let status_png = sunset_png(450, 800);
    let status_img = media_ref(
        "/mock/status-sunset.png",
        MediaKind::Image,
        "image/png",
        &status_png,
    );
    let status_thumb = thumbnail_of(&status_png);
    media.insert(status_img.direct_path.clone(), status_png);
    // Statuses expire by the real clock, whatever `now` the chats use.
    let status_now = chrono::Utc::now().timestamp();
    let st = |id: &str, author: &str, name: &str, ago: i64, body: StatusBody| StatusItem {
        id: id.into(),
        author: author.into(),
        author_name: Some(name.into()).filter(|n: &String| !n.is_empty()),
        from_me: false,
        timestamp: status_now - ago,
        body,
    };
    let stext = |t: &str, bg: u32, font: i32| StatusBody::Text {
        text: t.into(),
        background: bg,
        font,
    };
    let statuses = vec![
        st(
            "ST1",
            marcos,
            "Marcos",
            3 * 3600,
            StatusBody::Image {
                caption: Some("Atardecer en la playa 🌅".into()),
                width: 450,
                height: 800,
                thumbnail: status_thumb.clone(),
                media: Some(status_img.clone()),
            },
        ),
        st(
            "ST2",
            lucia,
            "Lu",
            2 * 3600,
            stext("¡Ya tengo los billetes! ✈️", 0xFF1E_6E4F, 6),
        ),
        st(
            "ST3",
            lucia,
            "Lu",
            3600,
            StatusBody::Video {
                caption: Some("Ensayo del viernes".into()),
                seconds: 3,
                width: 320,
                height: 180,
                thumbnail: clip_thumb.clone(),
                media: Some(clip_ref.clone()),
            },
        ),
        st(
            "ST4",
            carmen,
            "Carmen R.",
            45 * 60,
            stext("Domingo de paella y siesta", 0xFF8E_24AA, 8),
        ),
        st(
            "ST5",
            mum,
            "Mama",
            20 * 3600,
            stext("Buenos días, familia 💛", 0xFF0D_47A1, 0),
        ),
        st("ST6", elena, "Ele", 10 * 60, {
            StatusBody::Image {
                caption: None,
                width: 450,
                height: 800,
                thumbnail: status_thumb,
                media: Some(status_img),
            }
        }),
    ];

    let sg = |jid: &str, name: &str, ann: bool, gen_: bool, joined: bool, n: u32| SubGroup {
        jid: jid.into(),
        name: name.into(),
        announcement: ann,
        general: gen_,
        joined,
        members: Some(n),
    };
    let communities = vec![
        CommunityInfo {
            jid: "120363111000000001@g.us".into(),
            name: "Vecinos de la calle Mayor".into(),
            description: Some("Avisos y charla del barrio".into()),
            members: Some(86),
            subgroups: vec![
                sg("120363111000000002@g.us", "Avisos", true, false, true, 86),
                sg("120363111000000003@g.us", "General", false, true, true, 71),
                sg(
                    "120363111000000004@g.us",
                    "Huerto urbano",
                    false,
                    false,
                    true,
                    18,
                ),
                sg(
                    "120363111000000005@g.us",
                    "Compraventa",
                    false,
                    false,
                    false,
                    40,
                ),
            ],
        },
        CommunityInfo {
            jid: "120363222000000001@g.us".into(),
            name: "Club de escalada".into(),
            description: None,
            members: Some(142),
            subgroups: vec![
                sg("120363222000000002@g.us", "Avisos", true, false, true, 142),
                sg(
                    "120363222000000003@g.us",
                    "Salidas del finde",
                    false,
                    false,
                    true,
                    55,
                ),
            ],
        },
    ];
    let star_key = |needle: &str| {
        b.msgs
            .iter()
            .find(|m| matches!(&m.content, Content::Text(t) if t.contains(needle)))
            .map(|m| crate::net::types::MsgKey {
                chat: m.chat.clone(),
                id: m.id.clone(),
                from_me: m.from_me,
                sender: if m.from_me {
                    String::new()
                } else {
                    m.sender.clone()
                },
            })
    };
    let starred: Vec<_> = [
        "Hola! ¿Ya tienes los billetes",
        "Sábado en el rocódromo",
        "He subido los nuevos mockups",
        "Yo llevo el postre",
    ]
    .iter()
    .filter_map(|n| star_key(n))
    .collect();
    let registered = vec!["34600555123".to_string(), "34611111111".to_string()];

    // Profiling aid: `FERROCHAT_MOCK_MESSAGES=600 ferrochat --mock` adds a
    // long scroll-back to the showcase chat.
    if let Some(n) = std::env::var("FERROCHAT_MOCK_MESSAGES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
    {
        b.msgs
            .extend(long_history(lucia, n.min(20_000), now - 30 * 86_400));
    }

    MockData {
        own_jid: OWN_JID.into(),
        chats,
        names,
        saved,
        business,
        lists,
        list_members,
        messages: b.msgs,
        media,
        avatars,
        typing: vec![(carmen.to_string(), None, ChatState::Composing)],
        presence: vec![(lucia.to_string(), true, None)],
        statuses,
        communities,
        registered,
        starred,
    }
}

/// `n` older text messages (varied lengths, links, mentions), the newest
/// ending at `until`. Used to profile scrolling.
pub fn long_history(chat: &str, n: usize, until: i64) -> Vec<Message> {
    const LINES: [&str; 6] = [
        "ok",
        "¿Quedamos mañana a las siete en la puerta del cine?",
        "Mira esto: https://example.com/articulo/sobre-escalada-en-roca y dime qué te parece 😄",
        "Vale, perfecto. Llevo yo las entradas y tú las palomitas, que la última vez me tocó a mí pagar todo.",
        "Una frase larga que ocupa varias líneas del globo de mensaje para medir cómo se comporta el texto cuando el panel es estrecho y hay que partirlo en muchas líneas distintas sin perder el ritmo de lectura.",
        "jajaja 😂😂",
    ];
    (0..n)
        .map(|i| {
            let from_me = i % 3 == 0;
            Message {
                id: format!("LONG{i:06}"),
                chat: chat.into(),
                sender: if from_me { OWN_JID.into() } else { chat.into() },
                sender_name: None,
                from_me,
                timestamp: until - ((n - i) as i64) * 600,
                content: Content::Text(LINES[(i * 7 + i / 3) % LINES.len()].into()),
                status: ReceiptStatus::Read,
                expires_at: None,
                reply: None,
                edited: false,
                forwarded: None,
                mentions: Vec::new(),
                reactions: Vec::new(),
            }
        })
        .collect()
}

/// A small animated GIF (a bouncing ball, 12 frames) for the "attach-gif"
/// screenshot. Empty when encoding fails.
pub fn sample_gif() -> Vec<u8> {
    use image::codecs::gif::{GifEncoder, Repeat};
    let (w, h) = (320u32, 200u32);
    let mut out = Vec::new();
    {
        let mut enc = GifEncoder::new(&mut out);
        if enc.set_repeat(Repeat::Infinite).is_err() {
            return Vec::new();
        }
        for i in 0..12u32 {
            let t = i as f32 / 12.0;
            let cx = 40.0 + t * 240.0;
            let cy = 150.0 - (t * std::f32::consts::PI).sin() * 100.0;
            let img = image::RgbaImage::from_fn(w, h, |x, y| {
                let d = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt();
                if d < 22.0 {
                    image::Rgba([255, 196, 0, 255])
                } else if y > 176 {
                    image::Rgba([60, 150, 90, 255])
                } else {
                    image::Rgba([120, 180, 235, 255])
                }
            });
            let frame =
                image::Frame::from_parts(img, 0, 0, image::Delay::from_numer_denom_ms(80, 1));
            if enc.encode_frame(frame).is_err() {
                return Vec::new();
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_data_is_consistent() {
        let d = mock_data(1_700_000_000);
        assert_eq!(d.chats.len(), 12);
        assert_eq!(d.chats.iter().filter(|c| c.archived).count(), 2);
        // Every message belongs to a known chat; every media ref is downloadable.
        for m in &d.messages {
            assert!(d.chats.iter().any(|c| c.id == m.chat), "{}", m.chat);
            if let Some(r) = m.content.media() {
                assert!(d.media.contains_key(&r.direct_path));
            }
        }
        let showcase: Vec<_> = d
            .messages
            .iter()
            .filter(|m| m.chat == SHOWCASE_CHAT)
            .collect();
        assert!(showcase.iter().any(|m| m.reply.is_some()));
        assert!(showcase.iter().any(|m| !m.reactions.is_empty()));
        assert!(showcase.iter().any(|m| m.edited));
        for tag in ["text", "image", "document", "voice", "video", "sticker"] {
            assert!(
                showcase.iter().any(|m| m.content.kind_tag() == tag),
                "{tag}"
            );
        }
    }

    #[test]
    fn voice_note_decodes() {
        let (ogg, wf) = voice_note(1.0);
        assert_eq!(wf.len(), 64);
        let pcm = audio::decode_ogg_opus(&ogg).unwrap();
        assert!(pcm.duration_ms() >= 950);
    }
}
