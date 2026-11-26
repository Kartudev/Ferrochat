//! The in-app video viewer: opens a video message, downloads it, runs the
//! player helper and keeps the viewer state (frame, position, volume).

use std::sync::{Arc, Mutex};

use futures::StreamExt;
use iced::Task;
use iced::widget::image;
use tokio::sync::mpsc::UnboundedReceiver;

use super::message::{Done, Message, Ui};
use super::{App, blocking};
use crate::media::video::{self, Event, Failure, Player};
use crate::media::video_proto::{Cmd, PlayState};
use crate::net::Content;

/// Where the viewer is in opening a video.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoPhase {
    /// Fetching the file.
    Downloading,
    /// The helper started; waiting for the first frame.
    Starting,
    /// Frames are flowing.
    Playing,
    /// Could not play (user-facing text).
    Failed(String),
}

/// A helper event tagged with the viewer session it belongs to, so events
/// of a closed viewer are ignored.
#[derive(Debug, Clone)]
pub struct VideoMsg {
    /// Session generation.
    pub session: u32,
    /// The event.
    pub event: Event,
}

type Shared = Arc<Mutex<Option<UnboundedReceiver<Event>>>>;

/// State of the open video viewer.
pub struct VideoView {
    /// Message id of the video.
    pub id: String,
    /// Opening / playing / failed.
    pub phase: VideoPhase,
    /// Latest frame, created when it arrives (never inside `view`).
    pub frame: Option<image::Handle>,
    /// Frame size in pixels.
    pub frame_size: (u32, u32),
    /// Position in ms.
    pub position_ms: u64,
    /// Duration in ms (0 = unknown).
    pub duration_ms: u64,
    /// Player state.
    pub state: PlayState,
    /// Volume 0..=100.
    pub volume: u8,
    /// Muted.
    pub muted: bool,
    /// Slider fraction while the user drags it.
    pub seeking: Option<f32>,
    /// Full screen.
    pub fullscreen: bool,
    /// A GIF: starts over when it ends, like WhatsApp plays GIFs.
    pub looping: bool,
    pub(crate) session: u32,
    pub(crate) rx: Shared,
    player: Option<Player>,
}

impl std::fmt::Debug for VideoView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "VideoView({:?})", self.phase)
    }
}

impl VideoView {
    /// A viewer state for `id` (also used by the status viewer, which draws
    /// the frames itself).
    pub(crate) fn for_id(id: String, session: u32) -> Self {
        Self::new(id, session)
    }

    fn new(id: String, session: u32) -> Self {
        Self {
            id,
            phase: VideoPhase::Downloading,
            frame: None,
            frame_size: (0, 0),
            position_ms: 0,
            duration_ms: 0,
            state: PlayState::Paused,
            volume: 100,
            muted: false,
            seeking: None,
            fullscreen: false,
            looping: false,
            session,
            rx: Arc::new(Mutex::new(None)),
            player: None,
        }
    }

    /// Slider position in `0.0..=1.0`.
    pub fn progress(&self) -> f32 {
        if let Some(s) = self.seeking {
            return s;
        }
        if self.duration_ms == 0 {
            return 0.0;
        }
        (self.position_ms as f32 / self.duration_ms as f32).clamp(0.0, 1.0)
    }

    /// Is the video running (not paused or ended)?
    pub fn is_playing(&self) -> bool {
        matches!(self.state, PlayState::Playing | PlayState::Buffering)
    }

