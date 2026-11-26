//! The Status tab: contacts' 24 hour updates, the full-window viewer with
//! auto-advance, view receipts and "My status".
//!
//! Statuses are never conversations. They arrive as [`Event::Status`], live
//! in the `statuses` table of the cache, and are kept in memory only while
//! live (24 h) and bounded ([`MAX_STATUS_ROWS`]). Pictures are downloaded
//! when the viewer reaches them, held in a byte-budgeted LRU and turned into
//! `image::Handle`s in `update`, never in `view`.

use std::collections::HashSet;

use iced::Task;
use iced::widget::image;

use super::message::{Done, Message};
use super::nav::{NavDone, NavMsg, Section};
use super::{App, blocking, now_secs, store_task};
use crate::media::ByteLru;
use crate::net::social::{MAX_STATUS_TEXT, StatusBody, StatusGroup, StatusItem, is_status_jid};
use crate::net::{Event, MediaRef};
use crate::store::{MAX_STATUS_ROWS, StoredStatus};

/// Bytes of full-size status pictures kept decoded.
pub const STATUS_IMAGE_BUDGET: usize = 24 * 1024 * 1024;
/// Bytes of status thumbnails kept decoded.
pub const STATUS_THUMB_BUDGET: usize = 6 * 1024 * 1024;
/// Viewer clock step (ms) while the viewer ticks.
pub const TICK_MS: u64 = 100;
/// Background colours offered when posting a text status (`0xAARRGGBB`).
pub const TEXT_COLORS: [u32; 8] = [
    0xFF1E_6E4F,
    0xFF0D_47A1,
    0xFF8E_24AA,
    0xFFC6_2828,
    0xFFEF_6C00,
    0xFF00_695C,
    0xFF37_474F,
    0xFF5D_4037,
];
/// Font ids (WhatsApp's `FontType`) offered when posting a text status.
pub const TEXT_FONTS: [i32; 4] = [0, 6, 8, 10];

/// Statuses split the way the list shows them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Split {
    /// Our own live statuses.
    pub mine: Option<StatusGroup>,
    /// Authors with something not seen yet, newest first.
    pub recent: Vec<StatusGroup>,
    /// Authors whose statuses were all seen, newest first.
    pub viewed: Vec<StatusGroup>,
}

/// Groups live statuses by author. `own` is our JID; statuses we posted
/// (`from_me`) form `mine`. Pure.
pub fn split(
    items: &[StatusItem],
    viewed: &HashSet<(String, String)>,
    own: &str,
    now: i64,
) -> Split {
    let seen = |i: &StatusItem| viewed.contains(&(i.author.clone(), i.id.clone()));
    let mut by_author: Vec<(String, Vec<StatusItem>)> = Vec::new();
    let mut mine_items: Vec<StatusItem> = Vec::new();
    for it in items.iter().filter(|i| i.is_live(now)) {
        if it.from_me || (!own.is_empty() && it.author == own) {
            mine_items.push(it.clone());
        } else if let Some((_, v)) = by_author.iter_mut().find(|(a, _)| *a == it.author) {
            v.push(it.clone());
        } else {
            by_author.push((it.author.clone(), vec![it.clone()]));
        }
    }
    let order = |v: &mut Vec<StatusItem>| {
        v.sort_by(|a, b| (a.timestamp, &a.id).cmp(&(b.timestamp, &b.id)));
    };
    let mine = (!mine_items.is_empty()).then(|| {
        order(&mut mine_items);
        StatusGroup {
            author: mine_items[0].author.clone(),
            viewed: mine_items.len(),
            items: mine_items.clone(),
        }
    });
    let groups: Vec<StatusGroup> = by_author
        .into_iter()
        .map(|(author, mut items)| {
            order(&mut items);
            // Seen statuses count from the start, so "resume" lands on the
            // first unseen one; everything seen means the author is viewed.
            let viewed = if items.iter().all(&seen) {
                items.len()
            } else {
                items.iter().take_while(|i| seen(i)).count()
            };
            StatusGroup {
                author,
                items,
                viewed,
            }
        })
        .collect();
    let (mut viewed_g, mut recent): (Vec<_>, Vec<_>) =
        groups.into_iter().partition(StatusGroup::all_viewed);
    recent.sort_by_key(|g| std::cmp::Reverse(g.newest()));
    viewed_g.sort_by_key(|g| std::cmp::Reverse(g.newest()));
    Split {
        mine,
        recent,
        viewed: viewed_g,
    }
}

