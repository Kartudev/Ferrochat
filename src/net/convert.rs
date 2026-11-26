//! Conversion between `whatsapp-rust` / protobuf types and domain types.
//!
//! Everything here is pure and unit-tested; the live client in
//! [`crate::net::whatsapp`] only glues these functions to the library.

use whatsapp_rust::wacore::types::presence::ReceiptType;
use whatsapp_rust::waproto::whatsapp as wa;

use super::Event;
use super::types::{
    ChatFlag, ChatMeta, Content, LinkPreview, ListDef, ListKind, MUTE_FOREVER, MUTE_OFF, MediaKind,
    MediaRef, Message, MsgKey, NameSource, OutgoingKind, OutgoingMedia, QUOTE_PREVIEW_MAX, Quote,
    Reaction, ReceiptStatus, mute_active,
};

/// Unwraps ephemeral / view-once / edited / caption wrappers (up to a few
/// levels; WhatsApp nests them).
pub fn base_message(msg: &wa::Message) -> &wa::Message {
    let mut cur = msg;
    for _ in 0..4 {
        let inner = cur
            .ephemeral_message
            .as_option()
            .or(cur.view_once_message.as_option())
            .or(cur.document_with_caption_message.as_option())
            // History stores an edited message as its latest content.
            .or(cur.edited_message.as_option())
            .and_then(|f| f.message.as_option());
        match inner {
            Some(m) => cur = m,
            None => break,
        }
    }
    cur
}

fn non_empty(s: Option<&String>) -> Option<String> {
    s.filter(|s| !s.is_empty()).cloned()
}

fn media_ref(
    kind: MediaKind,
    direct_path: Option<&String>,
    media_key: Option<&Vec<u8>>,
    file_sha256: Option<&Vec<u8>>,
    file_enc_sha256: Option<&Vec<u8>>,
    file_length: Option<u64>,
    mimetype: Option<&String>,
) -> Option<MediaRef> {
    let direct_path = direct_path.filter(|p| !p.is_empty())?;
    let media_key = media_key.filter(|k| !k.is_empty())?;
    Some(MediaRef {
        direct_path: direct_path.clone(),
        media_key: media_key.clone(),
        file_sha256: file_sha256.cloned().unwrap_or_default(),
        file_enc_sha256: file_enc_sha256.cloned().unwrap_or_default(),
        file_length: file_length.unwrap_or_default(),
        kind,
        mimetype: mimetype.cloned().unwrap_or_default(),
    })
}

/// True when the message (or any wrapper) is a view-once message.
pub fn is_view_once(msg: &wa::Message) -> bool {
    let mut cur = msg;
    for _ in 0..4 {
        if cur.view_once_message.is_set() {
            return true;
        }
        if cur.image_message.as_option().and_then(|m| m.view_once) == Some(true)
            || cur.video_message.as_option().and_then(|m| m.view_once) == Some(true)
            || cur.audio_message.as_option().and_then(|m| m.view_once) == Some(true)
        {
            return true;
        }
        match cur
            .ephemeral_message
            .as_option()
            .or(cur.document_with_caption_message.as_option())
            .or(cur.edited_message.as_option())
            .and_then(|f| f.message.as_option())
        {
            Some(m) => cur = m,
            None => return false,
        }
    }
    false
}

/// Converts a decrypted protobuf message body into [`Content`].
///
/// Returns `None` for messages that are not shown as bubbles (reactions,
/// key distribution, protocol messages other than "delete for everyone", ...).
pub fn content_from_wa(msg: &wa::Message) -> Option<Content> {
    if is_view_once(msg) {
        // Never expose the media reference: not downloaded, not cached.
        return Some(Content::ViewOnce);
    }
    let m = base_message(msg);

    if let Some(text) = non_empty(m.conversation.as_ref()) {
        return Some(Content::Text(text));
    }
    if let Some(ext) = m.extended_text_message.as_option()
        && let Some(text) = non_empty(ext.text.as_ref())
    {
        return Some(match link_preview(ext) {
            Some(preview) => Content::Link { text, preview },
            None => Content::Text(text),
        });
    }
    if let Some(img) = m.image_message.as_option() {
        return Some(Content::Image {
            caption: non_empty(img.caption.as_ref()),
            width: img.width.unwrap_or_default(),
            height: img.height.unwrap_or_default(),
            thumbnail: img.jpeg_thumbnail.clone().filter(|t| !t.is_empty()),
            media: media_ref(
                MediaKind::Image,
                img.direct_path.as_ref(),
                img.media_key.as_ref(),
                img.file_sha256.as_ref(),
                img.file_enc_sha256.as_ref(),
                img.file_length,
                img.mimetype.as_ref(),
            ),
        });
    }
    if let Some(doc) = m.document_message.as_option() {
        let file_name = non_empty(doc.file_name.as_ref())
            .or_else(|| non_empty(doc.title.as_ref()))
            .unwrap_or_else(|| "Document".into());
        return Some(Content::Document {
            file_name,
            mimetype: doc.mimetype.clone().unwrap_or_default(),
            size: doc.file_length.unwrap_or_default(),
            pages: doc.page_count.filter(|p| *p > 0),
            caption: non_empty(doc.caption.as_ref()),
            media: media_ref(
                MediaKind::Document,
                doc.direct_path.as_ref(),
                doc.media_key.as_ref(),
                doc.file_sha256.as_ref(),
                doc.file_enc_sha256.as_ref(),
                doc.file_length,
                doc.mimetype.as_ref(),
            ),
        });
    }
    if let Some(audio) = m.audio_message.as_option() {
        return Some(Content::Voice {
            seconds: audio.seconds.unwrap_or_default(),
            ptt: audio.ptt.unwrap_or(false),
            waveform: audio
                .waveform
                .iter()
                .flatten()
                .take(64)
                .map(|v| (*v).min(100))
                .collect(),
            media: media_ref(
                MediaKind::Audio,
                audio.direct_path.as_ref(),
                audio.media_key.as_ref(),
                audio.file_sha256.as_ref(),
                audio.file_enc_sha256.as_ref(),
                audio.file_length,
                audio.mimetype.as_ref(),
            ),
        });
    }
    if let Some(p) = m.protocol_message.as_option() {
        use wa::message::protocol_message::Type;
        return match p.r#type {
            Some(Type::REVOKE) => Some(Content::Deleted),
            _ => None,
        };
    }
    if let Some(v) = m.video_message.as_option() {
        return Some(video_content(v));
    }
    if let Some(v) = m.ptv_message.as_option() {
        // Round "video notes" travel as a video message.
        return Some(video_content(v));
    }
    if let Some(st) = m.sticker_message.as_option() {
        return Some(Content::Sticker {
            width: st.width.unwrap_or_default(),
            height: st.height.unwrap_or_default(),
            animated: st.is_animated.unwrap_or(false),
            media: media_ref(
                MediaKind::Sticker,
                st.direct_path.as_ref(),
                st.media_key.as_ref(),
                st.file_sha256.as_ref(),
                st.file_enc_sha256.as_ref(),
                st.file_length,
                st.mimetype.as_ref(),
            ),
        });
    }
    if m.location_message.is_set() || m.live_location_message.is_set() {
        return Some(Content::Unsupported("location".into()));
    }
    if m.contact_message.is_set() || m.contacts_array_message.is_set() {
        return Some(Content::Unsupported("contact card".into()));
    }
    if m.poll_creation_message.is_set() {
        return Some(Content::Unsupported("poll".into()));
    }
    if let Some(c) = m.call_log_messsage.as_option() {
        return Some(Content::Call(super::call::log_from_message(c)));
    }
    None
}

fn video_content(v: &wa::message::VideoMessage) -> Content {
    Content::Video {
        caption: non_empty(v.caption.as_ref()),
        seconds: v.seconds.unwrap_or_default(),
        width: v.width.unwrap_or_default(),
        height: v.height.unwrap_or_default(),
        thumbnail: v.jpeg_thumbnail.clone().filter(|t| !t.is_empty()),
        media: media_ref(
            MediaKind::Video,
            v.direct_path.as_ref(),
            v.media_key.as_ref(),
            v.file_sha256.as_ref(),
            v.file_enc_sha256.as_ref(),
            v.file_length,
            v.mimetype.as_ref(),
        ),
        gif: v.gif_playback.unwrap_or_default(),
    }
}

/// Largest inline preview thumbnail kept (they are a few kB in practice).
const MAX_PREVIEW_THUMB: usize = 64 * 1024;

/// The link preview the sender attached to an extended text message. Only
/// http(s) previews with a title count; the thumbnail is size-capped.
pub fn link_preview(ext: &wa::message::ExtendedTextMessage) -> Option<LinkPreview> {
    let title = non_empty(ext.title.as_ref())?;
    let url = non_empty(ext.matched_text.as_ref())?;
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) || url.len() > 2048 {
        return None;
    }
    Some(LinkPreview {
        url,
        title,
        description: non_empty(ext.description.as_ref()).unwrap_or_default(),
        thumbnail: ext
            .jpeg_thumbnail
            .clone()
            .filter(|t| !t.is_empty() && t.len() <= MAX_PREVIEW_THUMB),
    })
}

/// A saved-contact sync action → name events. The address-book name is the
/// full name, else the first name; an action without either removes the saved
/// name. The action may carry both the phone-number and the LID id of the
/// contact: the name is recorded under each, and the pair is learned.
pub fn contact_events(jid: &str, a: &wa::sync_action_value::ContactAction) -> Vec<Event> {
    let name = non_empty(a.full_name.as_ref())
        .or_else(|| non_empty(a.first_name.as_ref()))
        .unwrap_or_default();
    let mut ids = vec![bare_jid(jid)];
    let pn = a.pn_jid.as_deref().filter(|j| !j.is_empty()).map(bare_jid);
    let lid = a.lid_jid.as_deref().filter(|j| !j.is_empty()).map(bare_jid);
    for extra in pn.iter().chain(lid.iter()) {
        if !ids.contains(extra) {
            ids.push(extra.clone());
        }
    }
    let mut out = Vec::new();
    if let (Some(pn), Some(lid)) = (&pn, &lid)
        && let Some(pair) = lid_pair(lid, Some(pn))
    {
        out.push(Event::LidMappings(vec![pair]));
    }
    out.extend(ids.into_iter().map(|jid| Event::ContactName {
        jid,
        name: name.clone(),
        source: NameSource::Saved,
    }));
    out
}

