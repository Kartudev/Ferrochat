//! App-level flows against the fake protocol client: the real `App`, the
//! real store, the scripted fake. These are the integration tests for names,
//! archive, pins, lists and links.

use super::state::Filter;
use super::test_support::Harness;
use super::{MenuTarget, Ui};
use crate::mock::{self, OWN_JID, SHOWCASE_CHAT};
use crate::net::fake::{FakeOptions, Sent};
use crate::net::{ChatFlag, CollectionResync, Event, ListDef, ListKind, NameSource, ResyncReport};

const MARCOS: &str = "34622222222@s.whatsapp.net";
const PABLO: &str = "34655555555@s.whatsapp.net";
const CLINIC: &str = "34644444444@s.whatsapp.net";
const ARCHIVED_GROUP: &str = "120363000000000004@g.us";
const STRANGER: &str = "34600999888@s.whatsapp.net";

fn resyncs(h: &Harness) -> usize {
    h.fake
        .log()
        .iter()
        .filter(|s| matches!(s, Sent::Resync))
        .count()
}

#[tokio::test]
async fn names_follow_the_priority_in_the_running_app() {
    let h = Harness::new().await;
    let c = &h.app.chats;
    // Saved in the phone's address book beats the push name ("Lu").
    assert_eq!(c.display_name(SHOWCASE_CHAT), "Lucía Fernández");
    // Business verified name.
    assert_eq!(c.display_name(CLINIC), "Clínica Dental Sonrisa");
    // Only a push name: shown with "~".
    assert_eq!(c.display_name(PABLO), "~ Pablo");
    // Nothing known: the formatted number.
    assert_eq!(c.display_name(STRANGER), "+34 600 999 888");
    // Group senders use the same book, not the name carried by the message.
    assert_eq!(
        c.sender_name("34688888888@s.whatsapp.net", Some("Rosa")),
        "Tía Rosa"
    );
}

#[tokio::test]
async fn a_late_saved_name_replaces_the_push_name_and_survives_a_restart() {
    let mut h = Harness::new().await;
    h.net(Event::ContactName {
        jid: PABLO.into(),
        name: "Pablo Gómez".into(),
        source: NameSource::Saved,
    })
    .await;
    assert_eq!(h.app.chats.display_name(PABLO), "Pablo Gómez");
    // A push name arriving afterwards must not override it.
    h.net(Event::ContactName {
        jid: PABLO.into(),
        name: "Pablito".into(),
        source: NameSource::Push,
    })
    .await;
    assert_eq!(h.app.chats.display_name(PABLO), "Pablo Gómez");
    // Persisted: a fresh book built from the store shows the same.
    let book = super::state::NameBook::from_rows(h.app.store.contact_names().unwrap());
    assert_eq!(book.best(PABLO), Some((NameSource::Saved, "Pablo Gómez")));
    // The contact is removed from the address book: back to the push name.
    h.net(Event::ContactName {
        jid: PABLO.into(),
        name: String::new(),
        source: NameSource::Saved,
    })
    .await;
    assert_eq!(h.app.chats.display_name(PABLO), "~ Pablito");
}

#[tokio::test]
async fn lid_contacts_get_the_name_under_their_phone_number() {
    let mut h = Harness::new().await;
    h.net(Event::LidMappings(vec![(
        "111222333@lid".into(),
        STRANGER.into(),
    )]))
    .await;
    h.net(Event::ContactName {
        jid: "111222333@lid".into(),
        name: "Nueva Persona".into(),
        source: NameSource::Saved,
    })
    .await;
    assert_eq!(h.app.chats.display_name(STRANGER), "Nueva Persona");
}

#[tokio::test]
async fn existing_sessions_get_one_full_app_state_resync() {
    let mut h = Harness::new().await;
    assert_eq!(resyncs(&h), 1, "requested right after connecting");
    assert_eq!(
        h.app.store.kv("appstate_resync").unwrap().as_deref(),
        Some("3")
    );
    // Reconnecting does not ask again.
    h.net(Event::Connected {
        jid: Some(OWN_JID.into()),
    })
    .await;
    assert_eq!(resyncs(&h), 1);
}

#[tokio::test]
async fn the_resync_marker_in_the_cache_prevents_repeats_across_runs() {
    let mut cfg = crate::config::Config::defaults();
    cfg.mock = true;
    let (mut app, _) = super::App::boot(cfg);
    app.store.set_kv("appstate_resync", "3").unwrap();
    let (fake, _stream) = crate::net::fake::start(FakeOptions::default(), mock::mock_data(1));
    app.client = Some(fake.clone());
    let _ = app.update(super::Message::Net(Event::Connected {
        jid: Some(OWN_JID.into()),
    }));
    assert!(fake.log().iter().all(|s| !matches!(s, Sent::Resync)));
}