/// Where the viewer is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusViewer {
    /// Authors to play, in order.
    pub queue: Vec<String>,
    /// Current author (index into `queue`).
    pub pos: usize,
    /// Current status of that author.
    pub index: usize,
    /// Milliseconds shown so far.
    pub elapsed_ms: u64,
    /// Paused (pointer held / space).
    pub paused: bool,
}

/// What a navigation step does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Show this author / status.
    Go {
        /// Author position in the queue.
        pos: usize,
        /// Status index.
        index: usize,
    },
    /// Nothing left: close the viewer.
    Close,
    /// Already at the very beginning.
    Stay,
}

/// The step after `(pos, index)` given how many statuses each queued author
/// has. Pure.
pub fn next_step(counts: &[usize], pos: usize, index: usize) -> Step {
    if counts.get(pos).is_some_and(|c| index + 1 < *c) {
        return Step::Go {
            pos,
            index: index + 1,
        };
    }
    match counts.get(pos + 1) {
        Some(c) if *c > 0 => Step::Go {
            pos: pos + 1,
            index: 0,
        },
        Some(_) => next_step(counts, pos + 1, 0),
        None => Step::Close,
    }
}

/// The step before `(pos, index)`: the previous status, or the last one of
/// the previous author. Pure.
pub fn prev_step(counts: &[usize], pos: usize, index: usize) -> Step {
    if index > 0 {
        return Step::Go {
            pos,
            index: index - 1,
        };
    }
    if pos == 0 {
        return Step::Stay;
    }
    match counts.get(pos - 1) {
        Some(c) if *c > 0 => Step::Go {
            pos: pos - 1,
            index: c - 1,
        },
        _ => prev_step(counts, pos - 1, 0),
    }
}

/// Fraction (0..=1) of the progress bar of item `i` of the current author:
/// full for past items, partial for the current, empty after. Pure.
pub fn segment_fill(i: usize, current: usize, progress: f32) -> f32 {
    use std::cmp::Ordering::*;
    match i.cmp(&current) {
        Less => 1.0,
        Equal => progress.clamp(0.0, 1.0),
        Greater => 0.0,
    }
}

/// Posting a status is switched off: the library does not expose the account's
/// status privacy lists ("My contacts except..." / "Only share with..."), so a
/// post would go to every saved contact and could leak to people the user
/// excluded. The code stays; flip this once the lists are readable.
pub const STATUS_POSTING_ENABLED: bool = false;

/// The text status composer.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Compose {
    /// Text typed.
    pub text: String,
    /// Index into [`TEXT_COLORS`].
    pub color: usize,
    /// Index into [`TEXT_FONTS`].
    pub font: usize,
    /// Being sent.
    pub sending: bool,
}

/// Everything the Status tab keeps in memory.
pub struct StatusState {
    /// Live statuses, oldest first.
    pub items: Vec<StatusItem>,
    /// `(author, id)` pairs the user has seen.
    pub viewed: HashSet<(String, String)>,
    /// The viewer, when open.
    pub viewer: Option<StatusViewer>,
    /// Thumbnails by `author|id`.
    pub thumbs: ByteLru<String, image::Handle>,
    /// Full pictures by media cache key.
    pub images: ByteLru<String, image::Handle>,
    /// Pictures being downloaded.
    pub loading: HashSet<String>,
    /// Pictures that failed (a click retries).
    pub failed: HashSet<String>,
    /// Text composer.
    pub compose: Option<Compose>,
}

impl Default for StatusState {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            viewed: HashSet::new(),
            viewer: None,
            thumbs: ByteLru::new(STATUS_THUMB_BUDGET),
            images: ByteLru::new(STATUS_IMAGE_BUDGET),
            loading: HashSet::new(),
            failed: HashSet::new(),
            compose: None,
        }
    }
}

impl StatusState {
    /// Key of a thumbnail.
    pub fn thumb_key(author: &str, id: &str) -> String {
        format!("{author}|{id}")
    }

    /// Adds statuses (dedup by author + id), keeps the list sorted and
    /// bounded, builds thumbnails. Returns the ids that were new.
    pub fn add(&mut self, new: Vec<StatusItem>, now: i64) -> Vec<StatusItem> {
        let mut added = Vec::new();
        for it in new {
            if !it.is_live(now)
                || self
                    .items
                    .iter()
                    .any(|e| e.author == it.author && e.id == it.id)
            {
                continue;
            }
            let mut it = it;
            if let Some(t) = it.body.thumbnail() {
                self.thumbs.put(
                    Self::thumb_key(&it.author, &it.id),
                    image::Handle::from_bytes(t.to_vec()),
                    t.len(),
                );
            }
            // The caller persists the full item (thumbnail included); the
            // in-memory copy keeps only the decoded handle, so grouping the
            // list every frame never copies thumbnail bytes.
            added.push(it.clone());
            if let StatusBody::Image { thumbnail, .. } | StatusBody::Video { thumbnail, .. } =
                &mut it.body
            {
                *thumbnail = None;
            }
            self.items.push(it);
        }
        self.items
            .sort_by(|a, b| (a.timestamp, &a.id).cmp(&(b.timestamp, &b.id)));
        self.bound();
        added
    }

