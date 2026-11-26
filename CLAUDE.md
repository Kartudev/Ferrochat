# Ferrochat

## Overview
Native Rust WhatsApp desktop client (linked device). `iced` 0.14 UI that mimics
WhatsApp Desktop dark theme; protocol via pure-Rust `whatsapp-rust` 0.7 behind
the `net::WaClient` trait. Unofficial: third-party clients can get accounts banned.

## Commands
- `cargo build` / `cargo build --release` (use `CARGO_BUILD_JOBS=2` on shared hosts)
- `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`
- `cargo run -- --mock` (fake protocol + mock data, no network, no disk writes)
- Screenshots: `ICED_BACKEND=tiny-skia xvfb-run -a -s "-screen 0 1280x800x24" ./target/debug/ferrochat --screenshot <login|chats|chat|chat-menu|mute|reply|reactions|chat-search|list-search|delete|select|forward|settings|paste|attach-video|attach-gif|drop|message-info|group-message-info|update-banner|update-settings|emoji|viewer|info|group-info> docs/screenshots/<name>.png`
- Tray test needs a session bus: `dbus-run-session -- cargo test` (the SNI test is skipped without one).
- Regenerate fixtures: `cargo test --test gen_fixtures -- --ignored`

- Video: `target/debug/ferrochat-player` must exist next to `ferrochat`
  (`cargo build` builds both; OpenH264 compiles from source, needs a C++
  compiler). No system codec packages. `--no-default-features` builds
  without the helper. Release links libstdc++ statically
  (`CXXSTDLIB=static=stdc++`); `.github/scripts/check-helper-libs.sh` checks it.

## Architecture
See `docs/architecture.md` (shared Ferro* skeleton + §11 app specifics) and
`docs/protocol.md` (library decision, event mapping, what lands on disk).
`ui -> app -> {net, store, auth, media}`; `net`/`store`/`auth` never import iced widgets.

## Conventions
- English everywhere; Conventional Commits; small commits.
- No `unwrap`/`expect` outside tests (crate lints deny them).
- Only `auth/secrets.rs` touches `keyring`. Never log bodies, JIDs, keys, QR codes:
  use `Redacted`, `MaskedJid`, redacting `Debug` impls.
- Pure logic goes in `app/state.rs`, `app/live.rs`, `net/convert.rs`, `net/types.rs`
  with unit tests; user actions live in `app/actions.rs`.
- Every new crate needs a one-line justification in the PR.

## Workflow quirks
- Work on `feat/*` branches, PR to `development`. Never push `main`/`development`.
- `version.yml` bumps `Cargo.toml` (PATCH on development, MINOR on dev→main merge);
  dev→main merges are detected via the commits/{sha}/pulls API; runs by
  github-actions[bot] are skipped. Protected branches need a bot bypass.