/// A `label_edit` sync action → list event. Only user-created lists and the
/// Favourites list become chips; other kinds are kept as `Other`.
pub fn list_edit_event(id: &str, a: &wa::sync_action_value::LabelEditAction) -> Event {
    use wa::sync_action_value::label_edit_action::ListType;
    let kind = match a.r#type {
        Some(ListType::CUSTOM) => ListKind::Custom,
        Some(ListType::FAVORITES) => ListKind::Favourites,
        _ => ListKind::Other,
    };
    Event::ListEdit {
        def: ListDef {
            id: id.to_string(),
            name: a.name.clone().unwrap_or_default(),
            color: a.color.unwrap_or_default(),
            kind,
            order: a.order_index.unwrap_or_default(),
        },
        deleted: a.deleted.unwrap_or(false),
    }
}

/// The `label_edit` payload that creates / renames a custom list.
pub fn list_edit_action(def: &ListDef) -> wa::sync_action_value::LabelEditAction {
    wa::sync_action_value::LabelEditAction {
        name: Some(def.name.clone()),
        color: Some(def.color),
        deleted: Some(false),
        order_index: Some(def.order),
        is_active: Some(true),
        r#type: Some(wa::sync_action_value::label_edit_action::ListType::CUSTOM),
        ..Default::default()
    }
}

/// Archive / pin / mute sync actions → chat flag events.
pub fn archive_flag(a: &wa::sync_action_value::ArchiveChatAction) -> ChatFlag {
    ChatFlag::Archived(a.archived.unwrap_or(false))
}

/// See [`archive_flag`].
pub fn pin_flag(a: &wa::sync_action_value::PinAction) -> ChatFlag {
    ChatFlag::Pinned(a.pinned.unwrap_or(false))
}

/// See [`archive_flag`]. The end time is kept as sent (an expired one
/// simply no longer counts, see [`ChatMeta::is_muted`]).
pub fn mute_flag(a: &wa::sync_action_value::MuteAction) -> ChatFlag {
    if !a.muted.unwrap_or(false) {
        return ChatFlag::Muted(MUTE_OFF);
    }
    ChatFlag::Muted(mute_end_secs(a.mute_end_timestamp.unwrap_or(MUTE_FOREVER)))
}

/// A protocol mute end (milliseconds in sync actions, seconds in older
/// history blobs; `-1` or `0` = forever) as [`ChatMeta::mute_end`].
pub fn mute_end_secs(raw: i64) -> i64 {
    match raw {
        r if r <= 0 => MUTE_FOREVER,
        // Later than year 5000 in seconds: it is milliseconds.
        r if r > 100_000_000_000 => r / 1000,
        r => r,
    }
}

/// `Some(forwardingScore)` when the message carries the forwarded mark.
pub fn forward_from_wa(msg: &wa::Message) -> Option<u32> {
    let ctx = context_info(msg)?;
    let score = ctx.forwarding_score.unwrap_or(0);
    (ctx.is_forwarded == Some(true) || score > 0).then_some(score)
}

/// CDN host media URLs are built on when only the direct path is known
/// (forwards of cached media; recipients download through the path).
const MEDIA_HOST: &str = "https://mmg.whatsapp.net";

/// The body to send when forwarding `content` (see
/// `MessageExt::prepare_for_forward`): rebuilt from the cached content, the
/// media relayed from the same CDN blob, `source_forward` being the source's
/// own forwarded mark. `None` for what cannot be forwarded.
pub fn build_forward(content: &Content, source_forward: Option<u32>) -> Option<wa::Message> {
    use whatsapp_rust::wacore::proto_helpers::MessageExt;
    let ctx = || {
        buffa_some(wa::ContextInfo {
            is_forwarded: source_forward.map(|_| true),
            forwarding_score: source_forward.filter(|s| *s > 0),
            ..Default::default()
        })
    };
    let usable = |m: &Option<MediaRef>| m.clone().filter(|m| !m.media_key.is_empty());
    let url = |m: &MediaRef| Some(format!("{MEDIA_HOST}{}", m.direct_path));
    let msg = match content {
        Content::Text(text) => wa::Message {
            extended_text_message: buffa_some(wa::message::ExtendedTextMessage {
                text: Some(text.clone()),
                context_info: ctx(),
                ..Default::default()
            }),
            ..Default::default()
        },
        Content::Link { text, preview } => wa::Message {
            extended_text_message: buffa_some(wa::message::ExtendedTextMessage {
                text: Some(text.clone()),
                matched_text: Some(preview.url.clone()),
                title: Some(preview.title.clone()),
                description: Some(preview.description.clone()),
                jpeg_thumbnail: preview.thumbnail.clone(),
                context_info: ctx(),
                ..Default::default()
            }),
            ..Default::default()
        },
        Content::Image {
            caption,
            width,
            height,
            thumbnail,
            media,
        } => {
            let m = usable(media)?;
            wa::Message {
                image_message: buffa_some(wa::message::ImageMessage {
                    url: url(&m),
                    mimetype: Some(m.mimetype.clone()),
                    caption: caption.clone(),
                    file_sha256: Some(m.file_sha256.clone()),
                    file_length: Some(m.file_length),
                    height: Some(*height),
                    width: Some(*width),
                    media_key: Some(m.media_key.clone()),
                    file_enc_sha256: Some(m.file_enc_sha256.clone()),
                    direct_path: Some(m.direct_path.clone()),
                    jpeg_thumbnail: thumbnail.clone(),
                    context_info: ctx(),
                    ..Default::default()
                }),
                ..Default::default()
            }
        }
        Content::Document {
            file_name,
            mimetype,
            pages,
            caption,
            media,
            ..
        } => {
            let m = usable(media)?;
            wa::Message {
                document_message: buffa_some(wa::message::DocumentMessage {
                    url: url(&m),
                    mimetype: Some(mimetype.clone()),
                    title: Some(file_name.clone()),
                    file_name: Some(file_name.clone()),
                    file_sha256: Some(m.file_sha256.clone()),
                    file_length: Some(m.file_length),
                    page_count: *pages,
                    media_key: Some(m.media_key.clone()),
                    file_enc_sha256: Some(m.file_enc_sha256.clone()),
                    direct_path: Some(m.direct_path.clone()),
                    caption: caption.clone(),
                    context_info: ctx(),
                    ..Default::default()
                }),
                ..Default::default()
            }
        }
        Content::Voice {
            seconds,
            ptt,
            waveform,
            media,
        } => {
            let m = usable(media)?;
            wa::Message {
                audio_message: buffa_some(wa::message::AudioMessage {
                    url: url(&m),
                    mimetype: Some(m.mimetype.clone()),
                    file_sha256: Some(m.file_sha256.clone()),
                    file_length: Some(m.file_length),
                    seconds: Some(*seconds),
                    ptt: Some(*ptt),
                    media_key: Some(m.media_key.clone()),
                    file_enc_sha256: Some(m.file_enc_sha256.clone()),
                    direct_path: Some(m.direct_path.clone()),
                    waveform: (!waveform.is_empty()).then(|| waveform.clone()),
                    context_info: ctx(),
                    ..Default::default()
                }),
                ..Default::default()
            }
        }
        Content::Video {
            caption,
            seconds,
            width,
            height,
            thumbnail,
            media,
            gif,
        } => {
            let m = usable(media)?;
            wa::Message {
                video_message: buffa_some(wa::message::VideoMessage {
                    gif_playback: gif.then_some(true),
                    url: url(&m),
                    mimetype: Some(m.mimetype.clone()),
                    file_sha256: Some(m.file_sha256.clone()),
                    file_length: Some(m.file_length),
                    seconds: Some(*seconds),
                    media_key: Some(m.media_key.clone()),
                    caption: caption.clone(),
                    height: Some(*height),
                    width: Some(*width),
                    file_enc_sha256: Some(m.file_enc_sha256.clone()),
                    direct_path: Some(m.direct_path.clone()),
                    jpeg_thumbnail: thumbnail.clone(),
                    context_info: ctx(),
                    ..Default::default()
                }),
                ..Default::default()
            }
        }
        Content::Sticker {
            width,
            height,
            animated,
            media,
        } => {
            let m = usable(media)?;
            wa::Message {
                sticker_message: buffa_some(wa::message::StickerMessage {
                    url: url(&m),
                    file_sha256: Some(m.file_sha256.clone()),
                    file_enc_sha256: Some(m.file_enc_sha256.clone()),
                    media_key: Some(m.media_key.clone()),
                    mimetype: Some(m.mimetype.clone()),
                    height: Some(*height),
                    width: Some(*width),
                    direct_path: Some(m.direct_path.clone()),
                    file_length: Some(m.file_length),
                    is_animated: Some(*animated),
                    context_info: ctx(),
                    ..Default::default()
                }),
                ..Default::default()
            }
        }
        Content::Unsupported(_) | Content::ViewOnce | Content::Deleted | Content::Call(_) => {
            return None;
        }
    };
    Some(*msg.prepare_for_forward())
}

/// The `contextInfo` of whichever body the message carries.
pub fn context_info(msg: &wa::Message) -> Option<&wa::ContextInfo> {
    let m = base_message(msg);
    m.extended_text_message
        .as_option()
        .and_then(|x| x.context_info.as_option())
        .or_else(|| {
            m.image_message
                .as_option()
                .and_then(|x| x.context_info.as_option())
        })
        .or_else(|| {
            m.video_message
                .as_option()
                .and_then(|x| x.context_info.as_option())
        })
        .or_else(|| {
            m.document_message
                .as_option()
                .and_then(|x| x.context_info.as_option())
        })
        .or_else(|| {
            m.audio_message
                .as_option()
                .and_then(|x| x.context_info.as_option())
        })
        .or_else(|| {
            m.sticker_message
                .as_option()
                .and_then(|x| x.context_info.as_option())
        })
}

/// Longest group description / about text kept (WhatsApp allows 2048 /
/// 139 characters; anything longer is cut).
pub const MAX_INFO_TEXT: usize = 4096;

fn cap_text(s: &str) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.chars().take(MAX_INFO_TEXT).collect())
}

/// Group metadata → the info drawer's view of it: admins first (owner on
/// top), then members, each by phone number when the group gave one.
pub fn group_info(meta: &whatsapp_rust::features::GroupMetadata) -> super::types::GroupInfo {
    let mut participants: Vec<super::types::Participant> = meta
        .participants
        .iter()
        .take(4096)
        .map(|p| {
            let jid = bare_jid(&p.jid.to_string());
            let pn = p
                .phone_number
                .as_ref()
                .map(|j| bare_jid(&j.to_string()))
                .filter(|j| j.ends_with("@s.whatsapp.net"));
            let lid = p
                .lid
                .as_ref()
                .map(|j| bare_jid(&j.to_string()))
                .or_else(|| jid.ends_with("@lid").then(|| jid.clone()));
            let main = pn.clone().unwrap_or_else(|| jid.clone());
            let alias = if pn.is_some() { lid } else { None }.filter(|a| *a != main);
            super::types::Participant {
                jid: main,
                alias,
                admin: p.is_admin(),
                owner: p.is_super_admin(),
            }
        })
        .collect();
    participants.sort_by(|a, b| {
        b.owner
            .cmp(&a.owner)
            .then(b.admin.cmp(&a.admin))
            .then(a.jid.cmp(&b.jid))
    });
    super::types::GroupInfo {
        subject: meta.subject.chars().take(200).collect(),
        description: meta.description.as_deref().and_then(cap_text),
        created: meta.creation_time.and_then(|t| i64::try_from(t).ok()),
        creator: meta
            .creator_pn
            .as_ref()
            .or(meta.creator.as_ref())
            .map(|j| bare_jid(&j.to_string())),
        participants,
    }
}