    /// Drops expired statuses and, past the cap, the oldest ones.
    pub fn bound(&mut self) {
        self.items.truncate_front_to(MAX_STATUS_ROWS);
    }

    /// Removes expired statuses; returns true when something went away.
    pub fn expire(&mut self, now: i64) -> bool {
        let before = self.items.len();
        self.items.retain(|i| i.is_live(now));
        let live: HashSet<(String, String)> = self
            .items
            .iter()
            .map(|i| (i.author.clone(), i.id.clone()))
            .collect();
        self.viewed.retain(|k| live.contains(k));
        self.items.len() != before
    }

    /// Removes one status (revoked).
    pub fn remove(&mut self, author: &str, id: &str) -> bool {
        let before = self.items.len();
        self.items.retain(|i| !(i.author == author && i.id == id));
        self.viewed.remove(&(author.to_string(), id.to_string()));
        self.thumbs.pop(&Self::thumb_key(author, id));
        self.items.len() != before
    }

    /// Some author has a status not seen yet.
    pub fn has_unseen(&self, own: &str, now: i64) -> bool {
        !split(&self.items, &self.viewed, own, now).recent.is_empty()
    }

    /// The status the viewer shows now.
    pub fn current<'a>(&'a self, groups: &'a [StatusGroup]) -> Option<&'a StatusItem> {
        let v = self.viewer.as_ref()?;
        let author = v.queue.get(v.pos)?;
        groups
            .iter()
            .find(|g| &g.author == author)?
            .items
            .get(v.index)
    }
}

/// `Vec` helper: keep only the newest `n` (the vec is oldest first).
trait TruncateFront {
    fn truncate_front_to(&mut self, n: usize);
}

impl<T> TruncateFront for Vec<T> {
    fn truncate_front_to(&mut self, n: usize) {
        if self.len() > n {
            let extra = self.len() - n;
            self.drain(..extra);
        }
    }
}

impl App {
    /// All live statuses grouped for the list and the viewer.
    pub(crate) fn status_split(&self) -> Split {
        split(
            &self.status.items,
            &self.status.viewed,
            self.own_jid.as_deref().unwrap_or_default(),
            now_secs(),
        )
    }

    /// Statuses arrived (live, offline queue or the mock).
    pub(super) fn on_status_items(&mut self, mut items: Vec<StatusItem>) -> Task<Message> {
        let now = now_secs();
        items.retain(|i| !is_status_jid(&i.author));
        for it in &mut items {
            // LID authors are shown and grouped under their phone number.
            it.author = self.lid_map.normalize(&it.author);
        }
        let added = self.status.add(items, now);
        if added.is_empty() {
            return Task::none();
        }
        let authors: Vec<String> = added.iter().map(|i| i.author.clone()).collect();
        let persist = store_task(&self.store, move |s| {
            s.insert_statuses(&added, now_secs()).map(|_| ())
        });
        let avatars = if self.nav.section == Section::Status {
            self.want_avatars(authors)
        } else {
            Task::none()
        };
        let mut tasks = vec![persist, avatars];
        tasks.extend(self.nav_scene(super::nav::SceneStage::Statuses));
        Task::batch(tasks)
    }

    pub(super) fn on_status_revoked(&mut self, author: String, id: String) -> Task<Message> {
        let author = self.lid_map.normalize(&author);
        let mut close = false;
        if self.status.viewer.is_some() {
            // The one on screen was just deleted: move on.
            let split = self.status_split();
            let all = [split.recent.clone(), split.viewed.clone()].concat();
            close = self
                .status
                .current(&all)
                .is_some_and(|c| c.author == author && c.id == id);
        }
        if !self.status.remove(&author, &id) {
            return Task::none();
        }
        let persist = store_task(&self.store, move |s| s.delete_status(&author, &id));
        if close {
            self.clear_status_viewer();
        }
        persist
    }

