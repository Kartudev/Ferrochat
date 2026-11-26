//! Video decoder helper for Ferrochat, with no system codec dependency.
//!
//! `ferrochat-player <file>` plays one MP4 file (and `--gif-to-mp4 <in> <out>`
//! converts a GIF for sending, see `gif.rs`): H.264 video through
//! OpenH264 (Cisco's BSD-licensed decoder, built from source and linked
//! statically), AAC audio through symphonia (pure Rust), sound through
//! rodio. It writes RGBA frames and playback times to stdout and reads
//! commands from stdin; the protocol is in `ferrochat::media::video_proto`.
//! The app spawns one helper per open video and ends it when the viewer
//! closes, so a decoder never outlives its video and the main process links
//! no codec.
//!
//! Failures name the step that failed (open, container, codec, decode) so
//! the viewer can say what happened.

mod audio;
mod demux;
mod gif;
mod yuv;

fn main() {
    player::run();
}

/// A second handle to stdout as a plain file: no line buffering (Unix) and
/// no console text handling (Windows) for binary frames.
#[cfg(unix)]
fn raw_stdout() -> std::io::Result<std::fs::File> {
    use std::os::fd::AsFd;
    Ok(std::io::stdout().as_fd().try_clone_to_owned()?.into())
}

/// A second handle to stdout as a plain file: no line buffering (Unix) and
/// no console text handling (Windows) for binary frames.
#[cfg(windows)]
fn raw_stdout() -> std::io::Result<std::fs::File> {
    use std::os::windows::io::AsHandle;
    Ok(std::io::stdout().as_handle().try_clone_to_owned()?.into())
}

mod player {
    use std::io::{self, BufRead, BufWriter, Write};
    use std::path::Path;
    use std::sync::mpsc::{self, RecvTimeoutError};
    use std::time::{Duration, Instant};

    use ferrochat::media::video_proto::{self as proto, Cmd, Msg, PlayState};
    use openh264::decoder::{DecodedYUV, Decoder, DecoderConfig, Flush};
    use openh264::formats::YUVSource;

    use super::audio::{AHEAD_MS, AudioTrack, Output};
    use super::demux::{self, PendingTimes, TrackError, VideoTrack};
    use super::yuv::{self, Matrix, Planes};

    /// Widest / tallest frame sent to the app (aspect ratio is kept). The
    /// viewer scales it up; decoding and piping more would only cost RAM.
    const MAX_W: usize = 960;
    const MAX_H: usize = 540;
    /// How often the position is reported.
    const TIME_EVERY: Duration = Duration::from_millis(250);
    /// Consecutive undecodable samples before giving up.
    const MAX_BAD: usize = 90;

    type Out = BufWriter<std::fs::File>;