/// v0.8.0 wrote marker "1" although the library had dropped the snapshot as
/// stale: those caches must resync again.
#[tokio::test]
async fn the_v080_marker_does_not_count() {
    let mut cfg = crate::config::Config::defaults();
    cfg.mock = true;
    let (app, _) = super::App::boot(cfg);
    app.store.set_kv("appstate_resync", "1").unwrap();
    let (fake, stream) = crate::net::fake::start(FakeOptions::default(), mock::mock_data(1));
    let mut h = Harness { app, fake };
    drop(stream);
    h.app.client = Some(h.fake.clone());
    h.net(Event::Connected {
        jid: Some(OWN_JID.into()),
    })
    .await;
    assert_eq!(resyncs(&h), 1);
    assert_eq!(
        h.app.store.kv("appstate_resync").unwrap().as_deref(),
        Some("3")
    );
}

fn report(applied: bool, contacts: usize) -> ResyncReport {
    ResyncReport {
        collections: vec![CollectionResync {
            name: "critical_unblock_low",
            before: 7,
            after: if applied { 8 } else { 0 },
        }],
        had_sync_key: applied,
        contacts,
    }
}

#[tokio::test]
async fn a_resync_that_delivered_nothing_is_retried_and_not_marked_done() {
    let mut cfg = crate::config::Config::defaults();
    cfg.mock = true;
    let (app, _) = super::App::boot(cfg);
    let (fake, stream) = crate::net::fake::start(FakeOptions::default(), mock::mock_data(1));
    drop(stream);
    // 1: snapshot not applied (keys missing); 2: applied but no contact
    // arrived; 3: the address book arrives.
    fake.script_resync(vec![report(false, 0), report(true, 0), report(true, 42)]);
    let mut h = Harness { app, fake };
    h.app.client = Some(h.fake.clone());
    h.net(Event::Connected {
        jid: Some(OWN_JID.into()),
    })
    .await;
    assert_eq!(resyncs(&h), 3, "retried until the contacts came");
    assert_eq!(
        h.app.store.kv("appstate_resync").unwrap().as_deref(),
        Some("3")
    );
}

#[tokio::test]
async fn a_resync_failing_every_time_stops_for_this_run_and_retries_next_start() {
    let mut cfg = crate::config::Config::defaults();
    cfg.mock = true;
    let (app, _) = super::App::boot(cfg);
    let (fake, stream) = crate::net::fake::start(FakeOptions::default(), mock::mock_data(1));
    drop(stream);
    fake.script_resync((0..10).map(|_| report(false, 0)).collect());
    let mut h = Harness { app, fake };
    h.app.client = Some(h.fake.clone());
    h.net(Event::Connected {
        jid: Some(OWN_JID.into()),
    })
    .await;
    assert_eq!(resyncs(&h), super::RESYNC_MAX_ATTEMPTS as usize);
    assert_eq!(h.app.store.kv("appstate_resync").unwrap(), None);
    // A reconnect in the same run does not hammer the server again.
    h.net(Event::Connected {
        jid: Some(OWN_JID.into()),
    })
    .await;
    assert_eq!(resyncs(&h), super::RESYNC_MAX_ATTEMPTS as usize);
}

/// The real-account shape: the chat is keyed by the phone number and only
/// has a push name; the address-book entry arrives keyed by the LID with no
/// `pnJid`, and the pair is only known to the protocol library.
#[tokio::test]
async fn a_lid_keyed_contact_names_the_phone_number_chat() {
    use crate::net::convert::contact_events;
    use whatsapp_rust::waproto::whatsapp as wa;
    let mut h = Harness::new().await;
    assert_eq!(h.app.chats.display_name(PABLO), "~ Pablo");
    // The library already knew the pair (usync / earlier history sync).
    h.fake
        .set_known_lid_pairs(vec![("205000111222333@lid".into(), PABLO.into())]);
    h.net(Event::Connected {
        jid: Some(OWN_JID.into()),
    })
    .await;
    let action = wa::sync_action_value::ContactAction {
        full_name: Some("Pablo Gómez".into()),
        first_name: Some("Pablo".into()),
        lid_jid: None,
        pn_jid: None,
        ..Default::default()
    };
    for ev in contact_events("205000111222333@lid", &action) {
        h.net(ev).await;
    }
    assert_eq!(h.app.chats.display_name(PABLO), "Pablo Gómez");
    let st = h.app.chats.name_stats();
    assert!(st.chats_saved >= 1);
}

/// A name stored under the LID before the pair was known still shows on the
/// phone-number chat once the pair arrives (and after a restart).
#[tokio::test]
async fn a_name_learned_before_its_lid_pair_is_found_later() {
    let mut h = Harness::new().await;
    h.net(Event::ContactName {
        jid: "205000999@lid".into(),
        name: "Desconocida Ahora".into(),
        source: NameSource::Saved,
    })
    .await;
    assert_eq!(h.app.chats.display_name(STRANGER), "+34 600 999 888");
    h.net(Event::LidMappings(vec![(
        "205000999@lid".into(),
        STRANGER.into(),
    )]))
    .await;
    assert_eq!(h.app.chats.display_name(STRANGER), "Desconocida Ahora");
}

#[tokio::test]
async fn sessions_synced_before_stars_get_one_more_snapshot() {
    let mut h = Harness::new().await;
    assert_eq!(resyncs(&h), 1);
    // A v0.8 / pre-fix development cache: its marker does not count, one
    // real resync covers names and stars.
    h.app.store.set_kv("appstate_resync", "2").unwrap();
    h.app.resync = super::Resync::default();
    h.net(Event::Connected {
        jid: Some(OWN_JID.into()),
    })
    .await;
    assert_eq!(resyncs(&h), 2);
    assert_eq!(
        h.app.store.kv("appstate_resync").unwrap().as_deref(),
        Some("3")
    );
}