    /// Loads the cached statuses at start-up.
    pub(super) fn load_statuses_task(&self) -> Task<Message> {
        let store = self.store.clone();
        blocking(
            move || {
                let now = now_secs();
                let _ = store.prune_statuses(now, MAX_STATUS_ROWS);
                store.load_statuses(now).map_err(|e| e.to_string())
            },
            |r| Message::done(Done::Nav(Box::new(NavDone::StatusesLoaded(r)))),
        )
    }

    pub(super) fn on_statuses_loaded(
        &mut self,
        result: Result<Vec<StoredStatus>, String>,
    ) -> Task<Message> {
        let Ok(rows) = result else {
            return Task::none();
        };
        let now = now_secs();
        let mut items = Vec::new();
        for r in rows {
            if r.viewed {
                self.status
                    .viewed
                    .insert((r.item.author.clone(), r.item.id.clone()));
            }
            items.push(r.item);
        }
        self.status.add(items, now);
        Task::none()
    }

    /// Opens the viewer on `author`'s statuses, starting at the first one
    /// not seen (or the beginning when all were).
    pub(super) fn open_status(&mut self, author: &str) -> Task<Message> {
        let split = self.status_split();
        let list = if split.recent.iter().any(|g| g.author == author) {
            &split.recent
        } else if split.viewed.iter().any(|g| g.author == author) {
            &split.viewed
        } else if split.mine.as_ref().is_some_and(|g| g.author == author) {
            // Our own statuses play on their own.
            let mine = split.mine.iter().cloned().collect::<Vec<_>>();
            return self.start_viewer(&mine, 0, 0);
        } else {
            return Task::none();
        };
        let pos = list.iter().position(|g| g.author == author).unwrap_or(0);
        let index = list[pos].start_index();
        // Continue with the contacts after this one in the same section.
        let queue: Vec<StatusGroup> = list[pos..].to_vec();
        self.start_viewer(&queue, 0, index)
    }

    fn start_viewer(&mut self, queue: &[StatusGroup], pos: usize, index: usize) -> Task<Message> {
        if queue.is_empty() {
            return Task::none();
        }
        self.status.viewer = Some(StatusViewer {
            queue: queue.iter().map(|g| g.author.clone()).collect(),
            pos,
            index,
            elapsed_ms: 0,
            paused: false,
        });
        self.menu_open = false;
        self.enter_status()
    }

    /// Counts of statuses per queued author (fresh, since statuses can
    /// expire or be revoked while the viewer is open).
    fn viewer_counts(&self) -> Vec<usize> {
        let Some(v) = &self.status.viewer else {
            return Vec::new();
        };
        let split = self.status_split();
        v.queue
            .iter()
            .map(|a| {
                split
                    .recent
                    .iter()
                    .chain(split.viewed.iter())
                    .chain(split.mine.iter())
                    .find(|g| &g.author == a)
                    .map(|g| g.items.len())
                    .unwrap_or(0)
            })
            .collect()
    }

    /// The status on screen, looked up by queue position / index.
    pub(crate) fn current_status(&self) -> Option<StatusItem> {
        let v = self.status.viewer.as_ref()?;
        let author = v.queue.get(v.pos)?;
        let split = self.status_split();
        split
            .recent
            .iter()
            .chain(split.viewed.iter())
            .chain(split.mine.iter())
            .find(|g| &g.author == author)?
            .items
            .get(v.index)
            .cloned()
    }

    /// A status just came on screen: mark it seen, tell its author, start
    /// loading its media (and play it, if it is a video).
    fn enter_status(&mut self) -> Task<Message> {
        if let Some(v) = &mut self.status.viewer {
            v.elapsed_ms = 0;
        }
        // A video of the previous status must not keep playing.
        let stop = if self.video.is_some() {
            self.close_video()
        } else {
            Task::none()
        };
        let Some(item) = self.current_status() else {
            self.clear_status_viewer();
            return stop;
        };
        let mut tasks = vec![stop];
        let key = (item.author.clone(), item.id.clone());
        let first_view = !item.from_me && self.status.viewed.insert(key);
        if first_view {
            let (author, id) = (item.author.clone(), item.id.clone());
            let (a2, i2) = (author.clone(), id.clone());
            tasks.push(store_task(&self.store, move |s| {
                s.mark_statuses_viewed(&a2, std::slice::from_ref(&i2))
            }));
            if let Some(client) = self.client.clone() {
                tasks.push(Task::perform(
                    async move {
                        let ids = [id];
                        client.send_status_view(&author, &ids).await
                    },
                    |r| {
                        if let Err(e) = r {
                            tracing::warn!(error = %e, "status view receipt failed");
                        }
                        Message::done(Done::Nothing)
                    },
                ));
            }
        }
        match &item.body {
            StatusBody::Image { media, .. } => {
                if let Some(m) = media.clone() {
                    tasks.push(self.fetch_status_image(m));
                }
            }
            StatusBody::Video { media, .. } => {
                if let Some(m) = media.clone() {
                    tasks.push(self.play_status_video(&item.id, m));
                }
            }
            StatusBody::Text { .. } => {}
        }
        // Warm the next picture so auto-advance does not stall.
        if let Some(next) = self.peek_next_status()
            && let StatusBody::Image { media: Some(m), .. } = &next.body
        {
            tasks.push(self.fetch_status_image(m.clone()));
        }
        Task::batch(tasks)
    }