    pub fn run() {
        let args: Vec<std::ffi::OsString> = std::env::args_os().collect();
        if args
            .get(1)
            .is_some_and(|a| a == ferrochat::media::video::GIF_TO_MP4_FLAG)
        {
            let (Some(input), Some(output)) = (args.get(2), args.get(3)) else {
                eprintln!("usage: ferrochat-player --gif-to-mp4 <in.gif> <out.mp4>");
                std::process::exit(2);
            };
            super::gif::run(Path::new(input), Path::new(output));
        }
        let Some(path) = std::env::args().nth(1) else {
            eprintln!("usage: ferrochat-player <file>");
            std::process::exit(2);
        };
        // stdout as a plain file: no line buffering for binary frames.
        let out_file = match super::raw_stdout() {
            Ok(f) => f,
            Err(_) => std::process::exit(2),
        };
        let mut out: Out = BufWriter::with_capacity(1 << 20, out_file);
        // A hostile file must never take the app down, and a bug in a
        // demuxer must still end with a message instead of silence.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            Player::open(Path::new(&path)).and_then(|p| p.run(&mut out))
        }));
        let failure = match result {
            Ok(Ok(())) => return,
            Ok(Err(f)) => f,
            Err(_) => fail("the decoder stopped on malformed data"),
        };
        send(&mut out, &failure);
        std::process::exit(1);
    }

    fn send(out: &mut Out, m: &Msg) {
        if proto::write_msg(out, m).and_then(|()| out.flush()).is_err() {
            // The app is gone (broken pipe): nothing left to play for.
            std::process::exit(0);
        }
    }

    fn fail(text: impl Into<String>) -> Msg {
        Msg::Failed {
            missing: false,
            text: text.into(),
        }
    }

    /// The playback clock: wall time while playing, frozen while paused.
    struct Clock {
        base_ms: i64,
        since: Option<Instant>,
    }

    impl Clock {
        fn now(&self) -> i64 {
            self.base_ms + self.since.map_or(0, |s| s.elapsed().as_millis() as i64)
        }
        fn set(&mut self, ms: i64) {
            self.base_ms = ms;
            if self.since.is_some() {
                self.since = Some(Instant::now());
            }
        }
        fn play(&mut self) {
            if self.since.is_none() {
                self.since = Some(Instant::now());
            }
        }
        fn pause(&mut self) {
            self.base_ms = self.now();
            self.since = None;
        }
        fn playing(&self) -> bool {
            self.since.is_some()
        }
    }

    struct Video {
        track: VideoTrack,
        decoder: Decoder,
        /// Next sample to feed (decode order).
        next: usize,
        pending: PendingTimes,
        /// Decoded picture waiting for its time.
        ready: Option<(i64, Vec<u8>)>,
        out_w: usize,
        out_h: usize,
        matrix: Matrix,
        bad: usize,
        flushed: bool,
        /// Pictures flushed out of the decoder at the end.
        tail: std::collections::VecDeque<(i64, Vec<u8>)>,
        buf: Vec<u8>,
    }

    fn new_decoder() -> Result<Decoder, Msg> {
        // B-frames (Main / High profile) need the decoder's reorder buffer:
        // flushing after each sample breaks them.
        Decoder::with_api_config(
            openh264::OpenH264API::from_source(),
            DecoderConfig::new().flush_after_decode(Flush::NoFlush),
        )
        .map_err(|e| fail(format!("the H.264 decoder could not start: {e}")))
    }

    /// Output geometry of the converted pictures.
    #[derive(Clone, Copy)]
    struct Geometry {
        rotation: yuv::Rotation,
        out_w: usize,
        out_h: usize,
        matrix: Matrix,
    }

    fn convert(g: Geometry, pic: &DecodedYUV<'_>) -> Option<Vec<u8>> {
        let (w, h) = pic.dimensions();
        let (ys, us, vs) = pic.strides();
        yuv::to_rgba(
            &Planes {
                y: pic.y(),
                u: pic.u(),
                v: pic.v(),
                strides: (ys, us, vs),
                width: w,
                height: h,
            },
            g.rotation,
            g.out_w,
            g.out_h,
            g.matrix,
        )
    }

    impl Video {
        fn geometry(&self) -> Geometry {
            Geometry {
                rotation: self.track.rotation,
                out_w: self.out_w,
                out_h: self.out_h,
                matrix: self.matrix,
            }
        }

        /// Decodes until one picture is ready (or the stream ends).
        /// `Err` after too many consecutive undecodable samples.
        fn fill(&mut self) -> Result<(), Msg> {
            while self.ready.is_none() {
                if let Some(t) = self.tail.pop_front() {
                    self.ready = Some(t);
                    return Ok(());
                }
                if self.next >= self.track.times.len() {
                    if !self.flushed {
                        // End of stream: the reorder buffer still holds the
                        // last pictures (a handful at most).
                        self.flushed = true;
                        let g = self.geometry();
                        let pics = self.decoder.flush_remaining().unwrap_or_default();
                        let mut tail = std::collections::VecDeque::new();
                        for pic in pics.iter().take(16) {
                            if let (Some(t), Some(px)) = (self.pending.pop(), convert(g, pic)) {
                                tail.push_back((t, px));
                            }
                        }
                        self.tail = tail;
                        continue;
                    }
                    return Ok(());
                }
                let i = self.next;
                self.next += 1;
                let pts = self.track.times.get(i).map_or(0, |t| t.pts_ms);
                self.buf.clear();
                if let Err(e) = self.track.read(i, &mut self.buf) {
                    return Err(fail(format!("reading video sample {i} failed: {e}")));
                }
                self.pending.push(pts);
                let packet = std::mem::take(&mut self.buf);
                let g = self.geometry();
                let res = self.decoder.decode(&packet);
                let mut produced = None;
                match res {
                    Ok(Some(pic)) => {
                        self.bad = 0;
                        produced = convert(g, &pic);
                    }
                    Ok(None) => self.bad = 0,
                    Err(e) => {
                        self.bad += 1;
                        if self.bad >= MAX_BAD {
                            return Err(fail(format!(
                                "H.264 decoding failed at {:.1} s: {e}",
                                pts as f64 / 1000.0
                            )));
                        }
                    }
                }
                self.buf = packet;
                if let Some(px) = produced {
                    let t = self.pending.pop().unwrap_or(pts);
                    self.ready = Some((t, px));
                }
            }
            Ok(())
        }

        /// Restarts decoding from the keyframe before `ms`.
        fn seek(&mut self, ms: i64) -> Result<(), Msg> {
            self.decoder = new_decoder()?;
            let _ = self.decoder.decode(&self.track.header);
            self.next = demux::seek_start(&self.track.times, ms);
            self.pending.clear();
            self.ready = None;
            self.tail.clear();
            self.flushed = false;
            self.bad = 0;
            Ok(())
        }

        fn ended(&self) -> bool {
            self.ready.is_none()
                && self.tail.is_empty()
                && self.next >= self.track.times.len()
                && self.flushed
        }
    }

    struct Player {
        video: Option<Video>,
        audio: Option<AudioTrack>,
        duration_ms: u64,
    }

    impl Player {
        fn open(path: &Path) -> Result<Self, Msg> {
            if !path.is_file() {
                return Err(fail("the video file is missing"));
            }
            let (duration_ms, _) = demux::duration_ms(path).map_err(track_failure)?;
            let video = match VideoTrack::open(path).map_err(track_failure)? {
                Some(track) => {
                    let (dw, dh) = track.rotation.display(track.width, track.height);
                    let (out_w, out_h) = yuv::fit(dw, dh, MAX_W, MAX_H);
                    if out_w == 0 || out_h == 0 {
                        return Err(fail("the video has no picture size"));
                    }
                    let mut decoder = new_decoder()?;
                    let _ = decoder.decode(&track.header);
                    let matrix = Matrix::for_height(track.height);
                    Some(Video {
                        track,
                        decoder,
                        next: 0,
                        pending: PendingTimes::default(),
                        ready: None,
                        out_w,
                        out_h,
                        matrix,
                        bad: 0,
                        flushed: false,
                        tail: std::collections::VecDeque::new(),
                        buf: Vec::with_capacity(1 << 16),
                    })
                }
                None => None,
            };
            let audio = match AudioTrack::open(path) {
                Ok(a) => a,
                Err(e) => {
                    // The picture still plays; say why there is no sound.
                    eprintln!("ferrochat-player: no sound: {e}");
                    None
                }
            };
            if video.is_none() && audio.is_none() {
                return Err(fail("the file has no playable video or audio"));
            }
            Ok(Self {
                video,
                audio,
                duration_ms,
            })
        }

        fn run(mut self, out: &mut Out) -> Result<(), Msg> {
            let (tx, rx) = mpsc::channel::<Cmd>();
            std::thread::spawn(move || {
                for line in io::stdin().lock().lines() {
                    match line {
                        Ok(l) => {
                            if let Some(c) = Cmd::parse(&l)
                                && tx.send(c).is_err()
                            {
                                return;
                            }
                        }
                        Err(_) => break,
                    }
                }
                // stdin closed: the app is gone.
                let _ = tx.send(Cmd::Quit);
            });

            // First picture: it gives the size for `Ready` and is shown at once.
            let (width, height) = match self.video.as_mut() {
                Some(v) => {
                    v.fill()?;
                    if v.ready.is_none() {
                        return Err(fail("no picture could be decoded from the H.264 stream"));
                    }
                    (v.out_w as u32, v.out_h as u32)
                }
                None => (0, 0),
            };
            send(
                out,
                &Msg::Ready {
                    width,
                    height,
                    duration_ms: self.duration_ms,
                },
            );
            let sound = self.audio.as_ref().and_then(|_| Output::open());
            let mut volume = 1.0f32;
            let mut muted = false;
            let mut clock = Clock {
                base_ms: 0,
                since: None,
            };
            let mut want_play = true;
            let mut ended = false;
            // Show the picture at the clock even while paused (after a seek).
            let mut show_now = true;
            let mut last_time = Instant::now() - TIME_EVERY;

            loop {
                if want_play && !ended && !clock.playing() {
                    clock.play();
                    if let Some(s) = &sound {
                        s.set_playing(true);
                    }
                }
                let wait = match self.video.as_ref().and_then(|v| v.ready.as_ref()) {
                    Some((t, _)) if clock.playing() => (t - clock.now()).clamp(1, 15) as u64,
                    _ => 15,
                };
                match rx.recv_timeout(Duration::from_millis(wait)) {
                    Ok(Cmd::Quit) | Err(RecvTimeoutError::Disconnected) => return Ok(()),
                    Ok(Cmd::Play) => {
                        want_play = true;
                        if ended {
                            ended = false;
                            self.seek(0, &sound, &mut clock)?;
                            show_now = true;
                        }
                    }
                    Ok(Cmd::Pause) => {
                        want_play = false;
                        clock.pause();
                        if let Some(s) = &sound {
                            s.set_playing(false);
                        }
                    }
                    Ok(Cmd::Seek(ms)) => {
                        ended = false;
                        let ms = ms.min(self.duration_ms.max(1));
                        self.seek(ms as i64, &sound, &mut clock)?;
                        show_now = true;
                    }
                    Ok(Cmd::Volume(v)) => {
                        volume = f32::from(v.min(100)) / 100.0;
                        if let Some(s) = &sound {
                            s.set_volume(if muted { 0.0 } else { volume });
                        }
                    }
                    Ok(Cmd::Mute(m)) => {
                        muted = m;
                        if let Some(s) = &sound {
                            s.set_volume(if muted { 0.0 } else { volume });
                        }
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                }

                let now = clock.now();
                if let Some(v) = self.video.as_mut() {
                    v.fill()?;
                    // Late pictures are skipped (decoded, not sent) so a slow
                    // machine stays in sync instead of drifting behind.
                    let mut frame = None;
                    while let Some((t, _)) = &v.ready {
                        if *t > now && !(show_now && frame.is_none()) {
                            break;
                        }
                        frame = v.ready.take();
                        v.fill()?;
                        if show_now {
                            break;
                        }
                    }
                    if let Some((_, px)) = frame {
                        show_now = false;
                        if proto::write_frame(out, v.out_w as u32, v.out_h as u32, &px)
                            .and_then(|()| out.flush())
                            .is_err()
                        {
                            std::process::exit(0);
                        }
                    }
                }
                if let (Some(a), Some(s)) = (self.audio.as_mut(), &sound)
                    && clock.playing()
                {
                    while !a.done && a.decoded_until < now + AHEAD_MS {
                        match a.next_chunk() {
                            Some((_, samples)) => {
                                let (ch, rate) = a.format();
                                s.push(ch, rate, samples);
                            }
                            None => break,
                        }
                    }
                }

                let video_done = self.video.as_ref().is_none_or(Video::ended);
                if clock.playing()
                    && video_done
                    && (now as u64 >= self.duration_ms || self.video.is_some())
                {
                    ended = true;
                    want_play = false;
                    clock.pause();
                    if let Some(s) = &sound {
                        s.set_playing(false);
                    }
                    last_time = Instant::now() - TIME_EVERY;
                }

                if last_time.elapsed() >= TIME_EVERY {
                    last_time = Instant::now();
                    let state = if ended {
                        PlayState::Ended
                    } else if clock.playing() {
                        PlayState::Playing
                    } else {
                        PlayState::Paused
                    };
                    let pos = if ended {
                        self.duration_ms
                    } else {
                        (clock.now().max(0) as u64).min(self.duration_ms.max(1))
                    };
                    send(
                        out,
                        &Msg::Time {
                            position_ms: pos,
                            duration_ms: self.duration_ms,
                            state,
                        },
                    );
                }
            }
        }

        fn seek(&mut self, ms: i64, sound: &Option<Output>, clock: &mut Clock) -> Result<(), Msg> {
            clock.set(ms);
            if let Some(v) = self.video.as_mut() {
                v.seek(ms)?;
                // Decode up to the target: pictures before it are not shown.
                loop {
                    v.fill()?;
                    match &v.ready {
                        Some((t, _)) if *t + 20 < ms => v.ready = None,
                        _ => break,
                    }
                }
            }
            if let Some(a) = self.audio.as_mut() {
                a.seek(ms.max(0) as u64);
            }
            if let Some(s) = sound {
                s.clear(clock.playing());
            }
            Ok(())
        }
    }

    fn track_failure(e: TrackError) -> Msg {
        match e {
            TrackError::NotMp4(why) => fail(format!("not a playable MP4 file ({why})")),
            TrackError::Unsupported(codec) => Msg::Failed {
                missing: true,
                text: codec,
            },
        }
    }
}