#[tokio::test]
async fn archived_chats_are_hidden_listed_and_unarchivable() {
    let mut h = Harness::new().await;
    assert_eq!(h.app.chats.archived_count(), 2);
    let main = h.app.chats.filtered_view("", &Filter::All, false);
    assert!(main.iter().all(|e| !e.meta.archived));
    assert_eq!(main.len(), 10);

    h.ui(Ui::OpenArchived).await;
    assert!(h.app.archived_view);
    let archived = h.app.chats.filtered_view("", &Filter::All, true);
    assert_eq!(archived.len(), 2);

    // An archived chat opens like any other.
    h.ui(Ui::OpenChat(ARCHIVED_GROUP.into())).await;
    assert_eq!(
        h.app.open.as_ref().map(|c| c.chat.as_str()),
        Some(ARCHIVED_GROUP)
    );

    // Unarchive from the context menu: synced to the phone, persisted, listed.
    h.ui(Ui::ToggleArchive(ARCHIVED_GROUP.into())).await;
    assert!(h.fake.log().contains(&Sent::Archive {
        chat: ARCHIVED_GROUP.into(),
        archived: false
    }));
    assert_eq!(h.app.chats.archived_count(), 1);
    let rows = h.app.store.chat_rows().unwrap();
    assert!(
        !rows
            .iter()
            .find(|r| r.meta.id == ARCHIVED_GROUP)
            .unwrap()
            .meta
            .archived
    );

    h.ui(Ui::CloseArchived).await;
    assert!(!h.app.archived_view);
    assert_eq!(h.app.chats.filtered_view("", &Filter::All, false).len(), 11);

    // Archive another one.
    h.ui(Ui::ToggleArchive(MARCOS.into())).await;
    assert!(h.fake.log().contains(&Sent::Archive {
        chat: MARCOS.into(),
        archived: true
    }));
    assert!(h.app.chats.get(MARCOS).unwrap().meta.archived);
}

#[tokio::test]
async fn archive_done_on_the_phone_arrives_through_app_state() {
    let mut h = Harness::new().await;
    h.net(Event::ChatFlag {
        chat: MARCOS.into(),
        flag: ChatFlag::Archived(true),
    })
    .await;
    assert!(h.app.chats.get(MARCOS).unwrap().meta.archived);
    assert!(
        h.app
            .store
            .chat_rows()
            .unwrap()
            .iter()
            .any(|r| r.meta.id == MARCOS && r.meta.archived)
    );
    // The phone does not get an echo of its own change.
    assert!(
        h.fake
            .log()
            .iter()
            .all(|s| !matches!(s, Sent::Archive { .. }))
    );
    h.net(Event::ChatFlag {
        chat: MARCOS.into(),
        flag: ChatFlag::Archived(false),
    })
    .await;
    assert!(!h.app.chats.get(MARCOS).unwrap().meta.archived);
}

#[tokio::test]
async fn a_failed_archive_is_reverted_with_a_message() {
    let mut h = Harness::with(FakeOptions {
        auto_receipts: false,
        fail_sends: true,
        ..FakeOptions::default()
    })
    .await;
    h.ui(Ui::ToggleArchive(MARCOS.into())).await;
    assert!(!h.app.chats.get(MARCOS).unwrap().meta.archived, "reverted");
    assert!(h.app.toast.is_some());
    assert!(
        !h.app
            .store
            .chat_rows()
            .unwrap()
            .iter()
            .any(|r| r.meta.id == MARCOS && r.meta.archived)
    );
}

#[tokio::test]
async fn at_most_three_chats_can_be_pinned() {
    let mut h = Harness::new().await;
    // The showcase chat is already pinned.
    assert_eq!(h.app.chats.pinned_count(), 1);
    for chat in [MARCOS, PABLO] {
        h.ui(Ui::TogglePin(chat.into())).await;
    }
    assert_eq!(h.app.chats.pinned_count(), 3);
    h.ui(Ui::TogglePin(CLINIC.into())).await;
    assert_eq!(h.app.chats.pinned_count(), 3);
    assert!(
        h.app
            .toast
            .as_deref()
            .unwrap_or_default()
            .contains("pin up to 3")
    );
    let pins = h
        .fake
        .log()
        .into_iter()
        .filter(|s| matches!(s, Sent::Pin { .. }))
        .count();
    assert_eq!(pins, 2);
    // Unpinning works at the limit.
    h.ui(Ui::TogglePin(MARCOS.into())).await;
    assert_eq!(h.app.chats.pinned_count(), 2);
    assert!(h.fake.log().contains(&Sent::Pin {
        chat: MARCOS.into(),
        pinned: false
    }));
}

#[tokio::test]
async fn lists_from_the_phone_become_chips_with_members() {
    let h = Harness::new().await;
    let names: Vec<_> = h.app.chats.custom_lists().map(|l| l.name.clone()).collect();
    assert_eq!(names, ["Familia", "Trabajo", "Viajes"]);
    assert!(h.app.chats.in_list("1", "120363000000000001@g.us"));
    // Persisted in the cache too.
    let (defs, members) = h.app.store.load_lists().unwrap();
    assert_eq!(defs.len(), 4);
    assert!(members.contains(&("2".into(), "120363000000000002@g.us".into())));
}