    /// Applies one helper event. Returns true when the viewer should close
    /// its player (playback failed).
    pub fn apply(&mut self, ev: Event) -> bool {
        match ev {
            Event::Ready {
                width,
                height,
                duration_ms,
            } => {
                self.phase = VideoPhase::Playing;
                self.frame_size = (width, height);
                self.duration_ms = duration_ms;
                self.state = PlayState::Playing;
            }
            Event::Frame {
                width,
                height,
                rgba,
            } => {
                self.frame_size = (width, height);
                self.frame = Some(image::Handle::from_rgba(width, height, rgba));
                if self.phase == VideoPhase::Starting {
                    self.phase = VideoPhase::Playing;
                }
            }
            Event::Time {
                position_ms,
                duration_ms,
                state,
            } => {
                if self.seeking.is_none() {
                    self.position_ms = position_ms;
                }
                if duration_ms > 0 {
                    self.duration_ms = duration_ms;
                }
                if state == PlayState::Ended && self.looping && self.seeking.is_none() {
                    self.send(Cmd::Seek(0));
                    self.send(Cmd::Play);
                    self.position_ms = 0;
                    self.state = PlayState::Playing;
                    return false;
                }
                self.state = state;
            }
            Event::Failed(f) => {
                self.phase = VideoPhase::Failed(f.user_message());
                self.player = None;
                return true;
            }
        }
        false
    }

    fn send(&self, cmd: Cmd) {
        if let Some(p) = &self.player {
            p.send(cmd);
        }
    }
}

/// Subscription key: one stream per viewer session.
#[derive(Clone)]
pub struct VideoKey {
    pub(crate) session: u32,
    pub(crate) rx: Shared,
}

impl std::hash::Hash for VideoKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.session.hash(state);
    }
}

/// Events of one viewer session, newest frame wins: when the UI falls
/// behind, older frames are skipped instead of queueing up in memory.
pub fn video_stream(key: &VideoKey) -> futures::stream::BoxStream<'static, Message> {
    let session = key.session;
    let rx = key.rx.lock().ok().and_then(|mut g| g.take());
    let Some(rx) = rx else {
        return futures::stream::empty().boxed();
    };
    futures::stream::unfold(rx, move |mut rx| async move {
        let mut ev = rx.recv().await?;
        if matches!(ev, Event::Frame { .. }) {
            // Skip frames that are already stale.
            while let Ok(next) = rx.try_recv() {
                let keep = matches!(next, Event::Frame { .. });
                if keep {
                    ev = next;
                } else {
                    // A non-frame event: deliver it after this frame by
                    // handling it first; frames are cheap to lose.
                    ev = next;
                    break;
                }
            }
        }
        Some((
            Message::Video(Box::new(VideoMsg { session, event: ev })),
            rx,
        ))
    })
    .boxed()
}

impl App {
    /// Opens a video message in the viewer.
    pub(super) fn open_video_inline(&mut self, id: &str) -> Task<Message> {
        let Some(m) = self.find_message(id).cloned() else {
            return Task::none();
        };
        let origin = self.media_origin(&m);
        let Content::Video { media, gif, .. } = m.content else {
            return Task::none();
        };
        let Some(mref) = media else {
            self.toast("This video is no longer available.");
            return Task::none();
        };
        let Some(client) = self.client.clone() else {
            self.toast("Not connected yet.");
            return Task::none();
        };
        self.video_session = self.video_session.wrapping_add(1);
        let mut view = VideoView::new(id.to_string(), self.video_session);
        view.looping = gif;
        self.video = Some(view);
        self.viewer = None;
        self.popup = None;
        let dir = self.video_dir();
        let session = id.to_string();
        Task::perform(
            fetch_video_file(client, mref, dir, Some(origin)),
            move |result| {
                Message::done(Done::VideoFile {
                    id: session,
                    result,
                })
            },
        )
    }