/// About text and verified business name from a usync answer.
pub fn contact_info(about: Option<&str>, verified_name: Option<&str>) -> super::types::ContactInfo {
    super::types::ContactInfo {
        about: about.and_then(cap_text),
        business_name: verified_name.and_then(cap_text),
    }
}

/// Subjects of the groups among `groups` that `user` (PN or LID JID, plus
/// its other id) is a member of, sorted by name.
pub fn common_groups(
    groups: &std::collections::HashMap<whatsapp_rust::Jid, whatsapp_rust::features::GroupMetadata>,
    ids: &[String],
) -> Vec<(String, String)> {
    let users: Vec<&str> = ids.iter().filter_map(|j| j.split('@').next()).collect();
    let mut out: Vec<(String, String)> = groups
        .iter()
        .filter(|(_, m)| {
            m.participants.iter().any(|p| {
                [Some(&p.jid), p.phone_number.as_ref(), p.lid.as_ref()]
                    .into_iter()
                    .flatten()
                    .any(|j| users.contains(&j.user.as_str()))
            })
        })
        .map(|(j, m)| (bare_jid(&j.to_string()), m.subject.clone()))
        .collect();
    out.sort_by_key(|g| g.1.to_lowercase());
    out
}

/// JIDs the message's text or caption mentions (`contextInfo.mentionedJid`),
/// bare, deduplicated and bounded.
pub fn mentions_from_wa(msg: &wa::Message) -> Vec<String> {
    let Some(ci) = context_info(msg) else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    for j in ci.mentioned_jid.iter() {
        if j.len() > 64 || !j.contains('@') {
            continue;
        }
        let j = bare_jid(j);
        if !out.contains(&j) {
            out.push(j);
        }
        if out.len() == super::types::MAX_MENTIONS {
            break;
        }
    }
    out
}

/// The quoted message of a reply, if the message is one.
pub fn quote_from_wa(msg: &wa::Message) -> Option<Quote> {
    let ci = context_info(msg)?;
    let id = non_empty(ci.stanza_id.as_ref())?;
    let preview = ci
        .quoted_message
        .as_option()
        .and_then(content_from_wa)
        .map(|c| c.preview())
        .unwrap_or_default();
    Some(Quote {
        id,
        sender: bare_jid(ci.participant.as_deref().unwrap_or_default()),
        preview: preview.chars().take(QUOTE_PREVIEW_MAX).collect(),
    })
}

/// A reaction message: `(target message id, emoji)`; an empty emoji means
/// "reaction removed".
pub fn reaction_from_wa(msg: &wa::Message) -> Option<(String, String)> {
    let r = base_message(msg).reaction_message.as_option()?;
    let target = r.key.as_option()?.id.clone().filter(|i| !i.is_empty())?;
    Some((target, r.text.clone().unwrap_or_default()))
}

/// An edit of an earlier message: `(target id, new content)`. Only text and
/// caption edits are meaningful; anything else yields `None`.
pub fn edit_from_wa(msg: &wa::Message) -> Option<(String, Content)> {
    let p = base_message(msg).protocol_message.as_option()?;
    if p.r#type != Some(wa::message::protocol_message::Type::MESSAGE_EDIT) {
        return None;
    }
    let target = p.key.as_option()?.id.clone().filter(|i| !i.is_empty())?;
    let content = content_from_wa(p.edited_message.as_option()?)?;
    match content {
        Content::Text(_)
        | Content::Image { .. }
        | Content::Video { .. }
        | Content::Document { .. } => Some((target, content)),
        _ => None,
    }
}

/// The id of the message a "delete for everyone" refers to.
pub fn revoked_target(msg: &wa::Message) -> Option<String> {
    let m = base_message(msg);
    let p = m.protocol_message.as_option()?;
    if p.r#type != Some(wa::message::protocol_message::Type::REVOKE) {
        return None;
    }
    p.key.as_option()?.id.clone()
}

/// Strips the agent and device parts from a JID
/// (`123.0:4@s.whatsapp.net` → `123@s.whatsapp.net`). Group ids keep their
/// shape (they contain neither).
pub fn bare_jid(jid: &str) -> String {
    match jid.split_once('@') {
        Some((user, server)) => {
            let user = user.split(':').next().unwrap_or(user);
            let user = user.split('.').next().unwrap_or(user);
            format!("{user}@{server}")
        }
        None => jid.to_string(),
    }
}

/// Learns `(lid, pn)` from a chat/sender and its alternate addressing.
pub fn lid_pair(primary: &str, alt: Option<&str>) -> Option<(String, String)> {
    let (p, a) = (bare_jid(primary), bare_jid(alt?));
    match (p.ends_with("@lid"), a.ends_with("@s.whatsapp.net")) {
        (true, true) => Some((p, a)),
        _ if a.ends_with("@lid") && p.ends_with("@s.whatsapp.net") => Some((a, p)),
        _ => None,
    }
}

/// Fields of a live message envelope the converter needs. Decoupled from
/// `whatsapp_rust::types::message::MessageInfo` so fixtures can build it.
#[derive(Debug, Clone)]
pub struct Envelope {
    /// Message id.
    pub id: String,
    /// Chat JID.
    pub chat: String,
    /// Sender JID.
    pub sender: String,
    /// Outgoing (sent from another of our devices)?
    pub from_me: bool,
    /// Push name.
    pub push_name: String,
    /// Unix seconds.
    pub timestamp: i64,
    /// Alternate addressing of the sender (PN for a LID sender or vice versa).
    pub sender_alt: Option<String>,
    /// Alternate addressing of the recipient (our own outgoing DMs).
    pub recipient_alt: Option<String>,
    /// Disappearing-message timer in seconds, if any.
    pub ephemeral_secs: Option<u32>,
}

impl Envelope {
    /// LID ↔ PN pairs this envelope reveals.
    pub fn lid_pairs(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        out.extend(lid_pair(&self.sender, self.sender_alt.as_deref()));
        if self.from_me {
            out.extend(lid_pair(&self.chat, self.recipient_alt.as_deref()));
        } else if !self.chat.ends_with("@g.us") {
            out.extend(lid_pair(&self.chat, self.sender_alt.as_deref()));
        }
        out
    }
}

/// Live inbound message → domain message.
pub fn message_from_envelope(env: &Envelope, msg: &wa::Message) -> Option<Message> {
    let content = content_from_wa(msg)?;
    Some(Message {
        id: env.id.clone(),
        chat: bare_jid(&env.chat),
        sender: bare_jid(&env.sender),
        sender_name: Some(env.push_name.clone()).filter(|n| !n.is_empty()),
        from_me: env.from_me,
        timestamp: env.timestamp,
        content,
        // Messages from our other devices already reached the server.
        status: if env.from_me {
            ReceiptStatus::Sent
        } else {
            ReceiptStatus::Read
        },
        expires_at: env
            .ephemeral_secs
            .filter(|s| *s > 0)
            .map(|s| env.timestamp + s as i64),
        reply: quote_from_wa(msg).map(|mut q| {
            q.sender = bare_jid(&q.sender);
            q
        }),
        edited: false,
        forwarded: forward_from_wa(msg),
        reactions: Vec::new(),
        mentions: mentions_from_wa(msg),
    })
}

/// What an incoming decrypted message turned out to be.
#[derive(Debug, Clone, PartialEq)]
pub enum Parsed {
    /// A bubble (a revoke is a `Content::Deleted` message with the target id).
    Message(Box<Message>),
    /// A reaction added / changed / removed (empty emoji).
    Reaction {
        /// Reacted-to message.
        target: String,
        /// Emoji, empty when removed.
        emoji: String,
    },
    /// A text / caption edit.
    Edit {
        /// Edited message.
        target: String,
        /// New content.
        content: Content,
    },
}

/// Classifies one incoming message. Returns `None` for protocol chatter.
pub fn parse_incoming(env: &Envelope, msg: &wa::Message) -> Option<Parsed> {
    // Status updates arrive as messages in `status@broadcast`; they are not
    // a chat (they made a hidden "chat" whose unread count reached 99+).
    if super::types::is_pseudo_chat(&bare_jid(&env.chat)) {
        return None;
    }
    if let Some(target) = revoked_target(msg) {
        let mut m = message_from_envelope(env, msg)?;
        m.id = target;
        m.content = Content::Deleted;
        return Some(Parsed::Message(Box::new(m)));
    }
    if let Some((target, content)) = edit_from_wa(msg) {
        return Some(Parsed::Edit { target, content });
    }
    if let Some((target, emoji)) = reaction_from_wa(msg) {
        return Some(Parsed::Reaction { target, emoji });
    }
    message_from_envelope(env, msg).map(|m| Parsed::Message(Box::new(m)))
}

/// Typing indicator from the library's presence types.
pub fn chat_state(
    state: whatsapp_rust::wacore::types::presence::ChatPresence,
    media: whatsapp_rust::wacore::types::presence::ChatPresenceMedia,
) -> super::types::ChatState {
    use super::types::ChatState;
    use whatsapp_rust::wacore::types::presence::{ChatPresence, ChatPresenceMedia};
    match (state, media) {
        (ChatPresence::Composing, ChatPresenceMedia::Audio) => ChatState::Recording,
        (ChatPresence::Composing, _) => ChatState::Composing,
        (ChatPresence::Paused, _) => ChatState::Paused,
    }
}

/// History-sync status → receipt status.
pub fn status_from_web(status: Option<wa::web_message_info::Status>) -> ReceiptStatus {
    use wa::web_message_info::Status;
    match status {
        Some(Status::ERROR) => ReceiptStatus::Failed,
        Some(Status::PENDING) => ReceiptStatus::Pending,
        Some(Status::SERVER_ACK) | None => ReceiptStatus::Sent,
        Some(Status::DELIVERY_ACK) => ReceiptStatus::Delivered,
        Some(Status::READ) => ReceiptStatus::Read,
        Some(Status::PLAYED) => ReceiptStatus::Played,
    }
}