#[tokio::test]
async fn creating_renaming_and_deleting_a_list_syncs_to_the_phone() {
    let mut h = Harness::new().await;
    h.ui(Ui::NewList).await;
    assert!(h.app.list_dialog.is_some());
    h.ui(Ui::ListName("Amigos".into())).await;
    h.ui(Ui::SaveList).await;
    assert!(h.app.list_dialog.is_none());
    let created = h
        .app
        .chats
        .custom_lists()
        .find(|l| l.name == "Amigos")
        .cloned()
        .expect("list created");
    assert_eq!(
        created.id, "4",
        "next free number after the lists 1, 2 and 3"
    );
    assert_eq!(
        h.app.filter,
        Filter::List(created.id.clone()),
        "opens the new list"
    );
    assert!(h.fake.log().contains(&Sent::SaveList(created.clone())));
    assert!(
        h.app
            .store
            .load_lists()
            .unwrap()
            .0
            .iter()
            .any(|l| l.name == "Amigos")
    );

    // Add a chat to it: membership is synced and the filter shows it.
    h.ui(Ui::ToggleInList {
        chat: MARCOS.into(),
        list: created.id.clone(),
    })
    .await;
    assert!(h.fake.log().contains(&Sent::ListMember {
        list: created.id.clone(),
        chat: MARCOS.into(),
        member: true
    }));
    let rows = h.app.chats.filtered_view("", &h.app.filter, false);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].meta.id, MARCOS);
    h.ui(Ui::ToggleInList {
        chat: MARCOS.into(),
        list: created.id.clone(),
    })
    .await;
    assert!(
        h.app
            .chats
            .filtered_view("", &h.app.filter, false)
            .is_empty()
    );

    // Rename.
    h.ui(Ui::RenameList(created.id.clone())).await;
    h.ui(Ui::ListName("Colegas".into())).await;
    h.ui(Ui::SaveList).await;
    assert_eq!(h.app.chats.list(&created.id).unwrap().name, "Colegas");
    assert!(
        h.fake
            .log()
            .iter()
            .any(|s| matches!(s, Sent::SaveList(d) if d.id == created.id && d.name == "Colegas"))
    );

    // Delete.
    h.ui(Ui::AskDeleteList(created.id.clone())).await;
    h.ui(Ui::ConfirmDeleteList(created.id.clone())).await;
    assert!(h.app.chats.list(&created.id).is_none());
    assert_eq!(h.app.filter, Filter::All);
    assert!(h.fake.log().contains(&Sent::DeleteList(created.id.clone())));
    assert!(
        h.app
            .store
            .load_lists()
            .unwrap()
            .0
            .iter()
            .all(|l| l.id != created.id)
    );
}

#[tokio::test]
async fn duplicate_and_empty_list_names_are_refused() {
    let mut h = Harness::new().await;
    h.ui(Ui::NewList).await;
    h.ui(Ui::ListName("familia".into())).await;
    h.ui(Ui::SaveList).await;
    assert!(h.app.list_dialog.is_some(), "dialog stays open");
    assert!(
        h.app
            .toast
            .as_deref()
            .unwrap_or_default()
            .contains("already exists")
    );
    h.ui(Ui::ListName("   ".into())).await;
    h.ui(Ui::SaveList).await;
    assert!(h.app.list_dialog.is_some());
    assert!(h.fake.log().iter().all(|s| !matches!(s, Sent::SaveList(_))));
}

#[tokio::test]
async fn list_edits_from_the_phone_update_chips_live() {
    let mut h = Harness::new().await;
    h.net(Event::ListEdit {
        def: ListDef {
            id: "9".into(),
            name: "Nuevo".into(),
            kind: ListKind::Custom,
            order: 9,
            ..Default::default()
        },
        deleted: false,
    })
    .await;
    assert!(h.app.chats.custom_lists().any(|l| l.name == "Nuevo"));
    h.ui(Ui::Filter(Filter::List("9".into()))).await;
    h.net(Event::ListEdit {
        def: ListDef {
            id: "9".into(),
            ..Default::default()
        },
        deleted: true,
    })
    .await;
    assert!(h.app.chats.list("9").is_none());
    assert_eq!(h.app.filter, Filter::All, "the open list was deleted");
}

#[tokio::test]
async fn favourites_sync_through_the_phones_list_when_there_is_one() {
    let mut h = Harness::new().await;
    // The mock phone has a Favourites list with the showcase chat and Mamá.
    assert!(h.app.chats.is_favourite(SHOWCASE_CHAT));
    assert_eq!(h.app.chats.filtered("", Filter::Favourites).len(), 2);
    h.ui(Ui::ToggleFavourite(MARCOS.into())).await;
    assert!(h.app.chats.is_favourite(MARCOS));
    assert!(h.fake.log().contains(&Sent::ListMember {
        list: "fav".into(),
        chat: MARCOS.into(),
        member: true
    }));
    h.ui(Ui::ToggleFavourite(SHOWCASE_CHAT.into())).await;
    assert!(!h.app.chats.is_favourite(SHOWCASE_CHAT));
}