    fn peek_next_status(&self) -> Option<StatusItem> {
        let v = self.status.viewer.as_ref()?;
        let counts = self.viewer_counts();
        let Step::Go { pos, index } = next_step(&counts, v.pos, v.index) else {
            return None;
        };
        let split = self.status_split();
        let author = v.queue.get(pos)?;
        split
            .recent
            .iter()
            .chain(split.viewed.iter())
            .chain(split.mine.iter())
            .find(|g| &g.author == author)?
            .items
            .get(index)
            .cloned()
    }

    fn fetch_status_image(&mut self, mref: MediaRef) -> Task<Message> {
        let key = mref.cache_key();
        if self.status.images.contains(&key) || self.status.loading.contains(&key) {
            return Task::none();
        }
        let Some(client) = self.client.clone() else {
            return Task::none();
        };
        self.status.failed.remove(&key);
        self.status.loading.insert(key.clone());
        Task::perform(
            async move {
                let bytes = client.download(&mref).await.map_err(|e| e.to_string())?;
                // Decode off the UI thread; the cap protects from bombs.
                tokio::task::spawn_blocking(move || {
                    crate::media::image::decode_limited(&bytes)
                        .map(|img| {
                            let rgba = img.to_rgba8();
                            (rgba.width(), rgba.height(), rgba.into_raw())
                        })
                        .map_err(|e| e.to_string())
                })
                .await
                .map_err(|e| e.to_string())?
            },
            move |result| Message::done(Done::Nav(Box::new(NavDone::StatusImage { key, result }))),
        )
    }

    pub(super) fn on_status_image(
        &mut self,
        key: String,
        result: Result<(u32, u32, Vec<u8>), String>,
    ) {
        self.status.loading.remove(&key);
        match result {
            Ok((w, h, rgba)) => {
                let size = rgba.len();
                self.status
                    .images
                    .put(key, image::Handle::from_rgba(w, h, rgba), size);
            }
            Err(e) => {
                tracing::warn!(error = %e, "status picture failed");
                self.status.failed.insert(key);
            }
        }
    }

    /// Starts the existing video viewer helper for a status video.
    fn play_status_video(&mut self, id: &str, mref: MediaRef) -> Task<Message> {
        let Some(client) = self.client.clone() else {
            return Task::none();
        };
        self.video_session = self.video_session.wrapping_add(1);
        self.video = Some(super::video::VideoView::for_id(
            id.to_string(),
            self.video_session,
        ));
        let dir = self.video_dir();
        let sid = id.to_string();
        Task::perform(
            super::video::fetch_video_file(client, mref, dir, None),
            move |result| Message::done(Done::VideoFile { id: sid, result }),
        )
    }

    /// Called after every video helper event: a status video that ended
    /// moves the viewer on.
    pub(super) fn status_after_video(&mut self) -> Task<Message> {
        if self.status.viewer.is_none() {
            return Task::none();
        }
        let ended = self
            .video
            .as_ref()
            .is_some_and(|vid| matches!(vid.state, crate::media::video_proto::PlayState::Ended));
        // A failed video is skipped by the viewer clock (see `status_tick`).
        if ended {
            return self.status_step(true);
        }
        Task::none()
    }

    /// Moves forward or back; closes after the last one.
    pub(super) fn status_step(&mut self, forward: bool) -> Task<Message> {
        let Some(v) = &self.status.viewer else {
            return Task::none();
        };
        let counts = self.viewer_counts();
        let step = if forward {
            next_step(&counts, v.pos, v.index)
        } else {
            prev_step(&counts, v.pos, v.index)
        };
        match step {
            Step::Go { pos, index } => {
                if let Some(v) = &mut self.status.viewer {
                    v.pos = pos;
                    v.index = index;
                }
                self.enter_status()
            }
            Step::Close => self.close_status(),
            // First status of the first author: restart it.
            Step::Stay => self.enter_status(),
        }
    }