    /// Videos in the open chat that came without an inline thumbnail get a
    /// poster frame: the (small) file is downloaded into the video cache,
    /// the helper decodes its first picture and stops, and the frame is
    /// stored as the message's thumbnail, so it is extracted only once.
    /// One at a time, newest first.
    pub(super) fn want_poster(&mut self) -> Task<Message> {
        if self.poster_busy {
            return Task::none();
        }
        let Some(client) = self.client.clone() else {
            return Task::none();
        };
        let Some((chat, id, mref, origin)) = self.open.as_ref().and_then(|c| {
            c.messages
                .iter()
                .rev()
                .take(30)
                .find_map(|m| match &m.content {
                    Content::Video {
                        thumbnail: None,
                        media: Some(r),
                        ..
                    } if needs_poster(
                        r.file_length,
                        self.thumbs.contains_key(&m.id),
                        self.posters_tried.contains(&m.id),
                    ) =>
                    {
                        Some((
                            m.chat.clone(),
                            m.id.clone(),
                            r.clone(),
                            self.media_origin(m),
                        ))
                    }
                    _ => None,
                })
        }) else {
            return Task::none();
        };
        self.posters_tried.insert(id.clone());
        self.poster_busy = true;
        // Mock mode: a private temporary directory, removed right after (the
        // viewer's mock directory is deleted when a viewer closes).
        let scratch = self.cfg.mock.then(|| {
            static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            std::env::temp_dir().join(format!("ferrochat-poster-{}-{n}", std::process::id()))
        });
        let dir = scratch.clone().unwrap_or_else(|| self.video_dir());
        Task::perform(
            async move {
                let path = fetch_video_file(client, mref, dir, Some(origin)).await;
                let frame = match &path {
                    Ok(p) => {
                        crate::media::video::first_frame(p, std::time::Duration::from_secs(15))
                            .await
                            .map_err(|f| f.detail())
                    }
                    Err(e) => Err(e.clone()),
                };
                if let Some(d) = scratch {
                    let _ = tokio::task::spawn_blocking(move || std::fs::remove_dir_all(d)).await;
                }
                let (w, h, rgba) = frame?;
                tokio::task::spawn_blocking(move || {
                    crate::media::image::poster_jpeg(w, h, rgba).map_err(|e| e.to_string())
                })
                .await
                .map_err(|e| e.to_string())?
            },
            move |result| Message::done(Done::Poster { chat, id, result }),
        )
    }

    /// A poster frame is ready: its handle is made here (never in `view`)
    /// and it is cached as the message's thumbnail.
    pub(super) fn on_poster(
        &mut self,
        chat: String,
        id: String,
        result: Result<Vec<u8>, String>,
    ) -> Task<Message> {
        self.poster_busy = false;
        let store = match result {
            Ok(jpeg) => {
                self.thumbs.insert(
                    id.clone(),
                    iced::widget::image::Handle::from_bytes(jpeg.clone()),
                );
                if let Some(c) = self.open.as_mut().filter(|c| c.chat == chat)
                    && let Some(m) = c.messages.iter_mut().find(|m| m.id == id)
                    && let Content::Video { thumbnail, .. } = &mut m.content
                {
                    *thumbnail = Some(jpeg.clone());
                }
                super::store_task(&self.store, move |s| s.set_thumb(&chat, &id, &jpeg))
            }
            Err(e) => {
                tracing::warn!(error = %e, "video poster frame failed");
                Task::none()
            }
        };
        Task::batch([store, self.want_poster()])
    }