#[tokio::test]
async fn context_menus_remember_where_they_were_opened() {
    let mut h = Harness::new().await;
    crate::app::input::set(iced::Point::new(120.0, 300.0));
    h.ui(Ui::ChatMenu(MARCOS.into())).await;
    let m = h.app.sidebar_menu.clone().unwrap();
    assert_eq!(m.target, MenuTarget::Chat(MARCOS.into()));
    assert_eq!(m.at, iced::Point::new(120.0, 300.0));
    h.ui(Ui::ChipMenu("1".into())).await;
    assert_eq!(
        h.app.sidebar_menu.as_ref().unwrap().target,
        MenuTarget::Chip("1".into())
    );
    // Escape closes the menu before anything else.
    h.ui(Ui::CloseChat).await;
    assert!(h.app.sidebar_menu.is_none());
}

#[tokio::test]
async fn only_web_links_are_opened() {
    let mut h = Harness::new().await;
    h.ui(Ui::OpenLink("file:///etc/passwd".into())).await;
    assert!(
        h.app
            .toast
            .as_deref()
            .unwrap_or_default()
            .contains("cannot be opened")
    );
}

#[tokio::test]
async fn a_message_push_name_does_not_replace_the_saved_name_in_the_list() {
    let mut h = Harness::new().await;
    let mut msg = h.app.chats.get(MARCOS).unwrap().last.clone().unwrap();
    msg.id = "NEWMSG".into();
    msg.sender_name = Some("Marquitos".into());
    msg.from_me = false;
    msg.timestamp += 10;
    h.net(Event::Messages {
        messages: vec![msg],
        origin: crate::net::Origin::Live,
    })
    .await;
    assert_eq!(h.app.chats.display_name(MARCOS), "Marcos Díaz");
}

fn pasted_screenshot() -> Box<super::paste::Staged> {
    Box::new(super::paste::Staged::from_image(&super::paste::RawImage {
        width: 40,
        height: 30,
        rgba: vec![120; 40 * 30 * 4],
    }))
}

#[tokio::test]
async fn a_pasted_image_opens_the_preview_and_sends_with_its_caption() {
    use super::message::{Done, Message};
    use crate::net::Content;
    let mut h = Harness::new().await;
    h.ui(Ui::OpenChat(PABLO.into())).await;
    let before = h.app.open.as_ref().map(|c| c.messages.len()).unwrap_or(0);
    let t = h.app.update(Message::done(Done::Staged {
        chat: PABLO.into(),
        staged: Some(pasted_screenshot()),
    }));
    h.pump(t).await;
    let preview = h.app.attach.as_ref().expect("preview open");
    assert!(preview.handle.is_some(), "handle made in update");
    h.ui(Ui::AttachCaption("  mira  ".into())).await;
    h.ui(Ui::AttachSend).await;
    assert!(h.app.attach.is_none());
    let conv = h.app.open.as_ref().unwrap();
    assert_eq!(conv.messages.len(), before + 1);
    let last = conv.messages.back().unwrap();
    assert!(last.from_me);
    match &last.content {
        Content::Image {
            caption,
            width,
            height,
            media,
            ..
        } => {
            assert_eq!(caption.as_deref(), Some("mira"));
            assert_eq!((width, height), (&40, &30));
            assert_eq!(
                media.as_ref().map(|m| m.mimetype.as_str()),
                Some("image/jpeg")
            );
        }
        other => panic!("not an image: {other:?}"),
    }
}

#[tokio::test]
async fn the_preview_is_dropped_by_escape_and_by_a_chat_switch() {
    use super::message::{Done, Message};
    let mut h = Harness::new().await;
    h.ui(Ui::OpenChat(PABLO.into())).await;
    let stage = |chat: &str| {
        Message::done(Done::Staged {
            chat: chat.into(),
            staged: Some(pasted_screenshot()),
        })
    };
    // Escape closes the preview, not the chat.
    let t = h.app.update(stage(PABLO));
    h.pump(t).await;
    h.ui(Ui::CloseChat).await;
    assert!(h.app.attach.is_none());
    assert!(h.app.open.is_some());
    // A paste that finishes after the user moved to another chat is ignored.
    let t = h.app.update(stage(MARCOS));
    h.pump(t).await;
    assert!(h.app.attach.is_none());
    // Opening another chat discards an open preview; nothing was sent.
    let n = h.app.open.as_ref().unwrap().messages.len();
    let t = h.app.update(stage(PABLO));
    h.pump(t).await;
    h.ui(Ui::AttachCancel).await;
    assert!(h.app.attach.is_none());
    assert_eq!(h.app.open.as_ref().unwrap().messages.len(), n);
    let t = h.app.update(stage(PABLO));
    h.pump(t).await;
    h.ui(Ui::OpenChat(MARCOS.into())).await;
    assert!(h.app.attach.is_none());
    // A text paste stages nothing.
    let t = h.app.update(Message::done(Done::Staged {
        chat: MARCOS.into(),
        staged: None,
    }));
    h.pump(t).await;
    assert!(h.app.attach.is_none());
}

