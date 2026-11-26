//! Paste-to-send. Ctrl+V in the composer with a copied image, or with
//! files copied in a file manager, opens the attachment preview like
//! WhatsApp Desktop. Text paste stays the text input's own: this module
//! only acts when the clipboard holds no text.
//!
//! The clipboard is read with `arboard` on a blocking thread
//! ([`read_clipboard`]); the rest is pure and unit-tested.

use std::path::{Path, PathBuf};

use iced::keyboard::{Key, Modifiers, key::Named, key::Physical};
use iced::widget::image;

use crate::media::image as media_image;
use crate::net::OutgoingMedia;

/// Most files taken from one paste (or one pick).
pub const MAX_PASTED_FILES: usize = 10;

/// Whether a key press is a paste shortcut: Ctrl+V (Cmd+V on macOS, also
/// with Shift; non-Latin layouts by the physical key) or Shift+Insert.
pub fn is_paste_shortcut(key: &Key, physical: Physical, modifiers: Modifiers) -> bool {
    match key {
        Key::Named(Named::Insert) => modifiers.shift() && !modifiers.command(),
        Key::Character(_) => {
            modifiers.command()
                && !modifiers.alt()
                && key
                    .to_latin(physical)
                    .is_some_and(|c| c.eq_ignore_ascii_case(&'v'))
        }
        _ => false,
    }
}

/// Raw pixels of a copied image.
#[derive(Clone, PartialEq, Eq)]
pub struct RawImage {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

impl std::fmt::Debug for RawImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RawImage({}x{})", self.width, self.height)
    }
}

/// What a paste carries, as far as sending media is concerned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Clipboard {
    /// Files copied in a file manager (`text/uri-list`, `CF_HDROP`).
    Files(Vec<PathBuf>),
    /// A copied image (screenshot, "Copy image" in a browser).
    Image(RawImage),
    /// Text or nothing: the text input handles it.
    Text,
}

/// Decides what a paste means. Files win (file managers also offer the
/// paths as text); then a text that is only `file://` URIs of existing
/// files; then any other text stays a text paste, so an office suite that
/// offers both a text and a picture of the copied cells pastes text; an
/// image is only taken when there is no text at all. The image is fetched
/// lazily because decoding it is the expensive part.
pub fn classify(
    files: Vec<PathBuf>,
    text: Option<&str>,
    exists: impl Fn(&Path) -> bool,
    image: impl FnOnce() -> Option<RawImage>,
) -> Clipboard {
    if !files.is_empty() {
        return Clipboard::Files(files);
    }
    if let Some(text) = text.filter(|t| !t.trim().is_empty()) {
        return match parse_uri_list(text) {
            Some(paths) if paths.iter().all(|p| exists(p)) => Clipboard::Files(paths),
            _ => Clipboard::Text,
        };
    }
    match image() {
        Some(img) if img.width > 0 && img.height > 0 => Clipboard::Image(img),
        _ => Clipboard::Text,
    }
}

/// Parses a `text/uri-list` (RFC 2483): one URI per line, `#` comments.
/// `Some` only when every entry is a local `file://` URI, so ordinary text
/// that merely contains a URI is never mistaken for files.
pub fn parse_uri_list(text: &str) -> Option<Vec<PathBuf>> {
    let mut out = Vec::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        out.push(file_uri_to_path(line)?);
    }
    (!out.is_empty()).then_some(out)
}

/// `file:///a%20b` or `file://localhost/a%20b` → `/a b`. Other hosts and
/// schemes are not local files.
pub fn file_uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri
        .get(..7)
        .filter(|s| s.eq_ignore_ascii_case("file://"))
        .and_then(|_| uri.get(7..))?;
    let path = if rest.starts_with('/') {
        rest
    } else {
        let (host, path) = rest.split_at(rest.find('/')?);
        if !host.eq_ignore_ascii_case("localhost") {
            return None;
        }
        path
    };
    let bytes = percent_decode(path)?;
    if bytes.contains(&0) {
        return None;
    }
    bytes_to_path(bytes)
}

fn percent_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len());
    let mut it = s.bytes();
    while let Some(b) = it.next() {
        if b == b'%' {
            let hi = (it.next()? as char).to_digit(16)?;
            let lo = (it.next()? as char).to_digit(16)?;
            out.push((hi * 16 + lo) as u8);
        } else {
            out.push(b);
        }
    }
    Some(out)
}

#[cfg(unix)]
fn bytes_to_path(bytes: Vec<u8>) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    Some(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
}