## Deployment
Releases are automatic: `.github/workflows/release.yml` chains off the "Version bump" workflow
via `workflow_run` (GITHUB_TOKEN bot pushes don't trigger `push` workflows), builds on
ubuntu-22.04, and creates GitHub Release `v<Cargo version>` with `ferrochat-v<ver>-linux-x86_64.tar.gz`
+ `.sha256` (skips if the release exists; manual re-run: workflow_dispatch). Users run
`install.sh` (`curl ... | sh`; `-s -- --uninstall`); test it with `FERRO_RELEASE_API=file://...`.
Wayland `app_id` / desktop entry id: `dev.ferro.ferrochat` (`packaging/`).

## Windows
- CI job `windows` (windows-latest, MSVC): clippy, full tests, `--screenshot`, and the zip
  (`.github/scripts/package-windows.ps1`) installed/removed by `install.ps1`. Needs
  `OPENSSL_SRC_PERL=C:\Strawberry\perl\bin\perl.exe` (SQLCipher's OpenSSL is vendored on `cfg(windows)`).
- Release asset `ferrochat-v<ver>-windows-x86_64.zip` (+ `.sha256`), vcruntime140.dll bundled app-local.
  A folder with `installed-version` next to `ferrochat.exe` self-updates (`ScriptLayout::windows`):
  running `.exe`s are renamed to `.<name>.exe.old.<pid>` (`install::replace_moving`), never overwritten;
  `clean_leftovers` deletes them on the next start. Sync staged files through a WRITE handle.
- Release builds use `windows_subsystem = "windows"`; children (helper) get `CREATE_NO_WINDOW`.
- Autostart = HKCU Run value `Ferrochat` (`autostart::windows`); single instance = loopback TCP port in
  `<data_dir>/ferrochat.port`.
- Tray (`tray/windows.rs`): raw `Shell_NotifyIconW` on its own thread with a hidden window + message
  loop; model updates are posted (`WM_REFRESH`), re-added on `TaskbarCreated`, removed by
  `tray::shutdown()` at quit. Exe icon: `packaging/ferrochat.ico` (generated, `icon::tests`) embedded
  by `build.rs` (embed-resource) only for Windows targets.

## Gotchas
- Only ONE `libsqlite3-sys` may link: it is pinned to 0.37 with `bundled-sqlcipher`;
  `rusqlite` must stay on 0.39 and `whatsapp-rust` default features stay OFF.
- SQLCipher needs OpenSSL (`libssl-dev`); libopus is built from bundled source and
  linked statically (`.cargo/config.toml`, needs `cmake`).
- Never delete the session store except on a real logout; see `docs/protocol.md` §3.
- `scrollable().anchor_bottom()` reverses offsets: "bottom" is `RelativeOffset::START`.
- The default `tiny-skia` renderer ignores image corner radii: round avatars are
  masked into the RGBA tile (`media::avatar::circle_mask`).
- Message popups (reactions / menu) float in a stack layer placed from the row's
  on-screen bounds (`actions::Locate`); a scroll closes them. Jumping to a message
  uses the same operation plus `scroll_to` (offsets count from the newest end).
- Reactions/edits wait for pending message inserts (`App::deferred`), otherwise
  they race the cache write and are lost.
- Group ticks: per-participant receipts are stored; blue only when all read.
- iced style fns returning `impl Fn` need `+ use<>` (edition 2024 captures).
- The app is an `iced::daemon`: the main window is opened by `App::boot` / `background.rs`,
  hiding to the tray CLOSES it (frees the renderer). Never leave the process running with no
  window and no tray (`app/window.rs` invariant). `ksni` must stay on zbus 5 (shared with ashpd).
- Keep iced default features on (x11/wayland backends).
- No Secret Service in CI: tests use `MemoryStore`.
- Bubbles (`chat::finish`) are explicitly `width(Shrink)`: iced 0.14 makes a
  container with a Fill child Fill, and the bubble then split the row 50/50
  with the spacer. Columns inside a bubble that hold Fill children need
  `.width(Shrink)` too, or the compressed layout squeezes text to one glyph.
  `ui::chat::layout_tests` checks natural width and containment.
- Contact-name resync must reset the collection version first: wacore drops
  a snapshot that is not newer than the stored version. Library app-state
  logs use targets `Client/AppState` and `AppState`, not `whatsapp_rust`.
- Hover must never change layout: the message hover buttons live in a slot
  that is always laid out (`chat::action_slot`); `ui::chat::layout_tests`
  fails if a hovered row changes size.
- Names have sources (saved > business > push "~ Name" > number): never write
  names into one map; use `ChatList::resolve` / `set_name`. Display names of
  senders must not prefer `Message::sender_name` (a push name).
- `ferrochat` must not link a codec: only `src/bin/ferrochat-player/` may use
  `openh264` / `mp4` / `symphonia`. OpenH264 needs `Flush::NoFlush` or
  B-frames (Main/High) fail to decode. Video frames are `image::Handle`s made in `update`, never
  in `view`; the video viewer replaces the whole view while open.
- Chat flags / lists are optimistic: apply, persist, send, revert on failure
  (`app/lists.rs`, `app/chat_actions.rs`). Clear / delete chat are the exception:
  sent first, applied when the phone has them. Mutes have an end
  (`ChatMeta::mute_end`, seconds, -1 = always): read them through `App::is_muted` /
  `ChatMeta::is_muted(now)`, never as a flag. Never send the `favorites` app-state action (it carries
  the whole list and would overwrite the phone's).
- App-level tests: `app::test_support::Harness` (real `App` + fake client).
- Updater (`src/update/`, `app/updates.rs`, `ui/update.rs`, architecture §11.5): only
  install.sh installs self-update; cargo installs get the command, others the release
  page. Checks/installs run on detached threads (never `spawn_blocking`: quit waits for
  it). No checks in mock/screenshot/debug unless `FERRO_UPDATE_FORCE=1`; tests point
  `FERRO_RELEASE_API`/`endpoint_for` at a loopback HTTP server. Restart is spawned by
  `main` after the instance lock is dropped. Keep the asset names in sync with release.yml.
- Statuses are not chats (`net::social`, `app/status.rs`): diverted in
  `net/whatsapp.rs::translate`, own cache tables, 24 h + 1500-row bound. View
  receipts go through `WaClient::send_status_view`, which respects the
  `readreceipts` privacy setting. All Status / panel / star / logout actions
  are `Ui::Nav(NavMsg)` handled in `app/nav.rs`; leaving the status viewer
  must clear `App::video`.
- Paste-to-send (`app/paste.rs`): Ctrl+V / Shift+Insert is seen by the
  global event listener while the composer's `text_input` pastes text on
  its own. `arboard` (with `wayland-data-control`, or Wayland reads fail)
  is read in `spawn_blocking`, and only when `operation::is_focused(COMPOSER_ID)`.
  Order: file list > `file://`-only text > any text (stays a text paste) >
  image. File managers also paste the paths as text: `strip_pasted_paths`
  removes them. Picked and pasted media both go through `AttachPreview`;
  its image handle is made in `update`.
- Sending video / GIF (`app/paste.rs`): `.mp4`/`.m4v` (and `.gif`, converted first) are staged as documents, then
  `paste::finish_videos` runs the helper (`media::video::probe`, muted; `gif_to_mp4` = `ferrochat-player
  --gif-to-mp4`) as a child process, never on the blocking pool; a failure keeps the document. All file
  sources (paperclip, paste, drop) go through `paste::stage_paths` / `finish_videos`. The GIF encoder lives
  in `src/bin/ferrochat-player/gif.rs` (OpenH264 encoder + `mp4` writer): the main binary still links no codec.
  `Content::Video::gif` is `#[serde(default)]` (old rows = plain video); GIFs loop in the viewer (`VideoView::looping`).
- Drag and drop (`app/drop.rs`): one `FileDropped` per file, gathered for `drop::GATHER` and staged together;
  the overlay follows `FileHovered` / `FilesHoveredLeft`. winit 0.30 has no drag and drop on native Wayland
  (works on X11 / XWayland, Windows, macOS).
- Never park an unbounded wait on tokio's blocking pool: iced drops its runtime on quit and that waits for
  every `spawn_blocking` task (KDE keeps notifications open, so a click watch there hung Quit). Notification
  click watches run on detached threads (`notify::show`); `tests/notify_quit.rs` guards it.
- Composer is a `text_editor` (`app/composer.rs`): Enter sends via `key_binding` (`Binding::Custom`),
  Shift+Enter is the editor's own `Binding::Enter`. `min_height`/`max_height` limit the TEXT, padding comes
  on top: use `composer::text_height_limits`. Cursor columns are bytes. Text set programmatically is not
  drawn in `--screenshot` captures (tiny-skia replays the frame): take composer shots with xdotool.
- Emoji: bundled COLRv0 Twemoji renamed "Ferrochat Twemoji" (`ui::emoji_font`), installed zero-copy and first
  in cosmic-text's fallback list (`emoji_font::install` in `main`), so even span-less text gets colour emoji.
  Use `emoji_text`/`emoji_spans` (grapheme clusters) where the text font has the base glyph (keycaps, (c),
  digits). Never ship WhatsApp's Apple-style emoji (brand asset), never rely on a system emoji font
  (COLRv1 Noto draws blank/mono in swash). Picker data: `emojis` crate up to Unicode 17 (`app/emoji.rs`).
- tiny-skia does not clip images: zoomed / covering pictures must sit UNDER opaque layers (image viewer
  bar) or use `ContentFit::Contain` (info drawer thumbnails).
- Forwarding (`app/forward.rs`, `ui/forward.rs`): `WaClient::forward` rebuilds the body from the cached
  `Content` (`convert::build_forward` + the library's `prepare_for_forward`), so media is relayed, not
  re-uploaded; `Message::forwarded` is the forwarding score (`None` = not forwarded). Copies of one
  forward get ids from `forward::ordered_ids` (same second, sorted by id) and are sent one after the
  other. Selection mode covers each row with a click catcher; never send a forward for
  `!Message::can_forward()`.
- Mentions: show `ChatList::mention_label` / `render_mentions` (uses `Message::mentions`), never raw
  "@<LID>" text. Previews go through `App::preview`.
- Info drawer queries (`WaClient::contact_info/group_info/common_groups/is_blocked`) run only when it
  opens; results are dropped if the drawer moved on (`app/info.rs`).
- Read state: a message counts as read only while `App::reading_chat()` (focused, newest page, within
  `BOTTOM_SLACK` of the bottom). `send_pending_reads` clears the badge + cached counter and sends
  receipts to the wire JID (`App::wire`: LID the message came from). History chunks must not re-mark
  the chat being read. Regression tests: `app::flows` read-state section.

- Calls (`docs/protocol.md` §8): `whatsapp-rust` feature `voip` (MLOW + libopus, webrtc DTLS/SCTP)
  does signalling, call key, relay and SRTP; we only feed 16 kHz 960-sample PCM frames
  (`call::audio`). Never build `<call>` stanzas by hand: decline is `voip().reject(&offer)`,
  which needs the stored offer (`net::call::CallBook`). State machine: `call::Calls` (pure,
  tested); app glue `app/call.rs`. Video calls are NOT implemented: the video button must keep
  saying so. `--mock`/tests open no audio device. Each call is logged once (`call-<id>` bubble).
- Expired media (`app/media_fetch.rs`): every download goes through `media_fetch::download` with a `MediaOrigin`
  (`App::media_origin`); on `Error::MediaExpired` (CDN 404/410) it calls `WaClient::reupload`, caches the new ref
  (`Store::set_media_ref`) and parks it in `App::fresh_media`, applied to the open chat at the start of
  `App::update` (`apply_fresh_media`). Forward refreshes media older than `forward::STALE_MEDIA_SECS` first.
- Message info (`app/msg_info.rs`): receipt times live in `receipt_times` (schema 9), written by
  `App::record_receipt_times` from every `Event::Receipt` (`timestamp` 0 = now); the open panel reloads after
  the write (chained task). Only our messages; it shares the right slot with the info drawer (one at a time).