/// History-sync message → domain message. `ephemeral_secs` is the chat's
/// disappearing-messages timer.
pub fn message_from_web_info(
    chat: &str,
    wmi: &wa::WebMessageInfo,
    ephemeral_secs: Option<u32>,
) -> Option<Message> {
    let key = wmi.key.as_option()?;
    let id = key.id.clone()?;
    let content = match wmi.message.as_option() {
        Some(m) => content_from_wa(m)?,
        // Missed calls are history stubs with no message body.
        None => Content::Call(super::call::log_from_stub(wmi.message_stub_type?)?),
    };
    let from_me = key.from_me.unwrap_or(false);
    let sender = if from_me {
        String::new()
    } else {
        wmi.participant
            .clone()
            .or_else(|| key.participant.clone())
            .unwrap_or_else(|| chat.to_string())
    };
    let timestamp = wmi.message_timestamp.unwrap_or_default() as i64;
    Some(Message {
        id,
        chat: bare_jid(chat),
        sender: bare_jid(&sender),
        sender_name: non_empty(wmi.push_name.as_ref()),
        from_me,
        timestamp,
        content,
        status: if from_me {
            status_from_web(wmi.status)
        } else {
            ReceiptStatus::Read
        },
        expires_at: ephemeral_secs
            .filter(|s| *s > 0)
            .map(|s| timestamp + s as i64),
        reply: wmi.message.as_option().and_then(quote_from_wa),
        edited: false,
        forwarded: wmi.message.as_option().and_then(forward_from_wa),
        reactions: wmi
            .reactions
            .iter()
            .filter_map(|r| reaction_from_web(chat, r))
            .collect(),
        mentions: wmi
            .message
            .as_option()
            .map(mentions_from_wa)
            .unwrap_or_default(),
    })
}

/// A reaction stored in a history-synced message.
fn reaction_from_web(chat: &str, r: &wa::Reaction) -> Option<Reaction> {
    let key = r.key.as_option()?;
    let emoji = non_empty(r.text.as_ref())?;
    let reactor = if key.from_me.unwrap_or(false) {
        String::new() // filled with our own JID when stored (unknown here)
    } else {
        bare_jid(
            key.participant
                .as_deref()
                .or(key.remote_jid.as_deref())
                .unwrap_or(chat),
        )
    };
    Some(Reaction {
        reactor,
        emoji,
        ts: r.sender_timestamp_ms.unwrap_or_default() / 1000,
    })
}

/// History-sync conversation → chat metadata.
pub fn chat_from_conversation(conv: &wa::Conversation, now: i64) -> ChatMeta {
    let last_ts = conv
        .conversation_timestamp
        .or(conv.last_msg_timestamp)
        .unwrap_or_default() as i64;
    ChatMeta {
        id: bare_jid(&conv.id),
        name: non_empty(conv.name.as_ref()).or_else(|| non_empty(conv.display_name.as_ref())),
        // "Mark as unread" on the phone shows as one unread, like the dot.
        unread: match conv.unread_count.unwrap_or_default() {
            0 if conv.marked_as_unread == Some(true) => 1,
            n => n,
        },
        last_ts,
        pinned: conv.pinned.unwrap_or_default() > 0,
        archived: conv.archived.unwrap_or(false),
        // `u64::MAX` (-1) means "forever"; an expired mute is not muted.
        mute_end: match conv.mute_end_time.map(|t| t as i64) {
            Some(t) if t != 0 => match mute_end_secs(t) {
                end if mute_active(end, now) => end,
                _ => MUTE_OFF,
            },
            _ => MUTE_OFF,
        },
    }
}

/// Everything a history-sync blob carries that the app uses.
#[derive(Debug, Default)]
pub struct HistoryParts {
    /// Chats.
    pub chats: Vec<ChatMeta>,
    /// Messages.
    pub messages: Vec<Message>,
    /// Push names `(jid, name)`.
    pub names: Vec<(String, String)>,
    /// LID → PN pairs.
    pub lid_pairs: Vec<(String, String)>,
}

/// Converts a full history-sync blob into chats, messages, names and LID
/// mappings.
pub fn from_history(hs: &wa::HistorySync, now: i64) -> HistoryParts {
    let mut parts = HistoryParts::default();
    for m in &hs.phone_number_to_lid_mappings {
        if let (Some(pn), Some(lid)) = (&m.pn_jid, &m.lid_jid)
            && let Some(pair) = lid_pair(lid, Some(pn))
        {
            parts.lid_pairs.push(pair);
        }
    }
    for conv in &hs.conversations {
        if conv.id.ends_with("@broadcast") || conv.id.ends_with("@newsletter") {
            continue;
        }
        let alt = conv.pn_jid.as_deref().or(conv.lid_jid.as_deref());
        if let Some(pair) = lid_pair(&conv.id, alt) {
            parts.lid_pairs.push(pair);
        }
        parts.chats.push(chat_from_conversation(conv, now));
        for hm in &conv.messages {
            if let Some(m) = hm
                .message
                .as_option()
                .and_then(|w| message_from_web_info(&conv.id, w, conv.ephemeral_expiration))
            {
                parts.messages.push(m);
            }
        }
    }
    parts.names = hs
        .pushnames
        .iter()
        .filter_map(|p| Some((bare_jid(p.id.as_ref()?), non_empty(p.pushname.as_ref())?)))
        .collect();
    parts
}

/// Receipt type → tick state. `None` for receipts that do not change ticks.
pub fn receipt_status(t: &ReceiptType) -> Option<ReceiptStatus> {
    match t {
        ReceiptType::Delivered => Some(ReceiptStatus::Delivered),
        ReceiptType::Sent => Some(ReceiptStatus::Sent),
        ReceiptType::Read => Some(ReceiptStatus::Read),
        ReceiptType::Played => Some(ReceiptStatus::Played),
        _ => None,
    }
}

/// True for receipts meaning "we read this chat on another device".
pub fn is_self_read(t: &ReceiptType) -> bool {
    matches!(t, ReceiptType::ReadSelf | ReceiptType::PlayedSelf)
}

/// Builds a plain text message.
pub fn build_text(text: &str) -> wa::Message {
    wa::Message {
        conversation: Some(text.to_string()),
        ..Default::default()
    }
}