#[cfg(not(unix))]
fn bytes_to_path(bytes: Vec<u8>) -> Option<PathBuf> {
    let s = String::from_utf8(bytes).ok()?;
    // `/C:/Users/x` → `C:/Users/x`.
    let s = match s.as_bytes() {
        [b'/', d, b':', ..] if d.is_ascii_alphabetic() => s[1..].to_string(),
        _ => s,
    };
    Some(PathBuf::from(s))
}

/// Percent-encodes a path the way file managers write `file://` URIs.
fn percent_encode(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for b in path.bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The text input pasted the clipboard's text flavour too: plain paths
/// (Nautilus) or `file://` URIs (Dolphin). Removes those from the draft.
pub fn strip_pasted_paths(draft: &str, paths: &[PathBuf]) -> Option<String> {
    let mut text = draft.to_string();
    for p in paths {
        let plain = p.display().to_string();
        let variants = [
            format!("file://{}", percent_encode(&plain)),
            format!("file://{plain}"),
            plain,
        ];
        for v in variants.iter().filter(|v| !v.is_empty()) {
            text = text.replace(v.as_str(), "");
        }
    }
    (text != draft).then(|| text.trim().to_string())
}

/// A picked file that should go as a video once the helper has read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingVideo {
    /// Position in [`Staged::items`] (staged as a document until then).
    pub index: usize,
    pub path: PathBuf,
    /// A GIF: converted to a silent MP4 first.
    pub gif: bool,
}

/// Whether a file goes as a video: `Some(false)` for MP4, `Some(true)` for
/// a GIF (converted), `None` for anything else (other containers stay
/// documents: the helper only reads MP4 and WhatsApp only plays MP4).
pub fn video_kind(path: &Path) -> Option<bool> {
    match media_image::mime_for_path(path) {
        "video/mp4" => Some(false),
        "image/gif" => Some(true),
        _ => None,
    }
}

/// Media ready for the preview, built off the UI thread.
#[derive(Debug, Clone, Default)]
pub struct Staged {
    pub items: Vec<OutgoingMedia>,
    /// Items still to be probed as videos ([`finish_videos`]).
    pub videos: Vec<PendingVideo>,
    /// Preview pixels of the first item when it is a photo.
    pub preview: Option<(u32, u32, Vec<u8>)>,
    /// Paths to strip from the draft (the text input pasted them as text).
    pub pasted_paths: Vec<PathBuf>,
    /// Files that could not be attached, with the reason.
    pub problems: Vec<String>,
}

/// Reads one file the way the attach button does: size-checked first, then
/// a photo (JPEG / PNG / WebP that decodes) or a document. Blocking.
pub fn prepare_path(path: &Path) -> Result<OutgoingMedia, String> {
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err("Folders can't be sent.".into());
    }
    if meta.len() == 0 {
        return Err("Empty files can't be sent.".into());
    }
    if meta.len() > crate::media::MAX_DOWNLOAD_BYTES {
        return Err("Files over 100 MB cannot be sent.".to_string());
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    Ok(media_image::outgoing_from_file(path, bytes, None))
}

impl Staged {
    /// Prepares files (attach button or pasted from a file manager).
    pub fn from_paths(mut paths: Vec<PathBuf>, pasted: bool) -> Self {
        let mut problems = Vec::new();
        if paths.len() > MAX_PASTED_FILES {
            problems.push(format!(
                "Only {MAX_PASTED_FILES} files can be sent at once."
            ));
            paths.truncate(MAX_PASTED_FILES);
        }
        let mut items = Vec::new();
        let mut videos = Vec::new();
        for p in &paths {
            match prepare_path(p) {
                Ok(m) => {
                    if let Some(gif) = video_kind(p) {
                        videos.push(PendingVideo {
                            index: items.len(),
                            path: p.clone(),
                            gif,
                        });
                    }
                    items.push(m);
                }
                Err(e) => {
                    let name = p
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    problems.push(format!("{name}: {e}"));
                }
            }
        }
        let mut s = Self::finish(items, if pasted { paths } else { Vec::new() }, problems);
        s.videos = videos;
        s
    }

    /// Prepares a copied image.
    pub fn from_image(img: &RawImage) -> Self {
        match media_image::outgoing_from_rgba(img.width, img.height, &img.rgba) {
            Ok(m) => Self::finish(vec![m], Vec::new(), Vec::new()),
            Err(_) => Self::finish(
                Vec::new(),
                Vec::new(),
                vec!["The copied image could not be read.".into()],
            ),
        }
    }