    pub(super) fn close_status(&mut self) -> Task<Message> {
        self.clear_status_viewer();
        Task::none()
    }

    /// Closes the viewer. A video playing in it stops with it (the helper
    /// process ends when its handle is dropped); the status viewer and the
    /// chat video viewer are never open together.
    fn clear_status_viewer(&mut self) {
        self.status.viewer = None;
        self.video = None;
    }

    /// Progress of the current status, 0..=1. Videos report the helper's
    /// position, everything else the viewer clock.
    pub(crate) fn status_progress(&self, item: &StatusItem) -> f32 {
        let Some(v) = &self.status.viewer else {
            return 0.0;
        };
        if matches!(item.body, StatusBody::Video { .. })
            && let Some(vid) = self.video.as_ref().filter(|x| x.id == item.id)
            && vid.duration_ms > 0
        {
            return (vid.position_ms as f32 / vid.duration_ms as f32).clamp(0.0, 1.0);
        }
        (v.elapsed_ms as f32 / item.duration_ms().max(1) as f32).clamp(0.0, 1.0)
    }

    /// The picture of a status is ready to be drawn (or needs no download).
    pub(crate) fn status_ready(&self, item: &StatusItem) -> bool {
        match &item.body {
            StatusBody::Text { .. } => true,
            StatusBody::Image { media, .. } => media
                .as_ref()
                .is_some_and(|m| self.status.images.contains(&m.cache_key())),
            StatusBody::Video { .. } => self
                .video
                .as_ref()
                .is_some_and(|v| v.id == item.id && v.phase == super::video::VideoPhase::Playing),
        }
    }

    /// Viewer clock. Images and text advance by time; videos by the helper.
    pub(super) fn status_tick(&mut self) -> Task<Message> {
        let Some(item) = self.current_status() else {
            self.clear_status_viewer();
            return Task::none();
        };
        let ready = self.status_ready(&item);
        let is_video = matches!(item.body, StatusBody::Video { .. });
        let video_failed = self
            .video
            .as_ref()
            .is_some_and(|v| matches!(v.phase, super::video::VideoPhase::Failed(_)));
        let image_failed = match &item.body {
            StatusBody::Image { media, .. } => media
                .as_ref()
                .is_none_or(|m| self.status.failed.contains(&m.cache_key())),
            _ => false,
        };
        let Some(v) = &mut self.status.viewer else {
            return Task::none();
        };
        if v.paused {
            return Task::none();
        }
        // A video drives itself while it plays; until it plays (or if it
        // cannot) the clock runs so the queue never stalls.
        if is_video && ready && !video_failed {
            return Task::none();
        }
        // Wait for the picture; a failed one is skipped after the usual time.
        if !ready && !image_failed && !video_failed && !is_video {
            return Task::none();
        }
        if is_video && !ready && !video_failed {
            return Task::none();
        }
        v.elapsed_ms += TICK_MS;
        let limit = if video_failed || image_failed {
            crate::net::social::STATUS_IMAGE_MS
        } else {
            item.duration_ms()
        };
        if v.elapsed_ms >= limit {
            return self.status_step(true);
        }
        Task::none()
    }

    // ---------------------------------------------------------- posting

    /// Our saved contacts (phone JIDs): who a posted status goes to. This is
    /// the "My contacts" audience of the official client.
    pub(crate) fn status_audience(&self) -> Vec<String> {
        self.contacts().into_iter().map(|c| c.jid).collect()
    }

    pub(super) fn status_post(&mut self) -> Task<Message> {
        let Some(c) = self.status.compose.as_mut() else {
            return Task::none();
        };
        let text = c.text.trim().to_string();
        if text.is_empty() || c.sending {
            return Task::none();
        }
        let text: String = text.chars().take(MAX_STATUS_TEXT).collect();
        let (bg, font) = (
            TEXT_COLORS[c.color % TEXT_COLORS.len()],
            TEXT_FONTS[c.font % TEXT_FONTS.len()],
        );
        let Some(client) = self.client.clone() else {
            self.toast("Not connected yet.");
            return Task::none();
        };
        let recipients = self.status_audience();
        if recipients.is_empty() {
            self.toast("No saved contacts to share a status with.");
            return Task::none();
        }
        if let Some(c) = self.status.compose.as_mut() {
            c.sending = true;
        }
        let own = self.own_jid.clone().unwrap_or_default();
        let shown = text.clone();
        Task::perform(
            async move {
                client
                    .post_status_text(&text, bg, font, &recipients)
                    .await
                    .map_err(|e| e.to_string())
            },
            move |r| {
                Message::done(Done::Nav(Box::new(NavDone::StatusPosted {
                    result: r,
                    own: own.clone(),
                    text: shown.clone(),
                    background: bg,
                    font,
                })))
            },
        )
    }

