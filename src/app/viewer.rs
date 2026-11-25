//! The full-size image viewer, like WhatsApp Desktop's: a dark overlay with
//! the picture fitted to the window, wheel / +/- zoom, drag to pan, Esc or a
//! click outside the picture closes it, Left / Right step through the
//! chat's photos, and "Save as…" / "Open externally".
//!
//! The picture is the full-resolution file. When the auto-download cache
//! already holds it (photos up to `MAX_AUTO_IMAGE_PIXELS`), its handle is
//! shared; otherwise the viewer downloads it into its own slot, which is
//! dropped when the viewer closes or moves on, so a 40 MP photo never stays
//! in memory after it was looked at. Handles are made in `update`, never in
//! `view`.

use std::path::PathBuf;
use std::sync::Arc;

use iced::widget::image;
use iced::{Task, Vector};

use super::message::{Done, Message, Ui};
use super::{App, blocking};
use crate::net::Content;

/// Smallest zoom (the fitted picture).
pub const MIN_ZOOM: f32 = 1.0;
/// Largest zoom.
pub const MAX_ZOOM: f32 = 8.0;
/// One wheel notch / "+" key.
pub const ZOOM_STEP: f32 = 1.25;

/// State of the open viewer.
#[derive(Debug, Clone)]
pub struct ImageViewer {
    /// Message shown.
    pub id: String,
    /// A full-resolution picture downloaded for the viewer only (freed on
    /// close). `None` while loading or when the shared cache has it.
    pub own: Option<image::Handle>,
    /// The download failed.
    pub failed: bool,
    /// 1.0 = fitted to the window.
    pub zoom: f32,
    /// Offset of the picture's centre from the window centre, in pixels.
    pub pan: Vector,
}

impl ImageViewer {
    /// A fresh viewer on `id`.
    pub fn new(id: String) -> Self {
        Self {
            id,
            own: None,
            failed: false,
            zoom: MIN_ZOOM,
            pan: Vector::ZERO,
        }
    }

    /// Zooms by `factor` keeping the point `at` (relative to the window
    /// centre) under the cursor. Back at 1.0 the picture is centred again.
    pub fn zoom_by(&mut self, factor: f32, at: Vector) {
        let z = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        let k = z / self.zoom;
        // p_screen = centre + pan + p_img * zoom: keep `at` fixed.
        self.pan = at + (self.pan - at) * k;
        self.zoom = z;
        if self.zoom <= MIN_ZOOM + f32::EPSILON {
            self.pan = Vector::ZERO;
        }
    }

    /// Drags the zoomed picture; at 1.0 it stays centred.
    pub fn pan_by(&mut self, d: Vector) {
        if self.zoom > MIN_ZOOM {
            self.pan += d;
        }
    }

    /// Keeps at least part of the picture (`size` = fitted size at zoom 1)
    /// in a window of `win`: it can never be dragged out of sight.
    pub fn clamp_pan(&mut self, size: (f32, f32), win: (f32, f32)) {
        let max_x = ((size.0 * self.zoom - win.0) / 2.0).max(0.0) + win.0 * 0.25;
        let max_y = ((size.1 * self.zoom - win.1) / 2.0).max(0.0) + win.1 * 0.25;
        self.pan.x = self.pan.x.clamp(-max_x, max_x);
        self.pan.y = self.pan.y.clamp(-max_y, max_y);
        if self.zoom <= MIN_ZOOM + f32::EPSILON {
            self.pan = Vector::ZERO;
        }
    }
}

/// `(w, h)` scaled to fit inside `(max_w, max_h)` (up or down), keeping
/// the aspect ratio. Unknown sizes read as 4:3.
pub fn fit(w: u32, h: u32, max_w: f32, max_h: f32) -> (f32, f32) {
    let (w, h) = if w == 0 || h == 0 {
        (4.0, 3.0)
    } else {
        (w as f32, h as f32)
    };
    let s = (max_w / w).min(max_h / h).max(0.0);
    (w * s, h * s)
}

/// The photo before (`delta` -1) or after (+1) `current` among `ids`
/// (oldest first), if any.
pub fn step(ids: &[String], current: &str, delta: i32) -> Option<String> {
    let i = ids.iter().position(|x| x == current)? as i64 + i64::from(delta);
    usize::try_from(i).ok().and_then(|i| ids.get(i)).cloned()
}

/// The encoded bytes behind a handle made with `from_bytes`.
pub fn handle_bytes(h: &image::Handle) -> Option<Vec<u8>> {
    match h {
        image::Handle::Bytes(_, b) => Some(b.to_vec()),
        _ => None,
    }
}