    /// The downloaded file is ready: start the helper.
    pub(super) fn on_video_file(&mut self, id: String, result: Result<std::path::PathBuf, String>) {
        let Some(v) = self.video.as_mut().filter(|v| v.id == id) else {
            return;
        };
        let path = match result {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(error = %e, "video download failed");
                v.phase = VideoPhase::Failed(
                    "This video could not be downloaded. Check your connection and try again."
                        .into(),
                );
                return;
            }
        };
        match video::spawn(&path) {
            Ok((player, rx)) => {
                if let Ok(mut g) = v.rx.lock() {
                    *g = Some(rx);
                }
                v.player = Some(player);
                v.phase = VideoPhase::Starting;
            }
            Err(f) => {
                tracing::warn!(?f, "video helper unavailable");
                v.phase = VideoPhase::Failed(f.user_message());
            }
        }
    }

    /// A helper event arrived.
    pub(super) fn on_video_event(&mut self, msg: VideoMsg) {
        let Some(v) = self.video.as_mut().filter(|v| v.session == msg.session) else {
            return;
        };
        if let Event::Failed(f) = &msg.event {
            let step = match f {
                Failure::Unavailable(_) => "helper start",
                Failure::MissingCodec(_) => "unsupported codec",
                Failure::Broken(_) => "decode",
            };
            tracing::warn!(step, detail = %f.detail(), "video playback failed");
        }
        v.apply(msg.event);
    }

    /// Stops playback and frees the decoder (the helper process ends).
    pub(super) fn close_video(&mut self) -> Task<Message> {
        let Some(v) = self.video.take() else {
            return Task::none();
        };
        drop(v);
        let leave_fullscreen = if self.video_fullscreen {
            self.video_fullscreen = false;
            iced::window::latest()
                .and_then(|id| iced::window::set_mode(id, iced::window::Mode::Windowed))
        } else {
            Task::none()
        };
        // Mock mode wrote its clip to a temporary directory.
        let cleanup = if self.cfg.mock {
            let dir = self.video_dir();
            blocking(
                move || {
                    let _ = std::fs::remove_dir_all(dir);
                },
                |()| Message::done(Done::Nothing),
            )
        } else {
            Task::none()
        };
        Task::batch([leave_fullscreen, cleanup])
    }

    /// Handles the viewer's controls; `Err(ui)` for everything else.
    pub(super) fn on_ui_video(&mut self, ui: Ui) -> Result<Task<Message>, Ui> {
        Ok(match ui {
            Ui::OpenVideo(id) => self.open_video_inline(&id),
            Ui::CloseVideo => self.close_video(),
            Ui::VideoToggle => {
                if let Some(v) = &mut self.video
                    && v.phase == VideoPhase::Playing
                {
                    if v.is_playing() {
                        v.send(Cmd::Pause);
                        v.state = PlayState::Paused;
                    } else {
                        v.send(Cmd::Play);
                        v.state = PlayState::Playing;
                    }
                }
                Task::none()
            }
            Ui::VideoSeek(f) => {
                if let Some(v) = &mut self.video {
                    v.seeking = Some(f.clamp(0.0, 1.0));
                }
                Task::none()
            }
            Ui::VideoSeekRelease => {
                if let Some(v) = &mut self.video
                    && let Some(f) = v.seeking.take()
                {
                    let ms = video::seek_target(f, v.duration_ms);
                    v.position_ms = ms;
                    v.send(Cmd::Seek(ms));
                    if v.state == PlayState::Ended {
                        v.state = PlayState::Paused;
                    }
                }
                Task::none()
            }
            Ui::VideoVolume(vol) => {
                if let Some(v) = &mut self.video {
                    v.volume = vol.min(100);
                    v.muted = false;
                    v.send(Cmd::Volume(v.volume));
                    v.send(Cmd::Mute(false));
                }
                Task::none()
            }
            Ui::VideoMute => {
                if let Some(v) = &mut self.video {
                    v.muted = !v.muted;
                    v.send(Cmd::Mute(v.muted));
                }
                Task::none()
            }
            Ui::VideoFullscreen => {
                self.video_fullscreen = !self.video_fullscreen;
                let mode = if self.video_fullscreen {
                    iced::window::Mode::Fullscreen
                } else {
                    iced::window::Mode::Windowed
                };
                iced::window::latest().and_then(move |id| iced::window::set_mode(id, mode))
            }
            Ui::OpenVideoExternal(id) => {
                self.popup = None;
                self.open_video_external(&id)
            }
            other => return Err(other),
        })
    }

    /// Directory the decrypted videos are kept in.
    pub(super) fn video_dir(&self) -> std::path::PathBuf {
        if self.cfg.mock {
            // Mock mode writes nothing to the user's directories; the one
            // small clip lives in the temp dir and is removed on close.
            std::env::temp_dir().join(format!("ferrochat-mock-{}", std::process::id()))
        } else {
            self.cfg.cache_dir.join("videos")
        }
    }
}