// ------------------------------------------------ composer, emoji, viewer

fn type_text(s: &str) -> Vec<Ui> {
    use iced::widget::text_editor::{Action, Edit};
    s.chars()
        .map(|c| {
            Ui::ComposerAction(Action::Edit(if c == '\n' {
                Edit::Enter
            } else {
                Edit::Insert(c)
            }))
        })
        .collect()
}

#[tokio::test]
async fn shift_enter_lines_are_sent_as_one_message() {
    let mut h = Harness::new().await;
    h.ui(Ui::OpenChat(MARCOS.into())).await;
    // Shift+Enter is bound to the editor's own Enter (a newline).
    for u in type_text("first line\nsecond line") {
        h.ui(u).await;
    }
    assert_eq!(h.app.composer.text(), "first line\nsecond line");
    h.ui(Ui::Send).await;
    assert!(h.app.composer.is_blank());
    assert!(h.fake.log().iter().any(|s| matches!(
        s,
        Sent::Text { chat, text, .. } if chat == MARCOS && text == "first line\nsecond line"
    )));
}

#[tokio::test]
async fn drafts_survive_switching_chats() {
    let mut h = Harness::new().await;
    h.ui(Ui::OpenChat(MARCOS.into())).await;
    for u in type_text("half\nwritten") {
        h.ui(u).await;
    }
    h.ui(Ui::OpenChat(PABLO.into())).await;
    assert!(h.app.composer.is_blank());
    assert_eq!(h.app.drafts.get(MARCOS), Some("half\nwritten"));
    h.ui(Ui::OpenChat(MARCOS.into())).await;
    assert_eq!(h.app.composer.text(), "half\nwritten");
    assert!(h.app.drafts.get(MARCOS).is_none());
}

#[tokio::test]
async fn picked_emoji_go_into_the_composer_and_the_recent_list() {
    let mut h = Harness::new().await;
    h.ui(Ui::OpenChat(MARCOS.into())).await;
    h.ui(Ui::ToggleEmoji).await;
    assert!(h.app.emoji_open);
    h.ui(Ui::Emoji("\u{1F44B}")).await;
    h.ui(Ui::Emoji("\u{1F1EA}\u{1F1F8}")).await;
    assert_eq!(h.app.composer.text(), "\u{1F44B}\u{1F1EA}\u{1F1F8}");
    assert_eq!(
        h.app.emoji.recent,
        vec!["\u{1F1EA}\u{1F1F8}".to_string(), "\u{1F44B}".to_string()]
    );
    let stored = h.app.store.kv(super::emoji::RECENT_KEY).unwrap().unwrap();
    assert_eq!(super::emoji::parse_recent(&stored), h.app.emoji.recent);
    h.ui(Ui::EmojiSearch("pizza".into())).await;
    assert_eq!(
        h.app.emoji.sections()[0].emoji,
        vec!["\u{1F355}".to_string()]
    );
}

#[tokio::test]
async fn lid_mentions_show_names_in_the_chat_list_preview() {
    let mut h = Harness::new().await;
    let group = "120363000000000001@g.us";
    h.net(Event::LidMappings(vec![(
        "987654321098765@lid".into(),
        MARCOS.into(),
    )]))
    .await;
    let mut m = h
        .app
        .chats
        .get(group)
        .and_then(|e| e.last.clone())
        .expect("group has a message");
    m.id = "MENTION1".into();
    m.timestamp += 10;
    m.content = crate::net::Content::Text("@987654321098765 are you coming?".into());
    m.mentions = vec!["987654321098765@lid".into()];
    h.net(Event::Messages {
        messages: vec![m],
        origin: crate::net::Origin::Live,
    })
    .await;
    let last = h.app.chats.get(group).and_then(|e| e.last.clone()).unwrap();
    let name = h.app.chats.plain_name(MARCOS);
    assert_eq!(h.app.preview(&last), format!("@{name} are you coming?"));
    assert!(!h.app.preview(&last).contains("987654321098765"));
}

#[tokio::test]
async fn the_image_viewer_opens_zooms_steps_and_frees_its_picture() {
    let mut h = Harness::new().await;
    h.ui(Ui::OpenChat(SHOWCASE_CHAT.into())).await;
    let ids = h.app.viewer_ids();
    assert!(!ids.is_empty(), "the showcase chat has photos");
    h.ui(Ui::OpenImage(ids[0].clone())).await;
    assert!(h.app.viewer.is_some());
    assert!(h.app.viewer_handle().is_some(), "full picture loaded");
    h.ui(Ui::ViewerZoom {
        factor: 2.0,
        at: iced::Vector::ZERO,
    })
    .await;
    assert_eq!(h.app.viewer.as_ref().unwrap().zoom, 2.0);
    h.ui(Ui::ViewerStep(-1)).await;
    assert_eq!(
        h.app.viewer.as_ref().unwrap().id,
        ids[0],
        "no photo before the first"
    );
    if ids.len() > 1 {
        h.ui(Ui::Arrow(1)).await;
        let v = h.app.viewer.as_ref().unwrap();
        assert_eq!(v.id, ids[1]);
        assert_eq!(v.zoom, 1.0, "a new photo starts fitted");
    }
    // Esc closes it and drops the viewer's own copy.
    h.ui(Ui::CloseChat).await;
    assert!(h.app.viewer.is_none());
    assert!(h.app.open.is_some(), "Esc closed only the viewer");
}