/// File extension for an image MIME type.
pub fn image_extension(mime: &str) -> &'static str {
    match mime.split(';').next().unwrap_or("").trim() {
        "image/png" => "png",
        "image/webp" => "webp",
        "image/gif" => "gif",
        _ => "jpg",
    }
}

impl App {
    /// The photos of the open chat, oldest first.
    pub fn viewer_ids(&self) -> Vec<String> {
        self.open
            .iter()
            .flat_map(|c| c.messages.iter())
            .filter(|m| matches!(m.content, Content::Image { .. }))
            .map(|m| m.id.clone())
            .collect()
    }

    /// The full picture for the open viewer: the shared cache's or our own.
    pub fn viewer_handle(&self) -> Option<&image::Handle> {
        let v = self.viewer.as_ref()?;
        v.own.as_ref().or_else(|| {
            self.find_message(&v.id)
                .and_then(|m| m.content.media())
                .and_then(|r| self.images.peek(&r.cache_key()))
        })
    }

    /// Opens the viewer on a photo (or moves it to another one).
    pub(super) fn open_viewer(&mut self, id: String) -> Task<Message> {
        self.popup = None;
        self.emoji_open = false;
        self.viewer = Some(ImageViewer::new(id.clone()));
        let Some((mref, origin)) = self
            .find_message(&id)
            .and_then(|m| m.content.media().map(|r| (r.clone(), self.media_origin(m))))
        else {
            return Task::none();
        };
        if self.images.contains(&mref.cache_key()) {
            return Task::none();
        }
        let Some(client) = self.client.clone() else {
            return Task::none();
        };
        let max = crate::media::MAX_IMAGE_PIXELS;
        Task::perform(
            async move {
                let bytes = super::media_fetch::download(&client, &mref, Some(&origin)).await?;
                // Header-only probe: refuse decompression bombs before iced
                // decodes the picture.
                tokio::task::spawn_blocking(move || {
                    crate::media::image::check_image_bounds_max(&bytes, max)
                        .map(|_| Arc::new(bytes))
                        .map_err(|e| e.to_string())
                })
                .await
                .map_err(|e| e.to_string())?
            },
            move |result| Message::done(Done::ViewerImage { id, result }),
        )
    }

    /// The viewer's own download finished.
    pub(super) fn on_viewer_image(&mut self, id: String, result: Result<Arc<Vec<u8>>, String>) {
        let Some(v) = self.viewer.as_mut().filter(|v| v.id == id) else {
            // Closed or moved on meanwhile: the bytes are just dropped.
            return;
        };
        match result {
            Ok(bytes) => {
                let owned = Arc::try_unwrap(bytes).unwrap_or_else(|a| (*a).clone());
                v.own = Some(image::Handle::from_bytes(owned));
            }
            Err(e) => {
                tracing::warn!(error = %e, "full image download failed");
                v.failed = true;
            }
        }
    }

    /// Viewer controls; `Err(ui)` for everything else.
    pub(super) fn on_ui_viewer(&mut self, ui: Ui) -> Result<Task<Message>, Ui> {
        Ok(match ui {
            Ui::OpenImage(id) => self.open_viewer(id),
            Ui::CloseViewer => {
                // Drops our own full-size copy.
                self.viewer = None;
                Task::none()
            }
            Ui::ViewerZoom { factor, at } => {
                if let Some(v) = &mut self.viewer {
                    v.zoom_by(factor, at);
                }
                Task::none()
            }
            Ui::ViewerResetZoom => {
                if let Some(v) = &mut self.viewer {
                    v.zoom = MIN_ZOOM;
                    v.pan = Vector::ZERO;
                }
                Task::none()
            }
            Ui::ViewerPan { delta, fitted } => {
                let win = self.window_size();
                if let Some(v) = &mut self.viewer {
                    v.pan_by(delta);
                    v.clamp_pan(fitted, (win.width, win.height));
                }
                Task::none()
            }
            Ui::ViewerStep(delta) => {
                let Some(cur) = self.viewer.as_ref().map(|v| v.id.clone()) else {
                    return Err(Ui::ViewerStep(delta));
                };
                match step(&self.viewer_ids(), &cur, delta) {
                    Some(next) => self.open_viewer(next),
                    None => Task::none(),
                }
            }
            Ui::ViewerSave => self.viewer_save(),
            Ui::ViewerOpenExternal => self.viewer_open_external(),
            other => return Err(other),
        })
    }

    fn viewer_file(&self) -> Option<(Vec<u8>, String)> {
        let v = self.viewer.as_ref()?;
        let bytes = handle_bytes(self.viewer_handle()?)?;
        let mime = self
            .find_message(&v.id)
            .and_then(|m| m.content.media())
            .map(|r| r.mimetype.clone())
            .unwrap_or_default();
        let ts = self
            .find_message(&v.id)
            .map(|m| m.timestamp)
            .unwrap_or_default();
        let stamp = chrono::DateTime::from_timestamp(ts, 0)
            .map(|d| d.format("%Y-%m-%d at %H.%M.%S").to_string())
            .unwrap_or_else(|| "photo".into());
        Some((
            bytes,
            format!("WhatsApp Image {stamp}.{}", image_extension(&mime)),
        ))
    }