/// Downloads (or finds in the cache) the decrypted video file.
pub(super) async fn fetch_video_file(
    client: crate::net::ClientHandle,
    mref: crate::net::MediaRef,
    dir: std::path::PathBuf,
    origin: Option<super::media_fetch::MediaOrigin>,
) -> Result<std::path::PathBuf, String> {
    use crate::media::avatar;
    let ext = super::actions::video_extension(&mref.mimetype);
    let name = format!("{}.{ext}", avatar::name_hash(&mref.cache_key()));
    let path = dir.join(&name);
    if !path.is_file() {
        let bytes = super::media_fetch::download(&client, &mref, origin.as_ref()).await?;
        let (d, p) = (dir.clone(), path.clone());
        tokio::task::spawn_blocking(move || {
            crate::config::secure_dir(&d).map_err(|e| e.to_string())?;
            std::fs::write(&p, &bytes).map_err(|e| e.to_string())?;
            avatar::prune(&d, super::MAX_VIDEO_FILES, 400 * 1024 * 1024);
            Ok::<_, String>(())
        })
        .await
        .map_err(|e| e.to_string())??;
    }
    Ok(path)
}

/// Whether a video bubble without an inline thumbnail should get a poster
/// frame: small enough to fetch on its own, not shown yet, not tried yet.
pub fn needs_poster(file_length: u64, has_thumb: bool, tried: bool) -> bool {
    !has_thumb && !tried && file_length > 0 && file_length <= crate::media::MAX_POSTER_VIDEO_BYTES
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posters_only_for_small_unshown_untried_videos() {
        assert!(needs_poster(2_000_000, false, false));
        assert!(!needs_poster(2_000_000, true, false));
        assert!(!needs_poster(2_000_000, false, true));
        assert!(!needs_poster(0, false, false));
        assert!(!needs_poster(
            crate::media::MAX_POSTER_VIDEO_BYTES + 1,
            false,
            false
        ));
    }

    fn view() -> VideoView {
        VideoView::new("m".into(), 1)
    }

    #[test]
    fn events_drive_the_viewer_state() {
        let mut v = view();
        assert_eq!(v.phase, VideoPhase::Downloading);
        v.apply(Event::Ready {
            width: 320,
            height: 180,
            duration_ms: 3000,
        });
        assert_eq!(v.phase, VideoPhase::Playing);
        assert!(v.is_playing());
        v.apply(Event::Frame {
            width: 2,
            height: 1,
            rgba: vec![0; 8],
        });
        assert!(v.frame.is_some());
        v.apply(Event::Time {
            position_ms: 1500,
            duration_ms: 3000,
            state: PlayState::Playing,
        });
        assert!((v.progress() - 0.5).abs() < 1e-6);
        v.apply(Event::Time {
            position_ms: 3000,
            duration_ms: 3000,
            state: PlayState::Ended,
        });
        assert!(!v.is_playing());
        assert!((v.progress() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn a_gif_starts_over_instead_of_ending() {
        let mut v = view();
        v.looping = true;
        v.apply(Event::Ready {
            width: 2,
            height: 2,
            duration_ms: 1000,
        });
        v.apply(Event::Time {
            position_ms: 1000,
            duration_ms: 1000,
            state: PlayState::Ended,
        });
        assert!(v.is_playing());
        assert_eq!(v.position_ms, 0);
        // A plain video stops at the end.
        let mut v = view();
        v.apply(Event::Time {
            position_ms: 1000,
            duration_ms: 1000,
            state: PlayState::Ended,
        });
        assert_eq!(v.state, PlayState::Ended);
    }

    #[test]
    fn dragging_the_slider_wins_over_reported_time() {
        let mut v = view();
        v.duration_ms = 10_000;
        v.seeking = Some(0.25);
        v.apply(Event::Time {
            position_ms: 9_000,
            duration_ms: 10_000,
            state: PlayState::Playing,
        });
        assert!((v.progress() - 0.25).abs() < 1e-6);
        assert_eq!(v.position_ms, 0);
    }

    #[test]
    fn a_failure_shows_the_user_message_and_stops_the_player() {
        let mut v = view();
        let close = v.apply(Event::Failed(Failure::Unavailable("x".into())));
        assert!(close);
        match &v.phase {
            VideoPhase::Failed(m) => assert!(m.contains("could not start (x)")),
            p => panic!("{p:?}"),
        }
    }

    #[test]
    fn unknown_duration_gives_zero_progress() {
        let v = view();
        assert_eq!(v.progress(), 0.0);
    }
}