    fn finish(
        items: Vec<OutgoingMedia>,
        pasted_paths: Vec<PathBuf>,
        problems: Vec<String>,
    ) -> Self {
        let preview = items.first().and_then(media_image::preview_rgba);
        Self {
            items,
            videos: Vec::new(),
            preview,
            pasted_paths,
            problems,
        }
    }

    /// Applies what the helper read from a pending video: the item becomes
    /// a video (GIFs take the converted MP4's bytes), or stays a document
    /// with a note when the file could not be read.
    pub fn apply_probe(
        &mut self,
        pending: &PendingVideo,
        result: Result<(crate::media::video::Probe, Option<Vec<u8>>), String>,
    ) {
        let Some(item) = self.items.get_mut(pending.index) else {
            return;
        };
        let probed = result.and_then(|(probe, converted)| {
            let (fw, fh, rgba) = probe.frame.clone();
            let thumbnail =
                media_image::thumbnail_from_rgba(fw, fh, rgba).map_err(|e| e.to_string())?;
            Ok((probe, converted, thumbnail))
        });
        match probed {
            Ok((probe, converted, thumbnail)) => {
                if let Some(bytes) = converted {
                    item.bytes = bytes;
                    item.file_name = mp4_name(&item.file_name);
                }
                item.mimetype = "video/mp4".into();
                item.kind = video_from_probe(&probe, thumbnail, pending.gif);
                if pending.index == 0 {
                    self.preview = Some(probe.frame);
                }
            }
            Err(e) => {
                tracing::info!(error = %e, gif = pending.gif, "video not readable, sent as a file");
                self.problems.push(format!(
                    "{}: can't be played here, it will be sent as a file.",
                    item.file_name
                ));
            }
        }
    }
}

/// `funny.gif` → `funny.mp4`.
fn mp4_name(name: &str) -> String {
    let stem = Path::new(name)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "video".into());
    format!("{stem}.mp4")
}

/// The [`OutgoingKind`](crate::net::OutgoingKind) of a probed video.
pub fn video_from_probe(
    probe: &crate::media::video::Probe,
    thumbnail: Vec<u8>,
    gif: bool,
) -> crate::net::OutgoingKind {
    crate::net::OutgoingKind::Video {
        seconds: ((probe.duration_ms + 500) / 1000) as u32,
        width: probe.width,
        height: probe.height,
        thumbnail,
        gif,
    }
}

/// Most time the helper gets to convert a GIF or read a video.
const VIDEO_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// Reads every pending video with the helper (GIFs are converted to MP4
/// first, in a private temporary file removed right after). Async: the
/// helper runs as a child process, never on the blocking pool.
pub async fn finish_videos(mut staged: Staged) -> Staged {
    static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    for pending in std::mem::take(&mut staged.videos) {
        let result = async {
            if !pending.gif {
                let probe = crate::media::video::probe(&pending.path, VIDEO_WAIT)
                    .await
                    .map_err(|f| f.detail())?;
                return Ok((probe, None));
            }
            let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let tmp =
                std::env::temp_dir().join(format!("ferrochat-gif-{}-{n}.mp4", std::process::id()));
            let out = async {
                crate::media::video::gif_to_mp4(&pending.path, &tmp, VIDEO_WAIT)
                    .await
                    .map_err(|f| f.detail())?;
                let probe = crate::media::video::probe(&tmp, VIDEO_WAIT)
                    .await
                    .map_err(|f| f.detail())?;
                let bytes = tokio::fs::read(&tmp).await.map_err(|e| e.to_string())?;
                Ok((probe, Some(bytes)))
            }
            .await;
            let _ = tokio::fs::remove_file(&tmp).await;
            out
        }
        .await;
        staged.apply_probe(&pending, result);
    }
    staged
}

/// Prepares picked, pasted or dropped files, videos included. Async.
pub async fn stage_paths(paths: Vec<PathBuf>, pasted: bool) -> Option<Staged> {
    let staged = tokio::task::spawn_blocking(move || Staged::from_paths(paths, pasted))
        .await
        .ok()?;
    Some(finish_videos(staged).await)
}

/// Turns what the clipboard holds into staged media; `None` for a text
/// paste. Blocking (file reads, encoding); videos still need
/// [`finish_videos`].
pub fn stage(clip: Clipboard) -> Option<Staged> {
    match clip {
        Clipboard::Files(paths) => Some(Staged::from_paths(paths, true)),
        Clipboard::Image(img) => Some(Staged::from_image(&img)),
        Clipboard::Text => None,
    }
}