/// Builds a text message that replies to `quote`.
pub fn build_reply(text: &str, quote: &Quote) -> wa::Message {
    wa::Message {
        extended_text_message: buffa_some(wa::message::ExtendedTextMessage {
            text: Some(text.to_string()),
            context_info: buffa_some(wa::ContextInfo {
                stanza_id: Some(quote.id.clone()),
                participant: Some(quote.sender.clone()),
                quoted_message: buffa_some(build_text(&quote.preview)),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Reaction target key for a message.
pub fn reaction_key(
    chat: &str,
    id: &str,
    from_me: bool,
    sender: &str,
    group: bool,
) -> wa::MessageKey {
    wa::MessageKey {
        remote_jid: Some(chat.to_string()),
        from_me: Some(from_me),
        id: Some(id.to_string()),
        // Groups attribute the reaction through the original author.
        participant: (group && !from_me).then(|| sender.to_string()),
    }
}

/// CDN result of an upload, decoupled from the library type for tests.
#[derive(Debug, Clone)]
pub struct Uploaded {
    /// Full URL.
    pub url: String,
    /// CDN path.
    pub direct_path: String,
    /// Media key.
    pub media_key: Vec<u8>,
    /// SHA-256 of the plaintext.
    pub file_sha256: Vec<u8>,
    /// SHA-256 of the ciphertext.
    pub file_enc_sha256: Vec<u8>,
    /// Plaintext length.
    pub file_length: u64,
    /// Key timestamp.
    pub media_key_timestamp: i64,
    /// Streaming sidecar for audio.
    pub streaming_sidecar: Option<Vec<u8>>,
}

/// Builds the protobuf for uploaded media plus the matching local content.
pub fn build_media(media: &OutgoingMedia, up: &Uploaded) -> (wa::Message, Content) {
    let kind = match media.kind {
        OutgoingKind::Image { .. } => MediaKind::Image,
        OutgoingKind::Document => MediaKind::Document,
        OutgoingKind::Video { .. } => MediaKind::Video,
        OutgoingKind::Voice { .. } => MediaKind::Audio,
    };
    let mref = MediaRef {
        direct_path: up.direct_path.clone(),
        media_key: up.media_key.clone(),
        file_sha256: up.file_sha256.clone(),
        file_enc_sha256: up.file_enc_sha256.clone(),
        file_length: up.file_length,
        kind,
        mimetype: media.mimetype.clone(),
    };
    let content = media.local_content(Some(mref));
    let msg = match &media.kind {
        OutgoingKind::Image {
            width,
            height,
            thumbnail,
        } => wa::Message {
            image_message: buffa_some(wa::message::ImageMessage {
                url: Some(up.url.clone()),
                mimetype: Some(media.mimetype.clone()),
                caption: media.caption.clone(),
                file_sha256: Some(up.file_sha256.clone()),
                file_length: Some(up.file_length),
                height: Some(*height),
                width: Some(*width),
                media_key: Some(up.media_key.clone()),
                file_enc_sha256: Some(up.file_enc_sha256.clone()),
                direct_path: Some(up.direct_path.clone()),
                media_key_timestamp: Some(up.media_key_timestamp),
                jpeg_thumbnail: Some(thumbnail.clone()),
                ..Default::default()
            }),
            ..Default::default()
        },
        OutgoingKind::Document => wa::Message {
            document_message: buffa_some(wa::message::DocumentMessage {
                url: Some(up.url.clone()),
                mimetype: Some(media.mimetype.clone()),
                title: Some(media.file_name.clone()),
                file_name: Some(media.file_name.clone()),
                file_sha256: Some(up.file_sha256.clone()),
                file_length: Some(up.file_length),
                media_key: Some(up.media_key.clone()),
                file_enc_sha256: Some(up.file_enc_sha256.clone()),
                direct_path: Some(up.direct_path.clone()),
                media_key_timestamp: Some(up.media_key_timestamp),
                caption: media.caption.clone(),
                ..Default::default()
            }),
            ..Default::default()
        },
        OutgoingKind::Video {
            seconds,
            width,
            height,
            thumbnail,
            gif,
        } => wa::Message {
            video_message: buffa_some(wa::message::VideoMessage {
                url: Some(up.url.clone()),
                mimetype: Some(media.mimetype.clone()),
                caption: media.caption.clone(),
                file_sha256: Some(up.file_sha256.clone()),
                file_length: Some(up.file_length),
                seconds: Some(*seconds),
                height: Some(*height),
                width: Some(*width),
                media_key: Some(up.media_key.clone()),
                file_enc_sha256: Some(up.file_enc_sha256.clone()),
                direct_path: Some(up.direct_path.clone()),
                media_key_timestamp: Some(up.media_key_timestamp),
                jpeg_thumbnail: Some(thumbnail.clone()),
                streaming_sidecar: up.streaming_sidecar.clone(),
                gif_playback: gif.then_some(true),
                ..Default::default()
            }),
            ..Default::default()
        },
        OutgoingKind::Voice { seconds, waveform } => wa::Message {
            audio_message: buffa_some(wa::message::AudioMessage {
                url: Some(up.url.clone()),
                mimetype: Some(media.mimetype.clone()),
                file_sha256: Some(up.file_sha256.clone()),
                file_length: Some(up.file_length),
                seconds: Some(*seconds),
                ptt: Some(true),
                media_key: Some(up.media_key.clone()),
                file_enc_sha256: Some(up.file_enc_sha256.clone()),
                direct_path: Some(up.direct_path.clone()),
                media_key_timestamp: Some(up.media_key_timestamp),
                streaming_sidecar: up.streaming_sidecar.clone(),
                waveform: Some(waveform.clone()),
                ..Default::default()
            }),
            ..Default::default()
        },
    };
    (msg, content)
}

fn buffa_some<T: Default>(v: T) -> whatsapp_rust::waproto::buffa::MessageField<T> {
    whatsapp_rust::waproto::buffa::MessageField::some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(from_me: bool) -> Envelope {
        Envelope {
            id: "ABC".into(),
            chat: "34600111222@s.whatsapp.net".into(),
            sender: "34600111222:3@s.whatsapp.net".into(),
            from_me,
            push_name: "Ana".into(),
            timestamp: 1_700_000_000,
            sender_alt: None,
            recipient_alt: None,
            ephemeral_secs: None,
        }
    }

    #[test]
    fn status_updates_are_not_chat_messages() {
        let mut e = env(false);
        e.chat = "status@broadcast".into();
        let msg = wa::Message {
            conversation: Some("my status".into()),
            ..Default::default()
        };
        assert!(parse_incoming(&e, &msg).is_none());
        e.chat = "34600111222@s.whatsapp.net".into();
        assert!(parse_incoming(&e, &msg).is_some());
    }

    fn mref(kind: MediaKind) -> MediaRef {
        MediaRef {
            direct_path: "/v/t62/abc".into(),
            media_key: vec![1; 32],
            file_sha256: vec![2; 32],
            file_enc_sha256: vec![3; 32],
            file_length: 1234,
            kind,
            mimetype: "image/jpeg".into(),
        }
    }

    fn forward_ctx(m: &wa::Message) -> (Option<bool>, Option<u32>) {
        let c = context_info(m).unwrap();
        (c.is_forwarded, c.forwarding_score)
    }

    #[test]
    fn forwarded_mark_is_read_from_the_context() {
        let mut m = build_forward(&Content::Text("hi".into()), None).unwrap();
        assert_eq!(forward_from_wa(&m), Some(0));
        assert_eq!(forward_from_wa(&build_text("hi")), None);
        let ctx = m
            .extended_text_message
            .as_option_mut()
            .unwrap()
            .context_info
            .as_option_mut()
            .unwrap();
        ctx.is_forwarded = None;
        ctx.forwarding_score = Some(3);
        assert_eq!(forward_from_wa(&m), Some(3));
        let msg = message_from_envelope(&env(false), &m).unwrap();
        assert_eq!(msg.forwarded, Some(3));
    }

    #[test]
    fn forwards_bump_the_score_like_the_official_client() {
        let text = Content::Text("hi".into());
        // First forward: marked, score 0; a forward of a forward: +1; from 5
        // on the "frequently forwarded" sentinel.
        assert_eq!(
            forward_ctx(&build_forward(&text, None).unwrap()),
            (Some(true), Some(0))
        );
        assert_eq!(
            forward_ctx(&build_forward(&text, Some(0)).unwrap()),
            (Some(true), Some(1))
        );
        assert_eq!(
            forward_ctx(&build_forward(&text, Some(4)).unwrap()),
            (Some(true), Some(127))
        );
        // The domain helper agrees.
        let mut m = message_from_envelope(&env(false), &build_text("x")).unwrap();
        for (src, next) in [
            (None, 0),
            (Some(0), 1),
            (Some(3), 4),
            (Some(4), 127),
            (Some(127), 127),
        ] {
            m.forwarded = src;
            assert_eq!(m.forward_score(), next, "{src:?}");
            let sent = build_forward(&m.content, src).unwrap();
            assert_eq!(forward_from_wa(&sent), Some(next));
        }
    }

    #[test]
    fn forwarded_media_reuses_the_cdn_blob() {
        let img = Content::Image {
            caption: Some("sunset".into()),
            width: 640,
            height: 420,
            thumbnail: Some(vec![9, 9]),
            media: Some(mref(MediaKind::Image)),
        };
        let m = build_forward(&img, None).unwrap();
        let i = m.image_message.as_option().unwrap();
        assert_eq!(i.direct_path.as_deref(), Some("/v/t62/abc"));
        assert_eq!(i.url.as_deref(), Some("https://mmg.whatsapp.net/v/t62/abc"));
        assert_eq!(i.media_key.as_deref(), Some(&[1u8; 32][..]));
        assert_eq!(i.jpeg_thumbnail.as_deref(), Some(&[9u8, 9][..]));
        // What arrives reads back as the same content.
        assert_eq!(content_from_wa(&m), Some(img));
        let voice = Content::Voice {
            seconds: 4,
            ptt: true,
            waveform: vec![10; 64],
            media: Some(mref(MediaKind::Audio)),
        };
        let back = content_from_wa(&build_forward(&voice, Some(0)).unwrap()).unwrap();
        assert!(matches!(
            back,
            Content::Voice {
                ptt: true,
                seconds: 4,
                ..
            }
        ));
        let link = Content::Link {
            text: "look https://example.com".into(),
            preview: LinkPreview {
                url: "https://example.com".into(),
                title: "Example".into(),
                description: "d".into(),
                thumbnail: None,
            },
        };
        assert_eq!(
            content_from_wa(&build_forward(&link, None).unwrap()),
            Some(link)
        );
    }

    #[test]
    fn what_cannot_be_forwarded_builds_nothing() {
        assert!(build_forward(&Content::Deleted, None).is_none());
        assert!(build_forward(&Content::ViewOnce, None).is_none());
        assert!(build_forward(&Content::Unsupported("poll".into()), None).is_none());
        let no_media = Content::Document {
            file_name: "a.pdf".into(),
            mimetype: "application/pdf".into(),
            size: 1,
            pages: None,
            caption: None,
            media: None,
        };
        assert!(build_forward(&no_media, None).is_none());
        let mut no_key = mref(MediaKind::Document);
        no_key.media_key.clear();
        let no_key = Content::Document {
            file_name: "a.pdf".into(),
            mimetype: "application/pdf".into(),
            size: 1,
            pages: None,
            caption: None,
            media: Some(no_key),
        };
        assert!(build_forward(&no_key, None).is_none());
    }

    #[test]
    fn text_and_extended_text() {
        let m = build_text("hola");
        assert_eq!(content_from_wa(&m), Some(Content::Text("hola".into())));
        let ext = wa::Message {
            extended_text_message: buffa_some(wa::message::ExtendedTextMessage {
                text: Some("link https://x".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            content_from_wa(&ext),
            Some(Content::Text("link https://x".into()))
        );
    }

    #[test]
    fn unwraps_ephemeral() {
        let inner = build_text("disappearing");
        let m = wa::Message {
            ephemeral_message: buffa_some(wa::message::FutureProofMessage {
                message: buffa_some(inner),
            }),
            ..Default::default()
        };
        assert_eq!(
            content_from_wa(&m),
            Some(Content::Text("disappearing".into()))
        );
    }

    #[test]
    fn envelope_conversion_strips_device() {
        let m = message_from_envelope(&env(false), &build_text("hi")).unwrap();
        assert_eq!(m.sender, "34600111222@s.whatsapp.net");
        assert_eq!(m.sender_name.as_deref(), Some("Ana"));
        assert!(!m.from_me);
        let mine = message_from_envelope(&env(true), &build_text("hi")).unwrap();
        assert_eq!(mine.status, ReceiptStatus::Sent);
    }

    #[test]
    fn reactions_are_not_bubbles() {
        let m = wa::Message {
            reaction_message: buffa_some(wa::message::ReactionMessage::default()),
            ..Default::default()
        };
        assert_eq!(content_from_wa(&m), None);
    }

    #[test]
    fn revoke_becomes_deleted() {
        let m = wa::Message {
            protocol_message: buffa_some(wa::message::ProtocolMessage {
                key: buffa_some(wa::MessageKey {
                    id: Some("TARGET".into()),
                    ..Default::default()
                }),
                r#type: Some(wa::message::protocol_message::Type::REVOKE),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(content_from_wa(&m), Some(Content::Deleted));
        assert_eq!(revoked_target(&m).as_deref(), Some("TARGET"));
    }

    #[test]
    fn receipt_mapping() {
        assert_eq!(
            receipt_status(&ReceiptType::Read),
            Some(ReceiptStatus::Read)
        );
        assert_eq!(
            receipt_status(&ReceiptType::Delivered),
            Some(ReceiptStatus::Delivered)
        );
        assert_eq!(receipt_status(&ReceiptType::Retry), None);
        assert!(is_self_read(&ReceiptType::ReadSelf));
    }

    #[test]
    fn media_roundtrip_build_then_parse() {
        let media = OutgoingMedia {
            kind: OutgoingKind::Voice {
                seconds: 7,
                waveform: vec![10; 64],
            },
            bytes: vec![1, 2, 3],
            file_name: "voice.ogg".into(),
            mimetype: "audio/ogg; codecs=opus".into(),
            caption: None,
        };
        let up = Uploaded {
            url: "https://mmg.whatsapp.net/x".into(),
            direct_path: "/v/t62/x".into(),
            media_key: vec![7; 32],
            file_sha256: vec![1; 32],
            file_enc_sha256: vec![2; 32],
            file_length: 3,
            media_key_timestamp: 1,
            streaming_sidecar: None,
        };
        let (msg, local) = build_media(&media, &up);
        let parsed = content_from_wa(&msg).unwrap();
        assert_eq!(parsed, local);
        match parsed {
            Content::Voice {
                seconds,
                ptt,
                media,
                ..
            } => {
                assert_eq!(seconds, 7);
                assert!(ptt);
                assert_eq!(media.unwrap().direct_path, "/v/t62/x");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn videos_and_gifs_roundtrip_and_forward_as_gifs() {
        let up = Uploaded {
            url: "https://mmg.whatsapp.net/v".into(),
            direct_path: "/v/t62/v".into(),
            media_key: vec![7; 32],
            file_sha256: vec![1; 32],
            file_enc_sha256: vec![2; 32],
            file_length: 9,
            media_key_timestamp: 1,
            streaming_sidecar: None,
        };
        for gif in [false, true] {
            let media = OutgoingMedia {
                kind: OutgoingKind::Video {
                    seconds: 4,
                    width: 640,
                    height: 360,
                    thumbnail: vec![5, 5],
                    gif,
                },
                bytes: vec![0; 9],
                file_name: "clip.mp4".into(),
                mimetype: "video/mp4".into(),
                caption: Some("hey".into()),
            };
            let (msg, local) = build_media(&media, &up);
            let v = msg.video_message.as_option().unwrap();
            assert_eq!(v.gif_playback, gif.then_some(true));
            assert_eq!(v.seconds, Some(4));
            assert_eq!(v.jpeg_thumbnail.as_deref(), Some(&[5u8, 5][..]));
            let parsed = content_from_wa(&msg).unwrap();
            assert_eq!(parsed, local);
            assert!(matches!(parsed, Content::Video { gif: g, .. } if g == gif));
            assert_eq!(parsed.media().unwrap().kind, MediaKind::Video);
            // A forwarded GIF is still a GIF.
            let fwd = build_forward(&parsed, None).unwrap();
            assert_eq!(
                fwd.video_message.as_option().unwrap().gif_playback,
                gif.then_some(true)
            );
        }
    }

    #[test]
    fn view_once_is_never_exposed() {
        let m = wa::Message {
            view_once_message: buffa_some(wa::message::FutureProofMessage {
                message: buffa_some(wa::Message {
                    image_message: buffa_some(wa::message::ImageMessage {
                        direct_path: Some("/secret".into()),
                        media_key: Some(vec![1; 32]),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
            }),
            ..Default::default()
        };
        assert_eq!(content_from_wa(&m), Some(Content::ViewOnce));
        let flagged = wa::Message {
            image_message: buffa_some(wa::message::ImageMessage {
                view_once: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(content_from_wa(&flagged), Some(Content::ViewOnce));
    }

    #[test]
    fn waveform_values_are_clamped() {
        let m = wa::Message {
            audio_message: buffa_some(wa::message::AudioMessage {
                waveform: Some(vec![250; 80]),
                ..Default::default()
            }),
            ..Default::default()
        };
        match content_from_wa(&m) {
            Some(Content::Voice { waveform, .. }) => {
                assert_eq!(waveform.len(), 64);
                assert!(waveform.iter().all(|v| *v == 100));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn lid_pairs_from_envelopes() {
        let mut e = env(false);
        e.chat = "111@lid".into();
        e.sender = "111:2@lid".into();
        e.sender_alt = Some("34600111222:2@s.whatsapp.net".into());
        let pairs = e.lid_pairs();
        assert!(pairs.contains(&("111@lid".into(), "34600111222@s.whatsapp.net".into())));
        let mut mine = env(true);
        mine.chat = "222@lid".into();
        mine.sender = "34600000000@s.whatsapp.net".into();
        mine.recipient_alt = Some("34600333444@s.whatsapp.net".into());
        assert!(
            mine.lid_pairs()
                .contains(&("222@lid".into(), "34600333444@s.whatsapp.net".into()))
        );
        assert_eq!(lid_pair("1@g.us", Some("x@lid")), None);
    }

    #[test]
    fn ephemeral_expiry_is_computed() {
        let mut e = env(false);
        e.ephemeral_secs = Some(86_400);
        let m = message_from_envelope(&e, &build_text("x")).unwrap();
        assert_eq!(m.expires_at, Some(1_700_000_000 + 86_400));
    }

    #[test]
    fn bare_jid_variants() {
        assert_eq!(bare_jid("1:2@s.whatsapp.net"), "1@s.whatsapp.net");
        assert_eq!(bare_jid("1.0:2@s.whatsapp.net"), "1@s.whatsapp.net");
        assert_eq!(bare_jid("123@g.us"), "123@g.us");
        assert_eq!(bare_jid("weird"), "weird");
    }
}

#[cfg(test)]
mod parity_tests {
    use super::*;
    use whatsapp_rust::wacore::types::presence::{ChatPresence, ChatPresenceMedia};

    fn env() -> Envelope {
        Envelope {
            id: "M1".into(),
            chat: "34600111222@s.whatsapp.net".into(),
            sender: "34600111222:3@s.whatsapp.net".into(),
            from_me: false,
            push_name: "Ana".into(),
            timestamp: 1_700_000_000,
            sender_alt: None,
            recipient_alt: None,
            ephemeral_secs: None,
        }
    }

    fn quote() -> Quote {
        Quote {
            id: "ORIG".into(),
            sender: "34699000111@s.whatsapp.net".into(),
            preview: "where are you?".into(),
        }
    }

    #[test]
    fn reply_roundtrips_through_the_wire_format() {
        let msg = build_reply("on my way", &quote());
        let m = message_from_envelope(&env(), &msg).unwrap();
        assert_eq!(m.content, Content::Text("on my way".into()));
        let q = m.reply.unwrap();
        assert_eq!(q.id, "ORIG");
        assert_eq!(q.sender, "34699000111@s.whatsapp.net");
        assert_eq!(q.preview, "where are you?");
    }

    #[test]
    fn mentioned_jids_are_parsed_bare_deduplicated_and_bounded() {
        let mk = |jids: Vec<String>| wa::Message {
            extended_text_message: buffa_some(wa::message::ExtendedTextMessage {
                text: Some("@123456789012345 hi".into()),
                context_info: buffa_some(wa::ContextInfo {
                    mentioned_jid: jids,
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let msg = mk(vec![
            "123456789012345@lid".into(),
            "34600111222:3@s.whatsapp.net".into(),
            "123456789012345@lid".into(),
            "garbage".into(),
        ]);
        let m = message_from_envelope(&env(), &msg).unwrap();
        assert_eq!(
            m.mentions,
            vec![
                "123456789012345@lid".to_string(),
                "34600111222@s.whatsapp.net".to_string()
            ]
        );
        let many = mk((0..5000).map(|i| format!("{i}@lid")).collect());
        assert_eq!(
            mentions_from_wa(&many).len(),
            crate::net::types::MAX_MENTIONS
        );
        assert!(mentions_from_wa(&build_text("plain")).is_empty());
    }

    #[test]
    fn quote_of_media_uses_the_media_preview() {
        let msg = wa::Message {
            extended_text_message: buffa_some(wa::message::ExtendedTextMessage {
                text: Some("nice".into()),
                context_info: buffa_some(wa::ContextInfo {
                    stanza_id: Some("IMG".into()),
                    participant: Some("1:5@s.whatsapp.net".into()),
                    quoted_message: buffa_some(wa::Message {
                        image_message: buffa_some(wa::message::ImageMessage {
                            caption: Some("sunset".into()),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let q = quote_from_wa(&msg).unwrap();
        assert_eq!(q.preview, "📷 sunset");
        assert_eq!(q.sender, "1@s.whatsapp.net", "device part stripped");
    }

    #[test]
    fn quote_preview_is_capped_and_missing_id_is_no_reply() {
        let long = "x".repeat(5000);
        let msg = build_reply(
            "r",
            &Quote {
                preview: long,
                ..quote()
            },
        );
        assert!(quote_from_wa(&msg).unwrap().preview.chars().count() <= QUOTE_PREVIEW_MAX);
        let no_id = wa::Message {
            extended_text_message: buffa_some(wa::message::ExtendedTextMessage {
                text: Some("t".into()),
                context_info: buffa_some(wa::ContextInfo::default()),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(quote_from_wa(&no_id), None);
        assert_eq!(quote_from_wa(&build_text("plain")), None);
    }

    fn reaction(target: Option<&str>, text: Option<&str>) -> wa::Message {
        wa::Message {
            reaction_message: buffa_some(wa::message::ReactionMessage {
                key: buffa_some(wa::MessageKey {
                    id: target.map(Into::into),
                    ..Default::default()
                }),
                text: text.map(Into::into),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn reactions_are_parsed_and_removal_is_an_empty_emoji() {
        assert_eq!(
            reaction_from_wa(&reaction(Some("T"), Some("👍"))),
            Some(("T".into(), "👍".into()))
        );
        assert_eq!(
            reaction_from_wa(&reaction(Some("T"), Some(""))),
            Some(("T".into(), String::new()))
        );
        assert_eq!(
            reaction_from_wa(&reaction(Some("T"), None)),
            Some(("T".into(), String::new()))
        );
        assert_eq!(reaction_from_wa(&reaction(None, Some("👍"))), None);
        assert_eq!(reaction_from_wa(&build_text("x")), None);
    }

    fn edit(target: &str, new: wa::Message) -> wa::Message {
        wa::Message {
            protocol_message: buffa_some(wa::message::ProtocolMessage {
                key: buffa_some(wa::MessageKey {
                    id: Some(target.into()),
                    ..Default::default()
                }),
                r#type: Some(wa::message::protocol_message::Type::MESSAGE_EDIT),
                edited_message: buffa_some(new),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn edits_are_parsed() {
        let m = edit("T", build_text("fixed"));
        assert_eq!(
            edit_from_wa(&m),
            Some(("T".into(), Content::Text("fixed".into())))
        );
        // An edit to something that is not text / caption is ignored.
        let sticker = wa::Message {
            sticker_message: buffa_some(wa::message::StickerMessage::default()),
            ..Default::default()
        };
        assert_eq!(edit_from_wa(&edit("T", sticker)), None);
        // Edits never become bubbles.
        assert_eq!(content_from_wa(&m), None);
    }

    #[test]
    fn incoming_messages_are_classified() {
        let e = env();
        assert!(matches!(
            parse_incoming(&e, &build_text("hi")),
            Some(Parsed::Message(_))
        ));
        assert!(matches!(
            parse_incoming(&e, &reaction(Some("T"), Some("❤️"))),
            Some(Parsed::Reaction { .. })
        ));
        assert!(matches!(
            parse_incoming(&e, &edit("T", build_text("x"))),
            Some(Parsed::Edit { .. })
        ));
        let revoke = wa::Message {
            protocol_message: buffa_some(wa::message::ProtocolMessage {
                key: buffa_some(wa::MessageKey {
                    id: Some("GONE".into()),
                    ..Default::default()
                }),
                r#type: Some(wa::message::protocol_message::Type::REVOKE),
                ..Default::default()
            }),
            ..Default::default()
        };
        match parse_incoming(&e, &revoke) {
            Some(Parsed::Message(m)) => {
                assert_eq!(m.id, "GONE");
                assert_eq!(m.content, Content::Deleted);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(parse_incoming(&e, &wa::Message::default()), None);
    }

    #[test]
    fn video_and_sticker_are_rendered_content() {
        let video = wa::Message {
            video_message: buffa_some(wa::message::VideoMessage {
                seconds: Some(14),
                width: Some(640),
                height: Some(360),
                caption: Some("trip".into()),
                jpeg_thumbnail: Some(vec![1, 2, 3]),
                direct_path: Some("/v/vid".into()),
                media_key: Some(vec![9; 32]),
                mimetype: Some("video/mp4".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        match content_from_wa(&video) {
            Some(Content::Video {
                caption,
                seconds,
                thumbnail,
                media,
                ..
            }) => {
                assert_eq!(caption.as_deref(), Some("trip"));
                assert_eq!(seconds, 14);
                assert_eq!(thumbnail, Some(vec![1, 2, 3]));
                let m = media.unwrap();
                assert_eq!(m.kind, MediaKind::Video);
                assert_eq!(m.direct_path, "/v/vid");
            }
            other => panic!("{other:?}"),
        }
        let sticker = wa::Message {
            sticker_message: buffa_some(wa::message::StickerMessage {
                is_animated: Some(true),
                width: Some(512),
                height: Some(512),
                direct_path: Some("/v/st".into()),
                media_key: Some(vec![1; 32]),
                ..Default::default()
            }),
            ..Default::default()
        };
        match content_from_wa(&sticker) {
            Some(Content::Sticker {
                animated, media, ..
            }) => {
                assert!(animated);
                assert_eq!(media.unwrap().kind, MediaKind::Sticker);
            }
            other => panic!("{other:?}"),
        }
        // A video without a CDN reference still renders (thumbnail only).
        let bare = wa::Message {
            video_message: buffa_some(wa::message::VideoMessage::default()),
            ..Default::default()
        };
        assert!(matches!(
            content_from_wa(&bare),
            Some(Content::Video { media: None, .. })
        ));
    }

    #[test]
    fn history_edited_wrapper_shows_the_latest_text() {
        let wrapped = wa::Message {
            edited_message: buffa_some(wa::message::FutureProofMessage {
                message: buffa_some(build_text("latest text")),
            }),
            ..Default::default()
        };
        assert_eq!(
            content_from_wa(&wrapped),
            Some(Content::Text("latest text".into()))
        );
    }

    #[test]
    fn view_once_video_is_still_hidden() {
        let m = wa::Message {
            video_message: buffa_some(wa::message::VideoMessage {
                view_once: Some(true),
                direct_path: Some("/secret".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(content_from_wa(&m), Some(Content::ViewOnce));
    }

    #[test]
    fn history_reactions_and_replies_are_converted() {
        let wmi = wa::WebMessageInfo {
            key: buffa_some(wa::MessageKey {
                id: Some("H1".into()),
                from_me: Some(false),
                remote_jid: Some("120363@g.us".into()),
                participant: Some("34600@s.whatsapp.net".into()),
            }),
            message: buffa_some(build_reply("hello", &quote())),
            message_timestamp: Some(1_700_000_000),
            reactions: vec![
                wa::Reaction {
                    key: buffa_some(wa::MessageKey {
                        participant: Some("34611:2@s.whatsapp.net".into()),
                        from_me: Some(false),
                        ..Default::default()
                    }),
                    text: Some("😂".into()),
                    sender_timestamp_ms: Some(1_700_000_005_000),
                    ..Default::default()
                },
                wa::Reaction {
                    key: buffa_some(wa::MessageKey {
                        from_me: Some(true),
                        ..Default::default()
                    }),
                    text: Some("👍".into()),
                    ..Default::default()
                },
                // Empty text = no reaction.
                wa::Reaction {
                    text: Some(String::new()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let m = message_from_web_info("120363@g.us", &wmi, None).unwrap();
        assert_eq!(m.reply.unwrap().id, "ORIG");
        assert_eq!(m.reactions.len(), 2);
        assert_eq!(m.reactions[0].reactor, "34611@s.whatsapp.net");
        assert_eq!(m.reactions[0].ts, 1_700_000_005);
        assert_eq!(
            m.reactions[1].reactor, "",
            "own reactions are filled in later"
        );
    }

    #[test]
    fn chat_state_mapping() {
        use crate::net::types::ChatState::*;
        assert_eq!(
            chat_state(ChatPresence::Composing, ChatPresenceMedia::Text),
            Composing
        );
        assert_eq!(
            chat_state(ChatPresence::Composing, ChatPresenceMedia::Audio),
            Recording
        );
        assert_eq!(
            chat_state(ChatPresence::Paused, ChatPresenceMedia::Audio),
            Paused
        );
    }

    #[test]
    fn reaction_keys_name_the_author_in_groups_only() {
        let g = reaction_key("g@g.us", "ID", false, "34600@s.whatsapp.net", true);
        assert_eq!(g.participant.as_deref(), Some("34600@s.whatsapp.net"));
        assert_eq!(g.from_me, Some(false));
        let dm = reaction_key(
            "34600@s.whatsapp.net",
            "ID",
            false,
            "34600@s.whatsapp.net",
            false,
        );
        assert_eq!(dm.participant, None);
        let mine = reaction_key("g@g.us", "ID", true, "me@s.whatsapp.net", true);
        assert_eq!(mine.participant, None);
        assert_eq!(mine.from_me, Some(true));
    }
}

#[cfg(test)]
mod name_and_list_tests {
    use super::*;
    use wa::sync_action_value::{ContactAction, LabelEditAction, MuteAction, label_edit_action};

    #[test]
    fn contact_action_yields_saved_name_for_every_id() {
        let a = ContactAction {
            full_name: Some("Ana García".into()),
            first_name: Some("Ana".into()),
            lid_jid: Some("999@lid".into()),
            pn_jid: Some("34600111222@s.whatsapp.net".into()),
            ..Default::default()
        };
        let evs = contact_events("34600111222@s.whatsapp.net", &a);
        let names: Vec<_> = evs
            .iter()
            .filter_map(|e| match e {
                Event::ContactName { jid, name, source } => {
                    Some((jid.as_str(), name.as_str(), *source))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            names,
            vec![
                (
                    "34600111222@s.whatsapp.net",
                    "Ana García",
                    NameSource::Saved
                ),
                ("999@lid", "Ana García", NameSource::Saved),
            ]
        );
        assert!(
            matches!(&evs[0], Event::LidMappings(p) if p == &[("999@lid".to_string(), "34600111222@s.whatsapp.net".to_string())])
        );
    }

    /// The shape seen on current accounts: the mutation index is the LID and
    /// `pnJid` carries the phone number.
    #[test]
    fn a_lid_indexed_contact_action_names_the_phone_number_too() {
        let a = ContactAction {
            full_name: Some("Lucía Fernández".into()),
            first_name: Some("Lucía".into()),
            pn_jid: Some("34611111111@s.whatsapp.net".into()),
            save_on_primary_addressbook: Some(true),
            ..Default::default()
        };
        let evs = contact_events("205000111222333@lid", &a);
        assert!(evs.iter().any(|e| matches!(e, Event::ContactName { jid, name, source: NameSource::Saved } if jid == "34611111111@s.whatsapp.net" && name == "Lucía Fernández")));
        assert!(
            evs.iter().any(
                |e| matches!(e, Event::ContactName { jid, .. } if jid == "205000111222333@lid")
            )
        );
    }

    #[test]
    fn history_conversation_unread_and_display_name() {
        let conv = wa::Conversation {
            id: "34600111222@s.whatsapp.net".into(),
            display_name: Some("Ana (agenda)".into()),
            marked_as_unread: Some(true),
            unread_count: Some(0),
            ..Default::default()
        };
        let c = chat_from_conversation(&conv, 0);
        assert_eq!(c.unread, 1, "marked as unread shows one");
        assert_eq!(c.name.as_deref(), Some("Ana (agenda)"));
        let conv = wa::Conversation {
            id: "x@s.whatsapp.net".into(),
            unread_count: Some(4),
            marked_as_unread: Some(true),
            ..Default::default()
        };
        assert_eq!(chat_from_conversation(&conv, 0).unread, 4);
    }

    #[test]
    fn contact_action_falls_back_to_first_name_and_can_clear() {
        let a = ContactAction {
            first_name: Some("Ana".into()),
            ..Default::default()
        };
        let evs = contact_events("1@s.whatsapp.net", &a);
        assert!(matches!(&evs[..], [Event::ContactName { name, .. }] if name == "Ana"));
        // Neither name: the saved name is removed (empty name).
        let evs = contact_events("1@s.whatsapp.net", &ContactAction::default());
        assert!(
            matches!(&evs[..], [Event::ContactName { name, source: NameSource::Saved, .. }] if name.is_empty())
        );
    }

    #[test]
    fn label_edit_maps_kinds_and_deletion() {
        let custom = LabelEditAction {
            name: Some("Familia".into()),
            color: Some(4),
            order_index: Some(2),
            r#type: Some(label_edit_action::ListType::CUSTOM),
            ..Default::default()
        };
        match list_edit_event("7", &custom) {
            Event::ListEdit { def, deleted } => {
                assert!(!deleted);
                assert_eq!(def.id, "7");
                assert_eq!(def.name, "Familia");
                assert_eq!(def.kind, ListKind::Custom);
                assert_eq!((def.color, def.order), (4, 2));
            }
            e => panic!("{e:?}"),
        }
        let fav = LabelEditAction {
            r#type: Some(label_edit_action::ListType::FAVORITES),
            ..Default::default()
        };
        assert!(
            matches!(list_edit_event("f", &fav), Event::ListEdit { def, .. } if def.kind == ListKind::Favourites)
        );
        let business = LabelEditAction {
            name: Some("New customer".into()),
            ..Default::default()
        };
        assert!(
            matches!(list_edit_event("b", &business), Event::ListEdit { def, .. } if def.kind == ListKind::Other)
        );
        let gone = LabelEditAction {
            deleted: Some(true),
            ..Default::default()
        };
        assert!(matches!(
            list_edit_event("7", &gone),
            Event::ListEdit { deleted: true, .. }
        ));
    }

    #[test]
    fn outgoing_list_edit_is_a_custom_list() {
        let def = ListDef {
            id: "9".into(),
            name: "Trabajo".into(),
            color: 3,
            kind: ListKind::Custom,
            order: 1,
        };
        let a = list_edit_action(&def);
        assert_eq!(a.name.as_deref(), Some("Trabajo"));
        assert_eq!(a.deleted, Some(false));
        assert_eq!(a.r#type, Some(label_edit_action::ListType::CUSTOM));
        assert_eq!(a.order_index, Some(1));
    }

    #[test]
    fn chat_flags_from_sync_actions() {
        assert_eq!(
            archive_flag(&wa::sync_action_value::ArchiveChatAction {
                archived: Some(true),
                ..Default::default()
            }),
            ChatFlag::Archived(true)
        );
        assert_eq!(
            pin_flag(&wa::sync_action_value::PinAction::default()),
            ChatFlag::Pinned(false)
        );
        let mute = |muted, end| MuteAction {
            muted: Some(muted),
            mute_end_timestamp: Some(end),
            ..Default::default()
        };
        assert_eq!(mute_flag(&mute(true, -1)), ChatFlag::Muted(MUTE_FOREVER));
        // Sync actions count in milliseconds; the app keeps seconds.
        assert_eq!(
            mute_flag(&mute(true, 1_800_000_000_000)),
            ChatFlag::Muted(1_800_000_000)
        );
        assert_eq!(mute_flag(&mute(false, 0)), ChatFlag::Muted(MUTE_OFF));
        assert_eq!(mute_end_secs(1_800_000_000), 1_800_000_000);
        assert_eq!(mute_end_secs(0), MUTE_FOREVER);
        // An 8 hour mute set long ago is over.
        assert!(!mute_active(
            mute_end_secs(1_000_000_000_000),
            1_800_000_000
        ));
    }

    #[test]
    fn link_preview_is_read_from_extended_text() {
        let ext = wa::message::ExtendedTextMessage {
            text: Some("look https://example.com/x".into()),
            matched_text: Some("https://example.com/x".into()),
            title: Some("Example".into()),
            description: Some("A page".into()),
            jpeg_thumbnail: Some(vec![1, 2, 3]),
            ..Default::default()
        };
        let msg = wa::Message {
            extended_text_message: buffa_some(ext.clone()),
            ..Default::default()
        };
        match content_from_wa(&msg) {
            Some(Content::Link { text, preview }) => {
                assert_eq!(text, "look https://example.com/x");
                assert_eq!(preview.title, "Example");
                assert_eq!(preview.thumbnail, Some(vec![1, 2, 3]));
            }
            c => panic!("{c:?}"),
        }
        // No title: plain text. A non-http target never becomes a preview.
        let mut no_title = ext.clone();
        no_title.title = None;
        assert!(link_preview(&no_title).is_none());
        let mut bad = ext.clone();
        bad.matched_text = Some("javascript:alert(1)".into());
        assert!(link_preview(&bad).is_none());
        // An oversized thumbnail is dropped, the preview stays.
        let mut big = ext;
        big.jpeg_thumbnail = Some(vec![0; MAX_PREVIEW_THUMB + 1]);
        assert_eq!(link_preview(&big).unwrap().thumbnail, None);
    }
}

// ------------------------------------------------------------ communities

/// Builds a community from its parent metadata, the subgroups the server
/// lists for it (`known`, may include groups we are not in) and every group
/// we participate in (`groups`). Subgroups we are in that the listing missed
/// are derived from their `parent_group_jid`.
pub fn community_info(
    jid: &whatsapp_rust::Jid,
    meta: &whatsapp_rust::features::GroupMetadata,
    known: Option<&[whatsapp_rust::features::CommunitySubgroup]>,
    groups: &std::collections::HashMap<whatsapp_rust::Jid, whatsapp_rust::features::GroupMetadata>,
) -> super::social::CommunityInfo {
    use super::social::{CommunityInfo, SubGroup};
    let mut subgroups: Vec<SubGroup> = Vec::new();
    for sg in known.unwrap_or_default() {
        subgroups.push(SubGroup {
            jid: bare_jid(&sg.id.to_string()),
            name: sg.subject.clone(),
            announcement: sg.is_default_sub_group,
            general: sg.is_general_chat,
            joined: groups.contains_key(&sg.id),
            members: sg.participant_count,
        });
    }
    for (gid, g) in groups {
        let is_child = g.parent_group_jid.as_ref().is_some_and(|p| p == jid);
        let id = bare_jid(&gid.to_string());
        if is_child && !subgroups.iter().any(|s| s.jid == id) {
            subgroups.push(SubGroup {
                jid: id,
                name: g.subject.clone(),
                announcement: g.is_default_sub_group,
                general: g.is_general_chat,
                joined: true,
                members: g.size,
            });
        }
    }
    let mut info = CommunityInfo {
        jid: bare_jid(&jid.to_string()),
        name: meta.subject.clone(),
        description: meta.description.clone().filter(|d| !d.is_empty()),
        members: meta.size,
        subgroups,
    };
    info.sort();
    info
}

/// Picks the answer for `digits` out of the usync results.
pub fn phone_lookup(
    digits: &str,
    found: &[whatsapp_rust::features::IsOnWhatsAppResult],
) -> super::social::PhoneLookup {
    use super::social::{PhoneLookup, phone_jid};
    // The server may answer with the LID as primary id; match on the PN
    // we asked about.
    let hit = found
        .iter()
        .find(|r| r.jid.user == digits || r.pn_jid.as_ref().is_some_and(|p| p.user == digits));
    match hit {
        Some(r) if r.is_registered => PhoneLookup::OnWhatsApp {
            jid: phone_jid(digits),
            business: r.is_business,
        },
        Some(_) => PhoneLookup::NotOnWhatsApp,
        None if found.len() == 1 && found[0].is_registered => PhoneLookup::OnWhatsApp {
            jid: phone_jid(digits),
            business: found[0].is_business,
        },
        None => PhoneLookup::NotOnWhatsApp,
    }
}

/// App-state `star` update → domain change.
pub fn star_change(
    u: &whatsapp_rust::wacore::types::events::StarUpdate,
) -> super::social::StarChange {
    let chat = bare_jid(&u.chat_jid.to_string());
    let sender = match (&u.participant_jid, u.from_me) {
        (Some(p), false) => bare_jid(&p.to_string()),
        _ => String::new(),
    };
    super::social::StarChange {
        key: MsgKey {
            chat,
            id: u.message_id.clone(),
            from_me: u.from_me,
            sender,
        },
        starred: u.action.starred.unwrap_or(false),
        ts: u.timestamp.timestamp(),
    }
}

#[cfg(test)]
mod community_tests {
    use super::*;
    use std::collections::HashMap;
    use whatsapp_rust::Jid;
    use whatsapp_rust::features::GroupMetadata;

    fn group(id: &Jid, subject: &str, parent: Option<&Jid>) -> GroupMetadata {
        GroupMetadata {
            id: id.clone(),
            subject: subject.into(),
            parent_group_jid: parent.cloned(),
            ..Default::default()
        }
    }

    fn member(
        jid: &str,
        pn: Option<&str>,
        kind: whatsapp_rust::features::ParticipantType,
    ) -> whatsapp_rust::features::GroupParticipant {
        whatsapp_rust::features::GroupParticipant {
            jid: jid.parse().expect("jid"),
            phone_number: pn.map(|p| p.parse().expect("jid")),
            lid: None,
            username: None,
            participant_type: kind,
            details: None,
        }
    }

    #[test]
    fn group_metadata_lists_admins_first_by_phone_number() {
        use whatsapp_rust::features::ParticipantType as T;
        let id: Jid = "120363001@g.us".parse().expect("jid");
        let mut meta = group(&id, "Familia", None);
        meta.description = Some("  hola  ".into());
        meta.creation_time = Some(1_700_000_000);
        meta.participants = vec![
            member("222@lid", Some("34600000002@s.whatsapp.net"), T::Member),
            member("111@lid", None, T::Admin),
            member("34600000003@s.whatsapp.net", None, T::SuperAdmin),
        ];
        let g = group_info(&meta);
        assert_eq!(g.subject, "Familia");
        assert_eq!(g.description.as_deref(), Some("hola"));
        assert_eq!(g.created, Some(1_700_000_000));
        let jids: Vec<&str> = g.participants.iter().map(|p| p.jid.as_str()).collect();
        assert_eq!(
            jids,
            [
                "34600000003@s.whatsapp.net",
                "111@lid",
                "34600000002@s.whatsapp.net"
            ]
        );
        assert!(g.participants[0].owner && g.participants[0].admin);
        assert!(g.participants[1].admin && !g.participants[1].owner);
        assert_eq!(g.participants[2].alias.as_deref(), Some("222@lid"));
        // Groups in common: by phone number or LID.
        let mut groups = HashMap::new();
        groups.insert(id.clone(), meta);
        let other: Jid = "120363002@g.us".parse().expect("jid");
        groups.insert(other.clone(), group(&other, "Otro", None));
        assert_eq!(
            common_groups(&groups, &["34600000002@s.whatsapp.net".into()]),
            vec![("120363001@g.us".to_string(), "Familia".to_string())]
        );
        assert_eq!(common_groups(&groups, &["111@lid".into()]).len(), 1);
        assert!(common_groups(&groups, &["999@lid".into()]).is_empty());
        let c = contact_info(Some("  "), Some("Shop"));
        assert_eq!(c.about, None);
        assert_eq!(c.business_name.as_deref(), Some("Shop"));
    }

    #[test]
    fn subgroups_are_derived_from_the_parent_link_when_the_listing_is_missing() {
        let parent: Jid = "120363111@g.us".parse().expect("jid");
        let ann: Jid = "120363112@g.us".parse().expect("jid");
        let sub: Jid = "120363113@g.us".parse().expect("jid");
        let other: Jid = "120363999@g.us".parse().expect("jid");
        let other_parent: Jid = "120363888@g.us".parse().expect("jid");
        let mut meta = group(&parent, "Barrio", None);
        meta.is_parent_group = true;
        meta.size = Some(40);
        let mut a = group(&ann, "Avisos", Some(&parent));
        a.is_default_sub_group = true;
        let groups: HashMap<Jid, GroupMetadata> = [
            (ann.clone(), a),
            (sub.clone(), group(&sub, "Huerto", Some(&parent))),
            (other.clone(), group(&other, "Otro", Some(&other_parent))),
        ]
        .into_iter()
        .collect();
        let c = community_info(&parent, &meta, None, &groups);
        assert_eq!(c.name, "Barrio");
        assert_eq!(c.members, Some(40));
        let names: Vec<_> = c.subgroups.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            ["Avisos", "Huerto"],
            "announcement first, strangers out"
        );
        assert!(c.subgroups.iter().all(|s| s.joined));
        assert!(c.subgroups[0].announcement);
    }
}
