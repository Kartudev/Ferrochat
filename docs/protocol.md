# Protocol

Ferrochat is a **linked device** (companion) on the WhatsApp multi-device
protocol, the same role WhatsApp Web and Desktop play. The phone stays the
primary device; Ferrochat pairs by scanning a QR code.

> Ferrochat is unofficial. Third-party clients break WhatsApp's terms of
> service and can get the account banned.

## 1. Library decision: `whatsapp-rust` (pure Rust), no Go sidecar

The brief asks for a maintained pure-Rust implementation if one exists and
works, otherwise `whatsmeow` (Go) as a sidecar. Evaluation of
[`whatsapp-rust`](https://github.com/jlucaso1/whatsapp-rust) 0.7.0
(time-boxed, done before writing app code):

| Question | Finding |
|---|---|
| Maintained? | Yes. 10 releases from 0.1.0 (2025-10) to 0.7.0 (2026-08-07), ~186k downloads, MIT. Inspired by whatsmeow and Baileys, which it tracks closely. |
| Builds here? | Yes, on Rust 1.97 with `default-features = false` and `tokio-runtime, tokio-native, tokio-transport, ureq-client`. Its storage crate builds against SQLCipher (see §3). |
| QR pairing as a linked device | Yes: `Event::PairingQrCode { code, timeout }` rotates codes, `PairingQrCodesExhausted`, `PairSuccess`, `PairError`; `DevicePropsOverride` names the device ("Ferrochat") in the phone's Linked devices list. Pair-code login also exists (not used). |
| Send / receive text | Yes: `Client::send_message_with_options` (caller-chosen message id), `Event::Messages` batches with decrypted `wa::Message` + `MessageInfo`. |
| Media upload / download | Yes: `Client::upload` (encrypts, returns `direct_path`, `media_key`, hashes, streaming sidecar for audio) and `Client::download_from_params` (decrypts + verifies). Works for images, documents and audio/PTT. |
| Receipts | Yes: `Event::Receipt` (delivered / read / played / read-self), `Client::mark_as_read`, `Client::mark_as_played`. |
| History sync | Yes: `Event::HistorySync(LazyHistorySync)` with lazily inflated `wa::HistorySync` (conversations, messages, push names). |
| Logout | `Client::logout()` deregisters the companion; deleting the store wipes keys. |
| Storage | Pluggable `Backend`; the bundled SQLite store has a per-connection init hook documented for SQLCipher keying. |

**Decision: use `whatsapp-rust`.** It covers every core feature in the list
natively. A Go sidecar would add a second toolchain, a second process to
supervise, an IPC protocol to version and a larger memory footprint, for no
feature gain.

The library is wrapped behind the `net::WaClient` trait plus the
`net::Event` enum, so the app never sees library types and is driven by
`net::fake::FakeClient` in `--mock` mode and tests. All protobuf ↔ domain
conversion lives in pure functions in `src/net/convert.rs`.

## 2. Event mapping

| Library event | `net::Event` | App effect |
|---|---|---|
| `PairingQrCode` | `Qr { code, valid_for_secs }` | QR rendered with `qrcode` → RGBA image; `AuthState::ShowingQr { generation }` increments |
| `PairingQrCodesExhausted` | `QrExhausted` | "Click to reload QR code" (restarts the client) |
| `PairSuccess` / `PairError` | same | logged in / error under the QR |
| `Connected` | `Connection(Connected)` + `Connected { jid }` | banner cleared |
| `Disconnected`, `StreamError`, `ConnectFailure` 500/503 | `Connection(Reconnecting { attempt })` | banner; the library reconnects with backoff |
| `ConnectFailure` other 4xx | `Connection(Offline { reason })` | library stopped; app retries with its own backoff (1 s → 60 s + jitter) and shows "Retry now" |
| `ConnectFailure` 401/403/406, `LoggedOut` | `LoggedOut { reason }` | session store and key deleted, cache wiped, back to QR (the only automatic wipe) |
| `StreamReplaced` | `Connection(Replaced)` | **non-destructive**: library stopped, banner "WhatsApp is open on another computer" with "Use here" |
| `TemporaryBan`, `ClientOutdated` | `Fatal` | shown on the login screen |
| `Messages` | `LidMappings` + `Messages { origin: Live \| Offline }` | stored first; ids the store did not have count as unread (live and offline-drained alike); one notification per live message, one summary per offline drain |
| `Receipt` | `Receipt` or `ChatRead` (read-self) | tick state machine |
| `HistorySync` | `LidMappings` + `History` / `Messages { origin: History }` in chunks of 500 | chats, names, cached messages (with their reactions and quotes); never counted as unread |
| `Messages` with a reaction | `Reaction { chat, target, reactor, emoji, ts }` | one reaction per reactor, newest wins, empty emoji removes it |
| `Messages` with `protocolMessage{MESSAGE_EDIT}` | `Edited { chat, id, sender, from_me, content }` | applied only when `sender` sent the original; "Edited" marker |
| `Receipt` in a group | `Receipt { participant: Some(..) }` | stored per participant; the tick is the aggregate (see below) |
| `ChatPresence` | `ChatState { chat, participant, state }` | "typing…" / "recording audio…" in the header and the chat list; expires after 10 s |
| `Presence` | `Presence { jid, online, last_seen }` | "online" / "last seen …" under the contact name |
| `PictureUpdate` | `PictureChanged { jid }` | drops the cached picture and refetches |
| `DeleteMessageForMeUpdate` | `DeletedForMe { chat, id }` | removes the message from the cache |
| `PushNameUpdate` | `ContactName { source: Push }` | the name the contact chose (shown "~ Name") |
| `ContactUpdate` (app-state `contact`) | `ContactName { source: Saved }` for the phone and LID ids, `LidMappings` | the name saved in the phone's address book |
| `BusinessStatusUpdate`, `verified_name` on a message | `ContactName { source: Business }` | verified business name |
| `ArchiveUpdate`, `PinUpdate`, `MuteUpdate` (app-state `archive`, `pin_v1`, `mute`) | `ChatFlag` | archive / pin / mute applied, persisted |
| `LabelEditUpdate` (app-state `label_edit`) | `ListEdit` | list created / renamed / deleted |
| `LabelAssociationUpdate` (`label_jid`) | `ListChat` | chat added to / removed from a list |
| `MarkChatAsReadUpdate` | `ChatRead` (read) / `ChatMarkedUnread` | badge cleared / one unread |
| `ClearChatUpdate` (app-state `clearChat`) | `ChatCleared { keep_starred }` | messages removed (starred kept when asked), the chat stays |
| `DeleteChatUpdate` (app-state `deleteChat`) | `ChatDeleted` | chat and messages removed, closed if open |
| `IncomingCall`, `MissedCall`, `CallEndedElsewhere` | `Call(CallSignal)` | call panel, ringtone, call bubbles (§8) |

Reactions, edits and "delete for me" events wait until the message batch that
may contain their target is in the cache (`App::deferred`).

Message bodies handled: `conversation`, `extendedTextMessage`,
`imageMessage`, `documentMessage` (also wrapped in
`documentWithCaptionMessage`), `audioMessage` (PTT and audio files),
`protocolMessage{REVOKE}` (shown as "This message was deleted", applied only
when the revoker is the original sender: our own devices for our messages,
the same participant otherwise; group-admin revokes are ignored because they
cannot be verified locally). Ephemeral wrappers are unwrapped; disappearing
messages carry `expires_at` and are pruned from the cache when it passes.
**View-once** messages are rendered as "View once message: open it on your
phone": their media reference is never exposed, downloaded or stored.
`videoMessage` (and round video notes) show the inline JPEG thumbnail and the
duration; a click downloads the file (capped at 100 MB) into the cache
directory and plays it in the in-app viewer (§6.1). "Open externally"
(message menu, or the viewer) hands the file to the system player (`open`
crate, extension from an allowlist). `extendedTextMessage` with a `title`
and an http(s) `matchedText` becomes a text message with the sender's link
preview (title, description, inline thumbnail); previews are never fetched
on our side, like the official clients. `stickerMessage` is downloaded (cap 2 MB, 1 Mpx), decoded to RGBA
and drawn statically; animated stickers show their first frame. Locations,
contacts and polls show an "unsupported" placeholder; key distribution
messages are not bubbles. Replies come from `contextInfo` (`stanzaId`,
`participant`, `quotedMessage`) and are rendered as a quote above the text; a
click jumps to the original, loading the cache around it when needed.

### 2.1 What we send

| Action | Library call | Notes |
|---|---|---|
| Reply | `send_message_with_options` with `ExtendedTextMessage.contextInfo` | the quoted body is the one-line preview, so media quotes arrive as text on the other side |
| React / remove | `Client::send_reaction` (empty emoji removes) | groups carry the original author as `participant` |
| Edit | `Client::edit_message` | only text, 15 minutes after sending |
| Delete for everyone | `Client::revoke_message(Sender)` | 2 days 12 hours after sending |
| Delete for me | `chat_actions().delete_message_for_me` | app-state sync to our other devices |
| Typing / recording | `chatstate().send_composing / send_recording / send_paused` | `composing` when typing starts, again every 10 s while it continues, `paused` after 5 s idle, on send, when the field empties and when the chat is closed |
| Presence | `presence().subscribe` when a DM is opened | the library announces us as available itself |
| Profile pictures | `contacts().get_profile_picture(preview)` + one GET through the library's HTTP client | max 3 lookups at once, only for rows on screen |
| Group size | `groups().query_info` (cached by the library) | for the aggregate ticks |
| Contact info drawer | `contacts().get_user_info` (usync: status = "About", verified business name) | only when the drawer opens; privacy settings may hide the About text |
| Group info drawer | `groups().get_metadata` (description, participants with admin type) | when the drawer opens |
| Groups in common | `groups().get_participating`, filtered by the contact's PN / LID | when the drawer opens |
| Block / unblock | `blocking().block / unblock`, `blocking().is_blocked` | after a confirmation in the drawer |
| Mute / unmute | `chat_actions().mute_chat_until` (8 hours, 1 week) / `mute_chat` (always) / `unmute_chat` | optimistic, reverted on failure; the end is kept (`chats.mute_end`, schema 7, Unix seconds, `-1` = always) and a timed mute ends on its own. Sync actions carry the end in milliseconds |
| Mark as read / unread | `chat_actions().mark_chat_as_read` with the newest message as `messageRange` | optimistic, reverted on failure; reading a chat that was marked unread by hand (here or on the phone, since start-up) also sends `read = true` |
| Clear chat | `chat_actions().clear_chat(deleteStarred = !keep, deleteMedia = false)` | asks first ("Keep starred messages" ticked by default); sent first, applied here once the phone has it |
| Delete chat | `chat_actions().delete_chat(deleteMedia = false)` | asks first; sent first, applied once the phone has it; list memberships are left to the phone |
| Send video / GIF | `Client::upload(MediaType::Video)` + `VideoMessage` (`seconds`, `width`, `height`, `jpegThumbnail`, `gifPlayback` for GIFs) | only `.mp4` / `.m4v` go as videos: the `ferrochat-player` helper reads the size, duration and first frame (muted, then stopped) before the preview opens; a file it can't read goes as a document with a note. A `.gif` is converted to a silent H.264 Baseline MP4 by the helper (`--gif-to-mp4`: frames composited by `image`, own delays kept, transparency on white, ≤ 640 px with even sides, ≤ 60 s / 1500 frames) and sent with `gifPlayback`, like WhatsApp. Received `gifPlayback` videos show "GIF" instead of a duration and loop in the viewer (`Content::Video::gif`, serde default, no schema change). Other containers (`.mov`, `.webm`, `.mkv`) stay documents |
| Re-upload expired media | `media_reupload().request(MediaReuploadRequest)` (the "media retry" receipt; `participant` only for others' group messages) | a download answered 404 / 410 by the CDN (`Error::MediaExpired`, typical for old history) asks the sender's phone to upload the blob again, points the cached message at the new `directPath` (`Store::set_media_ref`) and tries once more; `NotFound` / errors leave the placeholder with "no longer available". Applies to photos, videos, voice notes, documents and posters, not to statuses |
| Forward | `send_message_with_options` with the body rebuilt from the cached content and passed through `MessageExt::prepare_for_forward` (`convert::build_forward`) | up to 5 chats (1 when "forwarded many times"); each copy is a new message with our own id, shown and cached at once and sent in order. Media is relayed from the same CDN blob (`directPath`, `mediaKey`, hashes; `url` built on `mmg.whatsapp.net`), never re-uploaded by us; media older than 14 days (`forward::STALE_MEDIA_SECS`) is first refreshed with one re-upload request per source (best effort: on failure the old blob is relayed). Quote, mentions and `messageSecret` are dropped like the official client. Not offered for deleted, view-once, unsupported, call or media without a key, nor for our own pending / failed messages |

Message info: the menu of our own messages opens a "Message info" panel
(`app/msg_info.rs`, `ui/msg_info.rs`) with sent / delivered / read / played
times, and in groups who has read it and who has only received it. Times
come from the receipts seen arriving (`receipt_times`, schema 9: one row per
message, participant and status, first time kept; only our messages). Older
messages, and receipts that arrived while Ferrochat was closed, show the
status without a time ("Time not recorded").

Forwarded mark: `contextInfo.isForwarded` / `forwardingScore` is kept per
message (cache column `forwarded`, schema 8: the score, NULL = not
forwarded) and shown as "Forwarded", or "Forwarded many times" with a double
arrow from score 5 (the library jumps to the 127 sentinel there). A forward
of a forward carries the source score plus one.

Mentions: `contextInfo.mentionedJid` is kept per message (cache column
`mentions`, schema 6). An "@<digits>" in the text is shown as the name of
the mentioned JID: LID -> phone number through the learned pairs, then
saved > business > push name, then the formatted number; an unnamed LID
without a known number reads "@Unknown", never digits
(`ChatList::mention_label`).

The 10 s / 5 s typing timings and the 15 min / 2.5 day windows follow what
WhatsApp Web does as far as we know; they are constants in `app/live.rs` and
`net/types.rs` and have not been checked against a live account.

### 2.2 Contact names

The user sees the name saved in the phone's address book, as WhatsApp
Desktop does. The phone is the only source of those names; a linked device
receives them through **app-state sync**: `contact` mutations (index
`["contact", jid]`, value `ContactAction { fullName, firstName, lidJid,
pnJid }`) in the `critical_unblock_low` collection. `whatsapp-rust` decodes
them into `Event::ContactUpdate`; Ferrochat stores them by source in the
`contact_names (jid, source, name)` table (`Push` 1, `Business` 2, `Saved` 3)
and shows, in this order:

1. saved name (full name, else first name),
2. business verified name,
3. push name, marked `~ Name` (people who are not contacts),
4. a name the chat record itself carries (history sync),
5. the number, grouped (`+34 600 111 222`).

Groups show their subject; the same order applies to chat list rows, the
conversation header, group sender names, quotes, reaction lists and
`@mentions` in text (a mention shows the person's name instead of the number).
A push name carried by a message never replaces a saved name (that was the
bug: all sources shared one map and the last writer won). A `ContactAction`
carries both the phone-number and the LID id: the name is stored under both
and the pair is learned.

**Existing sessions.** A session linked before this was implemented already
holds the collections at their latest version, so the contact mutations are
never replayed. Asking for a snapshot is not enough: wacore 0.7 discards a
snapshot whose version is not newer than the persisted one
(`appstate_sync::snapshot_is_stale`, logged as "Skipping stale snapshot"
under the `AppState` target), which is why v0.8.0's one-time resync applied
nothing. Ferrochat therefore resets the collection first (persisted version
to 0 and its mutation MACs cleared: the state of a freshly linked device)
and then calls `Client::process_sync_task(MajorSyncTask::AppStateSync {
full_sync: true })` for `critical_unblock_low`, `regular_low`, `regular` and
`regular_high`; the snapshot then becomes the new baseline and every
mutation is replayed as an event. If a request fails half-way, the version
stays 0 and the next sync of that collection asks for a snapshot by itself.
Missing app-state keys are requested from the phone by the library
(`AppStateSyncKeyRequest`, 10 s wait); the phone must be online.

The resync reports per collection whether the snapshot was applied and how
many `contact` entries arrived. The `appstate_resync` marker (value `3`;
v0.8.0's `1` and the pre-fix development `2` do not count; the same resync
also replays starred messages) is written only once snapshots were applied and
the address book arrived (an account whose snapshot applies with no contact
at all counts as done after three attempts). Otherwise it is retried after
45 s, up to four times per run, and again at the next start.

**LID vs. phone number.** Contacts may be keyed by LID (`…@lid`) while the
chat uses the phone number, or the reverse. On every connection the LID ↔
PN pairs the library already knows (`get_all_lid_mappings`: usync, device
lists, earlier history syncs) are imported, and the name book looks a name
up under the other id of the same person, so a saved name stored under
either id wins over a push name under either. History conversations'
`displayName` is used as a chat-record name when `name` is empty.

**Diagnostics** (info level, counts only, never names, numbers or ids):
`app-state resync: <collection> version A -> B (snapshot applied | not
applied)`, `app-state resync attempt N: X/Y collections applied, K contact
entries, sync key stored before: …`, and `names after app-state resync:
saved=… business=… push=… lid_pairs=…; direct chats … (saved …, push …,
number only …)`. The library's own app-state logs use the targets
`Client/AppState` and `AppState` (not `whatsapp_rust`), e.g.
`RUST_LOG=info,ferrochat=debug,Client/AppState=debug,AppState=debug`.

Known limits: a contact *removed* from the address book arrives as a syncd
`REMOVE`, which `whatsapp-rust` 0.7 does not turn into an event, so the old
saved name stays until the next full resync (clearing the cache). Names of
contacts the phone has not synced (not in the address book) stay push names.

### 2.3 Archive, pins and lists

| Feature | Receive | Send |
|---|---|---|
| Archive | `archive` mutation → `ArchiveUpdate` → `ChatFlag::Archived` | `chat_actions().archive_chat` / `unarchive_chat` |
| Pin (max 3) | `pin_v1` → `PinUpdate` | `chat_actions().pin_chat` / `unpin_chat` |
| Custom lists | `label_edit` with `ListType::CUSTOM` (name, colour, order, deleted) → `ListEdit`; `label_jid` → `ListChat` | `send_app_state_action(LABEL_EDIT, ...)` with `type = CUSTOM`; `labels().delete_label`, `add_chat_label`, `remove_chat_label` |
| Favourites | a synced list of type `FAVORITES` (members through `label_jid`) | membership through `label_jid` when that list exists |

Changes are applied locally first, persisted (`chats.archived/pinned`,
`lists`, `list_chats`) and sent; a failed send is reverted with a toast. The
"Archived" row opens the archived list (archived chats stay out of the main
list; searching finds them, as in the official app). Lists appear as chips
after Favourites and Groups; a right click on a chip renames or deletes the
list, a right click on a chat adds it to or removes it from lists.

**Not verifiable without a phone, and the limits.** `whatsapp-rust` 0.7
exposes labels but has no helper for list *types*; Ferrochat writes the
`LabelEditAction` itself (`type = CUSTOM`), which matches the proto
(`LabelEditAction.ListType`) but has not been checked against a real
account. The library emits no event for the `favorites` action
(`FavoritesAction`), so favourites chosen on the phone through that action
are not visible; when no `FAVORITES` list has been synced, Ferrochat keeps
favourites on this device only (`ferrochat:favourites` in `list_chats`) and
never sends the `favorites` action, because it would overwrite the phone's
list with an incomplete one.

### 2.4 Group ticks

WhatsApp sends one receipt per participant. Ferrochat stores them
(`receipts` table) and shows two grey ticks once every other participant has
received the message and blue ticks once every other participant has read it.
The participant count (everyone but us) comes from the library's group
metadata; until it is known no group message advances past one tick. Direct
chats are unchanged.

## 3. What lands on disk

| Path | Content | Protection |
|---|---|---|
| OS keyring, service `dev.ferro.ferrochat`, account `signal-store-key` | 256-bit random key (hex) for the session store | Secret Service (KWallet / GNOME Keyring), Keychain, Credential Manager |
| `$XDG_DATA_HOME/ferrochat/session.sqlcipher` (+ `-wal`, `-shm`) | The library's whole Signal / device state: identity key pair, noise key, signed pre-keys, sessions, sender keys, app-state sync keys, LID mappings | **SQLCipher** (AES-256, HMAC per page) keyed with the raw key above (`PRAGMA key = "x'…'"`, no PBKDF2). File mode 0600. Tested: no `SQLite format 3` header and no schema text in the file; a wrong key is rejected. |
| `$XDG_DATA_HOME/ferrochat/cache.sqlite3` (+ `-wal`, `-shm`) | Cache: chats, message bodies, media references (CDN path + per-file media key, needed to download later), contact names (by source), chat lists and their members, own JID | Plain SQLite, mode 0600, like the official desktop client's local database. Deletable at any time; rebuilt from history sync. Never contains credentials or Signal keys. |
| (cache tables `statuses`, `starred`) | Statuses of the last 24 h (author, text / media references, thumbnail, viewed flag) and starred message keys | Same file as above; wiped on logout |
| `$XDG_CACHE_HOME/ferrochat/avatars/*.img` / `*.none` | Profile picture previews (JPEG as served) and "no picture" markers. File names are a hash of the JID. Refreshed after 7 days / 12 h or when the contact changes the picture; at most 1500 files and 32 MB; deleted on logout | Directory 0700 |
| `$XDG_CACHE_HOME/ferrochat/videos/*` | The last four videos you opened (or 400 MB), decrypted, so the player (in-app helper or system player) can read them | Directory 0700. `--mock` writes its 40 kB clip to `$TMPDIR/ferrochat-mock-<pid>` and removes it when the viewer closes. |
| `$XDG_CONFIG_HOME/ferrochat/settings.json` | Preferences: close to tray, start minimized, mute notifications | Plain JSON, no secrets; not written in `--mock` |
| `~/.config/autostart/dev.ferro.ferrochat.desktop` | "Start at login" entry (`Exec=<binary> --minimized`) | Only when the user turns the toggle on; the toggle removes it |
| `$XDG_DATA_HOME/ferrochat/ferrochat.sock` | Single-instance hand-off: accepts only the line `show` | Unix socket in the 0700 data directory, mode 0600, removed on exit |
| `$XDG_STATE_HOME/ferrochat/log` (rotated at 5 MB) | Logs | Bodies, phone numbers, QR payloads, keys and file names are redacted (`Redacted`, `MaskedJid`, manual `Debug` impls; covered by `tests/redaction.rs`). |
| `~/Downloads/<name>` | Documents the user opened; photos saved or opened externally from the image viewer | Only on explicit click. |
| (cache column `messages.thumb` of videos) | A poster frame (JPEG, <= 480 px) decoded by the player helper for videos (<= 16 MB) that arrived without an inline thumbnail; the video itself lands in the `videos/` cache above | Same file as the cache |
| (cache key `recent_emoji`) | The emoji picker's recent emoji | Same file as the cache |

The data directory is created with mode 0700 and every database file
(including `-wal` / `-shm`) is set to 0600. Downloads are capped at 100 MB
and images are probed (header only) and rejected above 40 Mpx before
decoding. Notification titles and bodies are markup-escaped (`&`, `<`, `>`)
because KDE renders a subset of HTML in them.

Decoded profile pictures live in a 12 MB LRU (96x96 tiles, round). Images and voice notes are kept in memory only (byte-bounded LRUs: 48 MB
images, 16 MB decoded audio) and re-downloaded after a restart.

The session store is **never deleted automatically** except on a real
logout (user action or server-side unlink). Only `SQLITE_NOTADB` (wrong key)
or a missing/malformed keyring entry next to an existing session counts as
"unreadable": the login screen then explains it and offers "Discard saved
session and link again"; no new key is minted over an existing session. Any
other error (I/O, permissions, busy database, migration) is shown as an
error with "Try again" and nothing is touched. If no Secret Service is
reachable the login screen explains how to enable KWallet / GNOME Keyring
and "Try again" retries without restarting; there is no file fallback (an
opt-in passphrase-encrypted fallback is future work).

A single-instance lock (`ferrochat.lock` in the data directory, OS file
lock) prevents two Ferrochat processes from sharing one session store. A
second launch connects to `ferrochat.sock`, asks the first instance to show
and focus its window, and exits with status 0 (`--minimized` launches, e.g.
autostart, exit silently).

The SQLCipher key only lives in zeroized buffers (`secrecy`, `Zeroizing`).
Media limits: downloads ≤ 100 MB, at most 4 concurrent; images are
header-probed and refused above 16 Mpx when auto-downloaded and 40 Mpx when
opened; voice notes refuse to decode beyond their declared duration
(+50 % + 10 s) or 10 minutes, and beyond 90 000 packets (decompression
bombs). Recording stops at 30 minutes. Documents with executable or active
content types (`.exe`, `.bat`, `.ps1`, `.lnk`, `.jar`, `.msi`, `.desktop`,
`.sh`, `.AppImage`, `.html`, `.svg`, …) are saved but never launched; file
names are sanitised (separators, control characters, trailing dots/spaces,
reserved Windows names). Files are opened without a shell (`open` crate:
`xdg-open` / `ShellExecuteW`).

`--mock` mode uses an in-memory secret store and an in-memory cache and
writes nothing except an optional screenshot.

## 4. Sending

1. The app generates a WhatsApp-Web-shaped id (`3EB0` + 18 hex) and inserts
   an optimistic `Pending` bubble (clock icon).
2. `send_text` / `send_media` (upload then send) runs as a `Task`.
3. Success → `Sent` (one grey tick); error → `Failed` (red icon, click to
   retry text).
4. Receipts move it to `Delivered` (two grey ticks), `Read` (two blue
   ticks), `Played` (voice notes).

The receipt state machine (`ReceiptStatus::advance`) only moves forward
(`Pending → Sent → Delivered → Read → Played`), tolerates out-of-order
receipts, only allows `Failed` from `Pending`, and lets a later server
confirmation override `Failed`. It is applied identically in memory and in
the store, so a late history-sync copy can never downgrade ticks.

Outgoing photos get a 96 px JPEG thumbnail and real dimensions
(`image` crate); other files go as documents with their MIME type.
Voice notes are recorded with `cpal`, down-mixed/resampled to 48 kHz mono,
encoded with libopus (24 kbit/s VoIP mode) into Ogg with a correct
`OpusHead`/`OpusTags`, granule positions and pre-skip, plus a 64-bucket
waveform.

## 5. Read receipts

When a chat is opened (or the window regains focus with a chat open), the
last *unread* incoming messages are marked read with `mark_as_read`,
batched per participant in groups (the protocol needs the sender there).
Playing an incoming voice note sends a `played` receipt. Live messages in
the focused open chat are marked read immediately.

## 5b. Status, communities, groups, stars

* **Status** (`status@broadcast`). Statuses are never a chat. In `translate`
  every message whose chat is the status broadcast is diverted to
  `Event::Status` (text with `background_argb` + font, photo, video) or
  `Event::StatusRevoked`; history sync already skips `@broadcast`. They are
  kept in the cache table `statuses` (24 h from the send time, at most 1500
  rows, thumbnails capped at 64 kB) and the legacy `status@broadcast` chat
  rows of v0.8 caches are deleted by migration 5. Viewing a status sends a
  `<receipt type="read" context="status">` through `Client::mark_as_read`
  (participant = author). The library's own privacy gate only covers private
  chats, so `RealClient::send_status_view` first reads the account's
  `readreceipts` privacy setting (`fetch_privacy_settings`, cached 10 min) and
  sends nothing when it is `none` or cannot be read. Posting a text status
  uses `Client::status().send_text` to the saved contacts.
* **Communities.** `community().get_participating()` (parents),
  `community().get_subgroups()` (MEX, includes groups we are not in) and
  `groups().get_participating()` (every group we are in, with
  `parent_group_jid` / `is_default_sub_group` / `is_general_chat`) are merged;
  subgroups we are in are derived from the parent link when the MEX query
  fails. Creating a community uses `community().create`.
* **New chat.** Contacts are the address-book names (app-state `contact`).
  A typed number is checked with `contacts().is_on_whatsapp` (usync) only
  when the user clicks "Message +...", never while typing.
* **New group.** `groups().create_group` with the picked phone JIDs, then
  `set_profile_picture` (640 px square JPEG) when an icon was chosen.
* **Starred messages.** `chat_actions().star_message / unstar_message`
  (app-state `star`) and `Event::StarUpdate` into the `starred` table.
  Existing sessions get one more full app-state snapshot (marker
  `appstate_resync` = 2) because stars were never replayed before. A star
  whose message is not in the local cache is kept but not listed.

## 6. Voice note playback

Ogg demux with the pure-Rust `ogg` crate, Opus decode with `opus` (libopus
bindings; `symphonia` has no Opus decoder), output through `rodio` on a
dedicated audio thread. The UI polls position every 100 ms while playing and
draws the waveform bars as played / unplayed.

### 6.1 Video playback

Videos play inside the app with no system codec package. Decoding runs in a
helper process, `ferrochat-player`, one process per open video: MP4 demux
with the `mp4` crate (pure Rust; sample table, `avcC`, display matrix),
H.264 with OpenH264 (Cisco's BSD-licensed decoder built from source and
linked statically; Main and High profile with B-frames, decoder reorder
buffer kept, no per-sample flush), AAC with symphonia (pure Rust), sound
through rodio, YUV → RGBA with scaling and rotation in one pass. The release
links libstdc++ statically; CI checks with `ldd` that the helper needs only
glibc, libgcc_s and libasound. H.265 (some iPhone videos), VP9 and other
codecs are reported by name ("This video uses H.265 (HEVC), which the
built-in player cannot decode") with "Open externally"; every failure names
its step (helper start, container, codec, decode position).

* The helper writes tagged binary messages to stdout (ready, RGBA frames of at
  most 960x540, position / state every 250 ms, failure) and reads text
  commands from stdin (`play`, `pause`, `seek <ms>`, `volume`, `mute`,
  `quit`); the format is `src/media/video_proto.rs`, with tests for hostile
  input (size caps, unknown tags).
* The app keeps the newest frame as an `image::Handle` created when the frame
  arrives, never in `view`; stale frames are dropped when the UI is behind.
  While a video plays, only the viewer is built, so frames do not rebuild the
  chat list and conversation.
* Closing the viewer drops the `Player`, which ends the process: the decoder,
  its buffers and the audio stream are freed. At most one exists at a time.
* The main binary links no codec. The system player is only ever used
  through the explicit "Open externally" button or the message menu entry.
* Controls: play / pause (also click on the picture and Space), seek bar with
  elapsed and total time, volume and mute, full screen, close (Esc).

GStreamer (v0.8.0) was replaced because it depends on system plugins the
user may not have. A static minimal ffmpeg was the alternative: more
codecs (H.265), but LGPL obligations for static linking and a C build of
ffmpeg in the release; OpenH264 + symphonia cover what WhatsApp sends
(phones transcode to H.264 + AAC). Patent note: Cisco's patent licence only
covers its prebuilt binaries, like any other software H.264 decoder built
from source.

`iced_video_player` 0.6 (GStreamer, iced 0.14) was evaluated and rejected: it
renders through a wgpu shader only (nothing is drawn on the tiny-skia
fallback), links GStreamer into the main binary (no graceful degradation), and
decodes in-process (memory cannot be given back by closing the viewer).

## 7. Known protocol gaps

* LID vs. phone-number addressing: LID ↔ PN pairs are learned from
  envelopes (`sender_alt` / `recipient_alt`), history sync
  (`phoneNumberToLidMappings`, conversation `pnJid`/`lidJid`) and persisted
  (`lid_map` table); messages, receipts, history chats and names are
  normalised to the PN JID. A LID seen before any mapping is learned still
  forms its own chat until the pair arrives (existing rows are not merged
  retroactively).
* Videos that arrive without `jpegThumbnail` (seen on history-synced media;
  the library's default `HistorySyncConfig` does not ask for
  `thumbnailSyncDaysLimit`) get a poster frame from the helper when they are
  at most 16 MB; bigger ones keep the placeholder until played.
* The info drawer has no "disappearing messages", encryption code,
  "report" or "exit group" actions, and media visibility / per-chat
  wallpaper are not offered.
* "Mark as read" from the chat menu syncs the read state to the other
  devices but sends no read receipts to the sender.
* Windows and macOS have no tray yet (stub): closing the window quits.
* On Wayland the compositor may refuse to focus a window that is shown from
  the tray or a notification (xdg-activation); it then appears without focus.

## 8. Calls

### 8.1 Feasibility (November 2026)

| Question | Finding |
|---|---|
| Call signalling in `whatsapp-rust` 0.7.0? | Yes. `handlers/call.rs` parses every `<call>` child (`offer`, `offer_notice`, `preaccept`, `accept`, `reject`, `terminate`, `transport`, `relaylatency`, `video`, group roster/rekey actions) with `wacore::stanza::call::parse_call_stanza`, sends the generic `<ack>` and the `<receipt><offer/>` the caller waits for, and dispatches `Event::IncomingCall(IncomingCall)`. Offers replayed from the offline queue surface only as `Event::MissedCall { reason: Offline }`. |
| Decline the way the official client does? | Yes: `Client::voip().reject(&offer)` builds the same `<call><reject call-id call-creator/></call>` as WhatsApp Web (`wacore::stanza::call::build_reject`) and clears the ringing state. Always available, no feature needed. |
| Media (call key, relay, SRTP)? | **Yes, in the pinned crate.** The `voip` feature (`voip-mlow` + `voip-libopus`) enables `whatsapp_rust::voip`: `voip().accept(&offer).audio(mic, speaker).start()` sends `<preaccept>`, decrypts the Signal-encrypted call key, sends `<accept>`, connects to the WhatsApp relay over DTLS/SCTP (`webrtc-dtls`, `webrtc-sctp`) and runs the media engine (RTP, SRTP/WARP key derivation, MLOW / Opus at 16 kHz, 60 ms frames). `voip().call(&peer)` generates the call key, encrypts it to each of the peer's devices and sends the `<offer>`; the relay arrives in the server's offer ack. `voip().terminate(..)` hangs up. With the feature on, an unanswered ringing call also yields `Event::MissedCall { reason: Remote }` and another of our devices answering / declining yields `Event::CallEndedElsewhere`. |
| Verified against real phones? | Upstream says native Opus (PT 120) was "verified against Android/Web implementations and with a live full-duplex Android call"; MLOW is the compatibility default (`agent_docs/voip_audio_codecs.md` in the crate). Ferrochat itself has **not** been tested with a real account yet. |
| Video? | The library transports pre-encoded H.264 (Constrained Baseline, Annex-B access units, `CallHandle::start_video` / `accept_video`); camera capture, an H.264 encoder and a decoder are the application's job. Not implemented (§8.4). |
| Group calls? | The library has group / call-link facades; not wired in Ferrochat (ring + Decline only). |
| Upstream after 0.7.0 | `main` (2026-11-24) keeps improving VoIP (MLOW low-rate decoding, relay candidate fixes, group keyframe requests) but is unreleased and breaking (chrono removed, relay dialler split into `voip-relay-native`); Ferrochat stays on the crates.io release. |
| Other open-source stacks | Baileys (TypeScript) and whatsmeow (Go) only parse call offers / terminations and reject calls; neither carries media. No reverse engineering or WhatsApp binaries were used here: everything goes through the library's public API. |

**Verdict:** voice calls are feasible with the pinned library and are
implemented; video needs camera capture plus an H.264 encoder/decoder and is
not.

### 8.2 Event mapping

| Library event | `net::CallSignal` | App effect |
|---|---|---|
| `IncomingCall` `offer` / `offer_notice` | `Ringing { call_id, caller (caller_pn, else the sender), group, video }` | call panel "Incoming voice call", ringtone, notification, window shown |
| `IncomingCall` `accept` | `PeerAccepted` | outgoing call: "Connecting…" |
| `IncomingCall` `reject` | `PeerRejected { busy }` | outgoing call ends ("Declined"); `busy` (one device) is ignored |
| `IncomingCall` `terminate` (not `*_elsewhere`) | `PeerEnded` | ringing → missed call; talking → ended with duration |
| `MissedCall` | `Missed` | missed-call bubble (once per call id) |
| `CallEndedElsewhere` | `Elsewhere { accepted }` | ringing stops; "Answered on another device" / declined bubble |
| `CallEvent::RelayAllocated` / accept finished | `Connected` | timer starts |
| `CallHandle::wait_ended` | `MediaEnded { error }` | call ends; relay failures toast |

Transport, relay latency and video-state stanzas are handled inside the
library. The pure state machine is `call::Calls` (`src/call/mod.rs`); the
app glue is `app/call.rs`, the library glue `net/call.rs`.

### 8.3 What we send

| Action | Library call |
|---|---|
| Decline | `voip().reject(&offer)` |
| Answer (1:1 voice, or a video offer answered as voice) | `voip().accept(&offer).audio(..).start()` |
| Call (header phone button, or a click on a call bubble) | `voip().call(&peer).audio(..).start()` |
| Hang up / cancel / 60 s without answer | `voip().terminate(call_id, peer, creator)` then `CallHandle::hangup` |
| Mute | `CallHandle::set_muted` (the engine sends comfort noise) |

Audio: `cpal` (through rodio) on its own threads, down-mixed and linearly
resampled to 16 kHz mono, cut into 960-sample frames; received frames are
resampled to the device rate with a 400 ms cap. The ringtone and ringback
are synthesized (no recorded tones shipped). `--mock` and tests open no
device. No echo cancellation: use headphones.

Call bubbles are local messages `call-<call id>` (`Content::Call(CallLog)`)
written through the normal message path: a missed incoming call is unread
and notifies, the rest is history. History sync adds `callLogMesssage`
messages and the `CALL_MISSED_*` stubs.

### 8.4 Not implemented

* **Video calls.** The video button explains it (toast) instead of
  pretending; an incoming video call can be answered as voice only.
  Needed: camera capture (PipeWire camera portal / V4L2), an H.264
  Constrained Baseline encoder and a decoder (OpenH264 is only allowed in
  the `ferrochat-player` helper, so this means a capture/encode helper
  process), frame transport to the UI as `image::Handle`s, and the
  in-call video layout. Estimate: 1–2 weeks plus testing with real phones.
* **Group calls**: ring and Decline only ("answer on your phone").
* **Second incoming call while busy**: not shown; logged as missed.
* **Device pickers, echo cancellation, network-quality indicator.**

## UI reference

The palette, metrics and screen map that reproduce the official look are in
`docs/architecture.md` §11.