/// Reads files or an image from the system clipboard (X11, Wayland
/// data-control, Windows). Blocking: call it from a `Task`.
pub fn read_clipboard() -> Clipboard {
    let Ok(mut cb) = arboard::Clipboard::new() else {
        return Clipboard::Text;
    };
    let files = cb.get().file_list().unwrap_or_default();
    let text = if files.is_empty() {
        cb.get().text().ok()
    } else {
        None
    };
    classify(files, text.as_deref(), Path::is_file, || {
        cb.get().image().ok().map(|i| RawImage {
            width: i.width,
            height: i.height,
            rgba: i.bytes.into_owned(),
        })
    })
}

/// The attachment preview that replaces the conversation until the media
/// is sent or discarded.
#[derive(Debug)]
pub struct AttachPreview {
    /// Chat it will be sent to.
    pub chat: String,
    /// What will be sent, in order; the caption goes with the first.
    pub items: Vec<OutgoingMedia>,
    /// Preview of the first item (photos), made once in `update`.
    pub handle: Option<image::Handle>,
    pub caption: String,
}

impl AttachPreview {
    /// Builds the preview; the image handle is created here, in `update`,
    /// never in `view`. `None` when nothing could be prepared.
    pub fn new(chat: String, staged: Staged) -> Option<Self> {
        if staged.items.is_empty() {
            return None;
        }
        let handle = staged
            .preview
            .map(|(w, h, px)| image::Handle::from_rgba(w, h, px));
        Some(Self {
            chat,
            items: staged.items,
            handle,
            caption: String::new(),
        })
    }