    /// "Save as…": the system save dialog, then the original bytes.
    fn viewer_save(&mut self) -> Task<Message> {
        let Some((bytes, name)) = self.viewer_file() else {
            self.toast("The photo is still loading.");
            return Task::none();
        };
        let dir = self.cfg.downloads_dir.clone();
        Task::perform(
            async move {
                let Some(file) = rfd::AsyncFileDialog::new()
                    .set_title("Save photo")
                    .set_directory(&dir)
                    .set_file_name(&name)
                    .save_file()
                    .await
                else {
                    return Ok(None);
                };
                let path: PathBuf = file.path().to_path_buf();
                tokio::task::spawn_blocking(move || {
                    std::fs::write(&path, &bytes).map(|_| Some(path))
                })
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())
            },
            |r| Message::done(Done::ViewerSaved(r)),
        )
    }

    /// "Open externally": the photo goes to the downloads folder and the
    /// system image viewer opens it.
    fn viewer_open_external(&mut self) -> Task<Message> {
        let Some((bytes, name)) = self.viewer_file() else {
            self.toast("The photo is still loading.");
            return Task::none();
        };
        if self.cfg.mock {
            self.toast("Mock mode: nothing is written to disk.");
            return Task::none();
        }
        let dir = self.cfg.downloads_dir.clone();
        blocking(
            move || {
                std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                let path = super::unique_path(&dir, &name);
                std::fs::write(&path, &bytes).map_err(|e| e.to_string())?;
                crate::notify::open_path(&path).map_err(|e| e.to_string())?;
                Ok(Some(path))
            },
            |r| Message::done(Done::ViewerSaved(r)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_is_clamped_and_keeps_the_point_under_the_cursor() {
        let mut v = ImageViewer::new("a".into());
        v.zoom_by(0.5, Vector::ZERO);
        assert_eq!(v.zoom, MIN_ZOOM);
        let at = Vector::new(100.0, -50.0);
        v.zoom_by(2.0, at);
        assert_eq!(v.zoom, 2.0);
        // The picture point under `at` (at zoom 1, pan 0: `at` itself)
        // is still under it: (at - pan) / zoom == at.
        let p = (at - v.pan) * (1.0 / v.zoom);
        assert!((p.x - at.x).abs() < 0.01 && (p.y - at.y).abs() < 0.01);
        for _ in 0..50 {
            v.zoom_by(ZOOM_STEP, Vector::ZERO);
        }
        assert_eq!(v.zoom, MAX_ZOOM);
        // Back to fit: centred again.
        for _ in 0..50 {
            v.zoom_by(1.0 / ZOOM_STEP, at);
        }
        assert_eq!(v.zoom, MIN_ZOOM);
        assert_eq!(v.pan, Vector::ZERO);
    }

    #[test]
    fn pan_only_when_zoomed_and_never_out_of_sight() {
        let mut v = ImageViewer::new("a".into());
        v.pan_by(Vector::new(50.0, 0.0));
        assert_eq!(v.pan, Vector::ZERO);
        v.zoom = 2.0;
        v.pan_by(Vector::new(10_000.0, -10_000.0));
        v.clamp_pan((800.0, 600.0), (1000.0, 700.0));
        // (1600 - 1000) / 2 + 250 = 550; (1200 - 700) / 2 + 175 = 425.
        assert_eq!(v.pan, Vector::new(550.0, -425.0));
    }

    #[test]
    fn fitting_and_stepping() {
        assert_eq!(fit(4000, 3000, 800.0, 800.0), (800.0, 600.0));
        assert_eq!(fit(100, 200, 800.0, 800.0), (400.0, 800.0));
        assert_eq!(fit(0, 0, 400.0, 400.0), (400.0, 300.0));
        let ids: Vec<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
        assert_eq!(step(&ids, "b", 1).as_deref(), Some("c"));
        assert_eq!(step(&ids, "b", -1).as_deref(), Some("a"));
        assert_eq!(step(&ids, "a", -1), None);
        assert_eq!(step(&ids, "c", 1), None);
        assert_eq!(step(&ids, "x", 1), None);
        assert_eq!(image_extension("image/png"), "png");
        assert_eq!(image_extension("image/jpeg"), "jpg");
        let h = image::Handle::from_bytes(vec![1u8, 2, 3]);
        assert_eq!(handle_bytes(&h), Some(vec![1, 2, 3]));
    }
}