#[tokio::test]
async fn a_video_without_inline_thumbnail_gets_a_cached_poster_frame() {
    use crate::net::Content;
    if crate::media::video::helper_path().is_none() {
        eprintln!("skipped: the ferrochat-player helper is not built");
        return;
    }
    let mut h = Harness::new().await;
    h.ui(Ui::OpenChat(SHOWCASE_CHAT.into())).await;
    // The grey-placeholder case: a video that came without jpegThumbnail.
    let id = {
        let c = h.app.open.as_mut().unwrap();
        let m = c
            .messages
            .iter_mut()
            .find(|m| matches!(m.content, Content::Video { .. }))
            .expect("showcase video");
        if let Content::Video { thumbnail, .. } = &mut m.content {
            *thumbnail = None;
        }
        m.id.clone()
    };
    h.app.thumbs.remove(&id);
    let t = h.app.want_poster();
    h.pump(t).await;
    assert!(
        h.app.thumbs.contains_key(&id),
        "poster handle made in update"
    );
    // Cached: the poster is the stored thumbnail now, never extracted again.
    let stored = h.app.store.message(SHOWCASE_CHAT, &id).unwrap().unwrap();
    assert!(matches!(
        stored.content,
        Content::Video {
            thumbnail: Some(_),
            ..
        }
    ));
    let t = h.app.want_poster();
    h.pump(t).await;
    assert!(h.app.posters_tried.contains(&id));
}

#[tokio::test]
async fn the_header_opens_the_contact_info_drawer() {
    use super::info::Load;
    let mut h = Harness::new().await;
    h.ui(Ui::OpenChat(SHOWCASE_CHAT.into())).await;
    h.ui(Ui::ToggleInfo).await;
    let info = h.app.info.as_ref().expect("drawer open");
    assert_eq!(info.chat, SHOWCASE_CHAT);
    assert_eq!(
        info.contact.ready().and_then(|c| c.about.as_deref()),
        Some(mock::about_for(SHOWCASE_CHAT))
    );
    assert_eq!(info.blocked, Load::Ready(false));
    assert!(matches!(info.common, Load::Ready(_)));
    assert!(
        info.counts.0 > 0,
        "the showcase chat has photos and a video"
    );
    assert!(!info.recent.is_empty());
    // Mute asks for how long, then is optimistic and synced.
    h.ui(Ui::ToggleMute(SHOWCASE_CHAT.into())).await;
    assert!(h.app.chat_dialog.is_some());
    h.ui(Ui::ChatAct(super::chat_actions::ChatAct::ConfirmMute))
        .await;
    assert!(h.app.is_muted(SHOWCASE_CHAT));
    assert!(h.fake.log().iter().any(|s| matches!(
        s,
        Sent::Mute { chat, end } if chat == SHOWCASE_CHAT && *end > 0
    )));
    // The switch unmutes at once.
    h.ui(Ui::ToggleMute(SHOWCASE_CHAT.into())).await;
    assert!(!h.app.is_muted(SHOWCASE_CHAT));
    // Block asks first.
    h.ui(Ui::AskBlock).await;
    assert!(h.app.info.as_ref().unwrap().confirm_block);
    h.ui(Ui::SetBlocked(true)).await;
    let info = h.app.info.as_ref().unwrap();
    assert_eq!(info.blocked, Load::Ready(true));
    assert!(!info.confirm_block);
    // The gallery lists the photos with thumbnails made in update.
    h.ui(Ui::InfoGallery(Some(super::info::Tab::Media))).await;
    let (_, items) = h.app.info.as_ref().unwrap().gallery.as_ref().unwrap();
    assert!(items.iter().any(|i| i.thumb.is_some()));
    // Esc goes back layer by layer: the drawer closes, the chat stays.
    h.ui(Ui::CloseChat).await;
    assert!(h.app.info.is_none());
    assert!(h.app.open.is_some());
}

#[tokio::test]
async fn the_group_info_drawer_lists_members_with_admins_first() {
    let mut h = Harness::new().await;
    let family = "120363000000000001@g.us";
    h.ui(Ui::OpenChat(family.into())).await;
    h.ui(Ui::ToggleInfo).await;
    let info = h.app.info.as_ref().unwrap();
    let g = info.group.ready().expect("metadata");
    assert!(g.participants.len() >= 2);
    assert!(g.participants[0].admin && g.participants[0].owner);
    assert_eq!(g.participants[0].jid, OWN_JID);
    assert!(g.description.is_some());
    // A second click on the header closes it.
    h.ui(Ui::ToggleInfo).await;
    assert!(h.app.info.is_none());
}

// ------------------------------------------------------------ read state

fn incoming(chat: &str, id: &str, ts: i64) -> crate::net::Message {
    crate::net::Message {
        id: id.into(),
        chat: chat.into(),
        sender: chat.into(),
        sender_name: None,
        from_me: false,
        timestamp: ts,
        content: crate::net::Content::Text(format!("hi {id}")),
        status: crate::net::ReceiptStatus::Read,
        expires_at: None,
        reply: None,
        edited: false,
        forwarded: None,
        mentions: Vec::new(),
        reactions: Vec::new(),
    }
}