    /// The media to send: the trimmed caption, if any, on the first item.
    pub fn into_outgoing(self) -> Vec<OutgoingMedia> {
        let caption = self.caption.trim().to_string();
        let mut items = self.items;
        if let Some(first) = items.first_mut()
            && !caption.is_empty()
        {
            first.caption = Some(caption);
        }
        items
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::OutgoingKind;
    use iced::keyboard::key::{Code, NativeCode};

    fn ch(c: &str) -> Key {
        Key::Character(c.into())
    }

    #[test]
    fn paste_shortcuts() {
        let v = Physical::Code(Code::KeyV);
        let ctrl = Modifiers::CTRL;
        assert!(is_paste_shortcut(&ch("v"), v, ctrl));
        assert!(is_paste_shortcut(&ch("V"), v, ctrl | Modifiers::SHIFT));
        // Russian layout: the "v" key types "м".
        assert!(is_paste_shortcut(&ch("м"), v, ctrl));
        assert!(is_paste_shortcut(
            &Key::Named(Named::Insert),
            Physical::Code(Code::Insert),
            Modifiers::SHIFT
        ));
        // Not a paste.
        assert!(!is_paste_shortcut(&ch("v"), v, Modifiers::empty()));
        assert!(!is_paste_shortcut(
            &ch("c"),
            Physical::Code(Code::KeyC),
            ctrl
        ));
        assert!(!is_paste_shortcut(&ch("v"), v, ctrl | Modifiers::ALT));
        assert!(!is_paste_shortcut(
            &ch("м"),
            Physical::Unidentified(NativeCode::Unidentified),
            ctrl
        ));
        assert!(!is_paste_shortcut(
            &Key::Named(Named::Insert),
            Physical::Code(Code::Insert),
            ctrl
        ));
    }

    fn img() -> Option<RawImage> {
        Some(RawImage {
            width: 1,
            height: 1,
            rgba: vec![0, 0, 0, 255],
        })
    }

    #[test]
    fn text_paste_stays_text_and_images_need_an_empty_text() {
        let yes = |_: &Path| true;
        // Plain text, even when an image flavour is offered too.
        assert_eq!(classify(vec![], Some("hola"), yes, img), Clipboard::Text);
        // Text that only mentions a URI is still text.
        assert_eq!(
            classify(vec![], Some("see file:///etc/hosts"), yes, img),
            Clipboard::Text
        );
        // Nothing usable.
        assert_eq!(classify(vec![], None, yes, || None), Clipboard::Text);
        // Image only (a screenshot).
        assert!(matches!(
            classify(vec![], None, yes, img),
            Clipboard::Image(_)
        ));
        assert!(matches!(
            classify(vec![], Some("  "), yes, img),
            Clipboard::Image(_)
        ));
        // A zero-sized image is not an image.
        let empty = || {
            Some(RawImage {
                width: 0,
                height: 0,
                rgba: vec![],
            })
        };
        assert_eq!(classify(vec![], None, yes, empty), Clipboard::Text);
    }

    #[test]
    fn files_win_and_uri_text_needs_existing_files() {
        let f = vec![PathBuf::from("/tmp/a.png")];
        let lazy = || -> Option<RawImage> { panic!("image must not be read") };
        assert_eq!(
            classify(f.clone(), Some("ignored"), |_| true, lazy),
            Clipboard::Files(f.clone())
        );
        assert_eq!(
            classify(vec![], Some("file:///tmp/a.png\n"), |_| true, lazy),
            Clipboard::Files(f)
        );
        assert_eq!(
            classify(vec![], Some("file:///tmp/a.png"), |_| false, lazy),
            Clipboard::Text
        );
    }

    #[cfg(unix)]
    #[test]
    fn uri_lists() {
        assert_eq!(
            parse_uri_list(
                "# comment\r\nfile:///home/u/My%20Pic.png\r\nfile://localhost/tmp/b.jpg\r\n"
            ),
            Some(vec![
                PathBuf::from("/home/u/My Pic.png"),
                PathBuf::from("/tmp/b.jpg")
            ])
        );
        assert_eq!(
            parse_uri_list("FILE:///tmp/%C3%B1.png"),
            Some(vec![PathBuf::from("/tmp/ñ.png")])
        );
        // Non-UTF-8 bytes survive on Unix.
        use std::os::unix::ffi::OsStrExt;
        let p = parse_uri_list("file:///tmp/%FF").unwrap();
        assert_eq!(p[0].as_os_str().as_bytes(), b"/tmp/\xFF");
        // Not a local file list.
        assert_eq!(parse_uri_list(""), None);
        assert_eq!(parse_uri_list("# only a comment"), None);
        assert_eq!(parse_uri_list("https://example.com/a.png"), None);
        assert_eq!(parse_uri_list("file://server/share/a.png"), None);
        assert_eq!(parse_uri_list("file:///tmp/a.png\nhello"), None);
        assert_eq!(parse_uri_list("file:///tmp/%zz"), None);
        assert_eq!(parse_uri_list("file:///tmp/%2"), None);
        assert_eq!(parse_uri_list("file:///tmp/%00x"), None);
        assert_eq!(parse_uri_list("/tmp/a.png"), None);
        assert_eq!(parse_uri_list("file:/"), None);
        assert_eq!(parse_uri_list("fileé"), None);
    }

    #[test]
    fn pasted_path_text_is_removed_from_the_draft() {
        let paths = [PathBuf::from("/home/u/My Pic.png")];
        assert_eq!(
            strip_pasted_paths("hola file:///home/u/My%20Pic.png", &paths).as_deref(),
            Some("hola")
        );
        assert_eq!(
            strip_pasted_paths("/home/u/My Pic.png", &paths).as_deref(),
            Some("")
        );
        assert_eq!(
            strip_pasted_paths("file:///home/u/My Pic.png ", &paths).as_deref(),
            Some("")
        );
        assert_eq!(strip_pasted_paths("hola", &paths), None, "untouched");
    }

    #[test]
    fn copied_images_and_files_are_staged_like_the_attach_button() {
        let s = stage(Clipboard::Image(RawImage {
            width: 4,
            height: 2,
            rgba: vec![10; 32],
        }))
        .unwrap();
        assert_eq!(s.items.len(), 1);
        assert_eq!(s.items[0].mimetype, "image/jpeg");
        assert_eq!(s.preview.as_ref().map(|p| (p.0, p.1)), Some((4, 2)));
        assert!(stage(Clipboard::Text).is_none());
        let bad = stage(Clipboard::Image(RawImage {
            width: 4,
            height: 2,
            rgba: vec![10; 3],
        }))
        .unwrap();
        assert!(bad.items.is_empty() && bad.problems.len() == 1);

        let base = std::env::temp_dir().join(format!("ferrochat-paste-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let dir = Cleanup(base);
        let dir = &dir.0;
        let photo = dir.as_path().join("cat.png");
        ::image::RgbImage::new(8, 6).save(&photo).unwrap();
        let doc = dir.as_path().join("notes.txt");
        std::fs::write(&doc, b"hi").unwrap();
        let empty = dir.as_path().join("empty.txt");
        std::fs::write(&empty, b"").unwrap();
        let s = stage(Clipboard::Files(vec![
            photo.clone(),
            doc.clone(),
            empty,
            dir.as_path().to_path_buf(),
            dir.as_path().join("missing.png"),
        ]))
        .unwrap();
        assert_eq!(s.items.len(), 2);
        assert!(matches!(
            s.items[0].kind,
            OutgoingKind::Image { width: 8, .. }
        ));
        assert_eq!(s.items[1].kind, OutgoingKind::Document);
        assert_eq!(s.problems.len(), 3);
        assert_eq!(s.pasted_paths.len(), 5);
        assert!(s.preview.is_some());
        let many = vec![doc; MAX_PASTED_FILES + 3];
        let s = Staged::from_paths(many, false);
        assert_eq!(s.items.len(), MAX_PASTED_FILES);
        assert_eq!(s.problems.len(), 1);
        assert!(s.pasted_paths.is_empty());
    }

    #[test]
    fn the_caption_goes_with_the_first_item() {
        let staged = stage(Clipboard::Image(RawImage {
            width: 2,
            height: 2,
            rgba: vec![255; 16],
        }))
        .unwrap();
        let mut p = AttachPreview::new("chat".into(), staged).unwrap();
        assert!(p.handle.is_some());
        p.caption = "  look  ".into();
        let out = p.into_outgoing();
        assert_eq!(out[0].caption.as_deref(), Some("look"));
        assert!(AttachPreview::new("chat".into(), Staged::default()).is_none());
    }

    #[test]
    fn only_mp4_and_gif_go_as_videos() {
        assert_eq!(video_kind(Path::new("/a/b.MP4")), Some(false));
        assert_eq!(video_kind(Path::new("/a/b.m4v")), Some(false));
        assert_eq!(video_kind(Path::new("/a/b.gif")), Some(true));
        assert_eq!(video_kind(Path::new("/a/b.mov")), None);
        assert_eq!(video_kind(Path::new("/a/b.webm")), None);
        assert_eq!(video_kind(Path::new("/a/b.png")), None);
        assert_eq!(mp4_name("baile.gif"), "baile.mp4");
        assert_eq!(mp4_name(".gif"), ".gif.mp4");
        assert_eq!(mp4_name(""), "video.mp4");
    }

    fn probe() -> crate::media::video::Probe {
        crate::media::video::Probe {
            width: 640,
            height: 360,
            duration_ms: 2600,
            frame: (16, 9, vec![90; 16 * 9 * 4]),
        }
    }

    #[test]
    fn a_probe_turns_a_staged_file_into_a_video() {
        let dir = std::env::temp_dir().join(format!("ferrochat-pv-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mp4 = dir.join("a.mp4");
        let gif = dir.join("b.gif");
        std::fs::write(&mp4, b"x").unwrap();
        std::fs::write(&gif, b"y").unwrap();
        let mut s = Staged::from_paths(vec![mp4, gif], false);
        assert_eq!(s.videos.len(), 2);
        assert_eq!(s.items[0].kind, OutgoingKind::Document, "until probed");
        let pending = std::mem::take(&mut s.videos);
        s.apply_probe(&pending[0], Ok((probe(), None)));
        match &s.items[0].kind {
            OutgoingKind::Video {
                seconds,
                width,
                gif,
                thumbnail,
                ..
            } => {
                assert_eq!((*seconds, *width, *gif), (3, 640, false));
                assert!(!thumbnail.is_empty());
            }
            k => panic!("{k:?}"),
        }
        assert_eq!(s.preview.as_ref().map(|p| (p.0, p.1)), Some((16, 9)));
        s.apply_probe(&pending[1], Ok((probe(), Some(b"mp4!".to_vec()))));
        assert_eq!(s.items[1].bytes, b"mp4!");
        assert_eq!(s.items[1].file_name, "b.mp4");
        assert!(matches!(
            s.items[1].kind,
            OutgoingKind::Video { gif: true, .. }
        ));
        // A failed probe leaves a document and says why.
        let mut s = Staged::from_paths(vec![dir.join("a.mp4")], false);
        let pending = std::mem::take(&mut s.videos);
        s.apply_probe(&pending[0], Err("no".into()));
        assert_eq!(s.items[0].kind, OutgoingKind::Document);
        assert_eq!(s.items[0].mimetype, "video/mp4");
        assert!(s.problems[0].contains("sent as a file"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