    pub(super) fn on_status_posted(
        &mut self,
        result: Result<String, String>,
        own: String,
        text: String,
        background: u32,
        font: i32,
    ) -> Task<Message> {
        match result {
            Ok(id) => {
                self.status.compose = None;
                let item = StatusItem {
                    id,
                    author: own,
                    author_name: None,
                    from_me: true,
                    timestamp: now_secs(),
                    body: StatusBody::Text {
                        text,
                        background,
                        font,
                    },
                };
                self.toast("Status posted.");
                self.on_status_items(vec![item])
            }
            Err(e) => {
                tracing::warn!(error = %e, "status post failed");
                if let Some(c) = self.status.compose.as_mut() {
                    c.sending = false;
                }
                self.toast("Could not post your status.");
                Task::none()
            }
        }
    }

    /// Reducer for the status messages of [`NavMsg`]; `Err(msg)` for others.
    pub(super) fn on_status_msg(&mut self, msg: NavMsg) -> Result<Task<Message>, NavMsg> {
        Ok(match msg {
            NavMsg::OpenStatus(author) => self.open_status(&author),
            NavMsg::StatusNext => self.status_step(true),
            NavMsg::StatusPrev => self.status_step(false),
            NavMsg::StatusTick => self.status_tick(),
            NavMsg::StatusPause => {
                if let Some(v) = &mut self.status.viewer {
                    v.paused = !v.paused;
                }
                Task::none()
            }
            NavMsg::CloseStatus => self.close_status(),
            NavMsg::StatusCompose => {
                if STATUS_POSTING_ENABLED {
                    self.status.compose = Some(Compose::default());
                }
                Task::none()
            }
            NavMsg::StatusComposeText(t) => {
                if let Some(c) = &mut self.status.compose {
                    c.text = t.chars().take(MAX_STATUS_TEXT).collect();
                }
                Task::none()
            }
            NavMsg::StatusComposeColor => {
                if let Some(c) = &mut self.status.compose {
                    c.color = (c.color + 1) % TEXT_COLORS.len();
                }
                Task::none()
            }
            NavMsg::StatusComposeFont => {
                if let Some(c) = &mut self.status.compose {
                    c.font = (c.font + 1) % TEXT_FONTS.len();
                }
                Task::none()
            }
            NavMsg::StatusComposeCancel => {
                self.status.compose = None;
                Task::none()
            }
            NavMsg::StatusPost => self.status_post(),
            other => return Err(other),
        })
    }

    /// Esc: closes the viewer or the composer. True when something closed.
    pub(super) fn status_escape(&mut self) -> bool {
        if self.status.viewer.is_some() {
            // The close task (stopping a video) is queued by `close_status`
            // through the normal message; here only the state is cleared.
            self.clear_status_viewer();
            return true;
        }
        self.status.compose.take().is_some()
    }

    /// Reacts to a protocol event that concerns statuses. `Err(ev)` for
    /// everything else.
    pub(super) fn on_status_event(&mut self, ev: Event) -> Result<Task<Message>, Event> {
        match ev {
            Event::Status(items) => Ok(self.on_status_items(items)),
            Event::StatusRevoked { author, id } => Ok(self.on_status_revoked(author, id)),
            other => Err(other),
        }
    }