async fn arrive(h: &mut Harness, m: crate::net::Message) {
    h.net(Event::Messages {
        messages: vec![m],
        origin: crate::net::Origin::Live,
    })
    .await;
}

fn read_sent(h: &Harness, id: &str) -> Option<String> {
    h.fake.log().into_iter().find_map(|s| match s {
        Sent::Read { chat, ids } if ids.iter().any(|i| i == id) => Some(chat),
        _ => None,
    })
}

fn unread(h: &Harness, chat: &str) -> u32 {
    h.app.chats.get(chat).map(|e| e.meta.unread).unwrap_or(0)
}

#[tokio::test]
async fn a_message_in_the_chat_being_read_is_read_at_once() {
    let mut h = Harness::new().await;
    h.ui(Ui::OpenChat(PABLO.into())).await;
    let _ = h.app.update(super::Message::Focus(true));
    arrive(&mut h, incoming(PABLO, "R1", 2_000_000_000)).await;
    assert_eq!(unread(&h, PABLO), 0);
    assert_eq!(read_sent(&h, "R1").as_deref(), Some(PABLO));
    assert!(h.app.open.as_ref().unwrap().unread.is_none(), "no divider");
}

#[tokio::test]
async fn an_unfocused_window_keeps_the_open_chat_unread_until_it_comes_back() {
    let mut h = Harness::new().await;
    h.ui(Ui::OpenChat(MARCOS.into())).await;
    let t = h.app.update(super::Message::Focus(false));
    h.pump(t).await;
    arrive(&mut h, incoming(MARCOS, "U1", 2_000_000_000)).await;
    arrive(&mut h, incoming(MARCOS, "U2", 2_000_000_001)).await;
    assert_eq!(unread(&h, MARCOS), 2);
    assert!(read_sent(&h, "U1").is_none(), "nothing read while away");
    let d = h.app.open.as_ref().unwrap().unread.clone().unwrap();
    assert_eq!(d.first.as_deref(), Some("U1"));
    assert_eq!(d.count, 2);
    // Back: the badge clears (also in the cache) and receipts go out.
    let t = h.app.update(super::Message::Focus(true));
    h.pump(t).await;
    assert_eq!(unread(&h, MARCOS), 0);
    assert!(read_sent(&h, "U1").is_some() && read_sent(&h, "U2").is_some());
    let row = h
        .app
        .store
        .chat_rows()
        .unwrap()
        .into_iter()
        .find(|r| r.meta.id == MARCOS)
        .unwrap();
    assert_eq!(row.meta.unread, 0);
}

#[tokio::test]
async fn scrolled_up_messages_stay_unread_until_the_bottom_is_reached() {
    let mut h = Harness::new().await;
    h.ui(Ui::OpenChat(MARCOS.into())).await;
    let _ = h.app.update(super::Message::Focus(true));
    h.ui(Ui::Scrolled {
        relative_y: 0.5,
        offset_y: 600.0,
    })
    .await;
    arrive(&mut h, incoming(MARCOS, "S1", 2_000_000_000)).await;
    assert_eq!(unread(&h, MARCOS), 1);
    assert!(read_sent(&h, "S1").is_none());
    h.ui(Ui::Scrolled {
        relative_y: 0.0,
        offset_y: 0.0,
    })
    .await;
    assert_eq!(unread(&h, MARCOS), 0);
    assert!(read_sent(&h, "S1").is_some());
}

#[tokio::test]
async fn a_history_chunk_does_not_mark_the_chat_being_read_unread() {
    let mut h = Harness::new().await;
    h.ui(Ui::OpenChat(MARCOS.into())).await;
    let _ = h.app.update(super::Message::Focus(true));
    let mut meta = h.app.chats.get(MARCOS).unwrap().meta.clone();
    meta.unread = 5;
    h.net(Event::History {
        chats: vec![meta.clone()],
        names: Vec::new(),
        progress: None,
    })
    .await;
    assert_eq!(unread(&h, MARCOS), 0);
    // Another chat keeps the phone's counter.
    let mut other = h.app.chats.get(PABLO).unwrap().meta.clone();
    other.unread = 4;
    h.net(Event::History {
        chats: vec![other],
        names: Vec::new(),
        progress: None,
    })
    .await;
    assert_eq!(unread(&h, PABLO), 4);
}

#[tokio::test]
async fn read_receipts_go_to_the_lid_a_message_came_from() {
    let mut h = Harness::new().await;
    h.net(Event::LidMappings(vec![(
        "555000111@lid".into(),
        MARCOS.into(),
    )]))
    .await;
    h.ui(Ui::OpenChat(MARCOS.into())).await;
    let _ = h.app.update(super::Message::Focus(true));
    arrive(&mut h, incoming("555000111@lid", "L1", 2_000_000_000)).await;
    // Shown in the phone-number chat, receipted to the LID it came from.
    assert!(h.app.open.as_ref().unwrap().get("L1").is_some());
    assert_eq!(read_sent(&h, "L1").as_deref(), Some("555000111@lid"));
    assert_eq!(unread(&h, MARCOS), 0);
}