    /// Periodic housekeeping: expire what is older than 24 h.
    pub(super) fn expire_statuses(&mut self) -> Task<Message> {
        let now = now_secs();
        if !self.status.expire(now) {
            return Task::none();
        }
        store_task(&self.store, move |s| {
            s.prune_statuses(now, MAX_STATUS_ROWS).map(|_| ())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::social::STATUS_TTL_SECS;

    fn it(id: &str, author: &str, ts: i64) -> StatusItem {
        StatusItem {
            id: id.into(),
            author: author.into(),
            author_name: None,
            from_me: false,
            timestamp: ts,
            body: StatusBody::Text {
                text: "t".into(),
                background: 0xFF00_0000,
                font: 0,
            },
        }
    }

    fn seen(pairs: &[(&str, &str)]) -> HashSet<(String, String)> {
        pairs
            .iter()
            .map(|(a, i)| (a.to_string(), i.to_string()))
            .collect()
    }

    #[test]
    fn split_orders_recent_by_newest_and_moves_fully_seen_authors_to_viewed() {
        let now = 100_000;
        let items = vec![
            it("1", "a", now - 500),
            it("2", "b", now - 300),
            it("3", "a", now - 100),
            it("4", "c", now - 200),
        ];
        let s = split(&items, &seen(&[("c", "4")]), "me", now);
        let recent: Vec<_> = s.recent.iter().map(|g| g.author.as_str()).collect();
        assert_eq!(recent, ["a", "b"], "a has the newest unseen status");
        assert_eq!(s.viewed.len(), 1);
        assert_eq!(s.viewed[0].author, "c");
        assert!(s.mine.is_none());
    }

    #[test]
    fn split_hides_expired_and_separates_our_own() {
        let now = 1_000_000;
        let mut mine = it("m", "me", now - 10);
        mine.from_me = true;
        let items = vec![
            it("old", "a", now - STATUS_TTL_SECS - 1),
            mine,
            it("x", "b", now - 5),
        ];
        let s = split(&items, &HashSet::new(), "me", now);
        assert_eq!(s.recent.len(), 1);
        assert_eq!(s.recent[0].author, "b");
        let m = s.mine.expect("own status");
        assert_eq!(m.items.len(), 1);
        assert!(m.all_viewed(), "our own are never 'unseen'");
    }

    #[test]
    fn partially_seen_author_resumes_at_the_first_unseen_status() {
        let now = 10_000;
        let items = vec![
            it("1", "a", now - 30),
            it("2", "a", now - 20),
            it("3", "a", now - 10),
        ];
        let s = split(&items, &seen(&[("a", "1")]), "me", now);
        assert_eq!(s.recent[0].viewed, 1);
        assert_eq!(s.recent[0].start_index(), 1);
    }

    #[test]
    fn next_step_walks_statuses_then_authors_then_closes() {
        let counts = [2, 1, 3];
        assert_eq!(next_step(&counts, 0, 0), Step::Go { pos: 0, index: 1 });
        assert_eq!(next_step(&counts, 0, 1), Step::Go { pos: 1, index: 0 });
        assert_eq!(next_step(&counts, 1, 0), Step::Go { pos: 2, index: 0 });
        assert_eq!(next_step(&counts, 2, 2), Step::Close);
        // An author whose statuses vanished is skipped.
        assert_eq!(next_step(&[1, 0, 2], 0, 0), Step::Go { pos: 2, index: 0 });
        assert_eq!(next_step(&[1, 0], 0, 0), Step::Close);
    }

    #[test]
    fn prev_step_goes_to_the_last_status_of_the_previous_author() {
        let counts = [2, 1, 3];
        assert_eq!(prev_step(&counts, 0, 1), Step::Go { pos: 0, index: 0 });
        assert_eq!(prev_step(&counts, 1, 0), Step::Go { pos: 0, index: 1 });
        assert_eq!(prev_step(&counts, 2, 0), Step::Go { pos: 1, index: 0 });
        assert_eq!(prev_step(&counts, 0, 0), Step::Stay);
    }

    #[test]
    fn segments_fill_before_the_current_one() {
        assert_eq!(segment_fill(0, 2, 0.4), 1.0);
        assert_eq!(segment_fill(2, 2, 0.4), 0.4);
        assert_eq!(segment_fill(3, 2, 0.4), 0.0);
        assert_eq!(segment_fill(2, 2, 7.0), 1.0, "clamped");
    }

    #[test]
    fn state_dedups_expires_removes_and_is_bounded() {
        let now = 1_000_000;
        let mut st = StatusState::default();
        let added = st.add(
            vec![
                it("1", "a", now - 10),
                it("1", "a", now - 10),
                it("2", "a", now - 5),
            ],
            now,
        );
        assert_eq!(added.len(), 2);
        assert_eq!(st.items.len(), 2);
        // Expired on arrival.
        assert!(
            st.add(vec![it("old", "a", now - STATUS_TTL_SECS - 1)], now)
                .is_empty()
        );
        st.viewed.insert(("a".into(), "1".into()));
        assert!(st.remove("a", "1"));
        assert!(st.viewed.is_empty());
        assert!(!st.expire(now));
        assert!(st.expire(now + STATUS_TTL_SECS));
        assert!(st.items.is_empty());

        let many: Vec<_> = (0..MAX_STATUS_ROWS + 50)
            .map(|i| it(&format!("{i}"), "a", now - 3000 + i as i64))
            .collect();
        st.add(many, now);
        assert_eq!(st.items.len(), MAX_STATUS_ROWS);
        // The oldest were dropped.
        assert_eq!(st.items[0].id, "50");
    }
}
