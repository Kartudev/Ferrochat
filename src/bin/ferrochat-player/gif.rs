//! `ferrochat-player --gif-to-mp4 <in.gif> <out.mp4>`: turns an animated
//! GIF into the silent H.264 MP4 WhatsApp sends for GIFs (`gifPlayback`).
//! OpenH264's encoder (Baseline, from the same statically linked build as
//! the decoder) and the `mp4` writer; the GIF is decoded and composited by
//! `image`. Lives in the helper because the main binary links no codec.
//!
//! Frames keep their own delays (the MP4 has variable sample durations),
//! transparency is flattened onto white, the picture is scaled to fit
//! [`MAX_SIDE`] with even sides (4:2:0 needs them), and very long GIFs are
//! cut at [`MAX_MS`] / [`MAX_FRAMES`].

use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::Path;

use image::codecs::gif::GifDecoder;
use image::{AnimationDecoder, RgbaImage};
use mp4::{AvcConfig, MediaConfig, Mp4Config, Mp4Sample, Mp4Writer, TrackConfig, TrackType};
use openh264::Timestamp;
use openh264::encoder::{BitRate, Encoder, EncoderConfig, FrameRate, FrameType, Profile};
use openh264::formats::{RgbSliceU8, YUVBuffer};

/// Longest side of the MP4 (WhatsApp's own GIFs are around 480 px).
pub const MAX_SIDE: u32 = 640;
/// Longest GIF kept, in ms.
pub const MAX_MS: u64 = 60_000;
/// Most frames kept.
pub const MAX_FRAMES: usize = 1500;
/// Browsers treat shorter delays as 100 ms; so does WhatsApp.
const MIN_DELAY_MS: u32 = 20;
const DEFAULT_DELAY_MS: u32 = 100;
/// A one-frame GIF still plays for this long.
const STILL_MS: u32 = 1000;
/// MP4 timescale: durations in ms.
const TIMESCALE: u32 = 1000;

/// A GIF frame's delay in ms, the way browsers play it.
pub fn delay_ms(numer: u32, denom: u32) -> u32 {
    let ms = numer.checked_div(denom).unwrap_or(0);
    if ms < MIN_DELAY_MS {
        DEFAULT_DELAY_MS
    } else {
        ms
    }
}

/// Output size for a `w`x`h` GIF: fits [`MAX_SIDE`], never upscales, both
/// sides even and at least 2.
pub fn even_size(w: u32, h: u32) -> (u32, u32) {
    if w == 0 || h == 0 {
        return (2, 2);
    }
    let s = (f64::from(MAX_SIDE) / f64::from(w.max(h))).min(1.0);
    let even = |v: u32| -> u32 {
        let v = (f64::from(v) * s).round() as u32;
        (v & !1).max(2)
    };
    (even(w), even(h))
}

/// RGBA → RGB on white, scaled to `size`.
fn flatten(frame: &RgbaImage, size: (u32, u32)) -> Vec<u8> {
    let scaled;
    let img = if frame.dimensions() == size {
        frame
    } else {
        scaled =
            image::imageops::resize(frame, size.0, size.1, image::imageops::FilterType::Triangle);
        &scaled
    };
    let blend = |c: u8, a: u8| -> u8 {
        let (c, a) = (u16::from(c), u16::from(a));
        ((c * a + 255 * (255 - a) + 127) / 255) as u8
    };
    img.pixels()
        .flat_map(|p| {
            let [r, g, b, a] = p.0;
            [blend(r, a), blend(g, a), blend(b, a)]
        })
        .collect()
}

/// Annex B NAL (with its start code) → payload.
fn strip_start_code(nal: &[u8]) -> &[u8] {
    if let Some(rest) = nal.strip_prefix(&[0, 0, 0, 1]) {
        rest
    } else if let Some(rest) = nal.strip_prefix(&[0, 0, 1]) {
        rest
    } else {
        nal
    }
}

struct Sample {
    avcc: Vec<u8>,
    sync: bool,
    start: u64,
    duration: u32,
}

/// Converts `input` into `output`. Errors are one line for the app.
pub fn convert(input: &Path, output: &Path) -> Result<(), String> {
    let file = File::open(input).map_err(|e| format!("cannot open the GIF: {e}"))?;
    let decoder = GifDecoder::new(BufReader::new(file)).map_err(|e| format!("not a GIF: {e}"))?;
    let mut size = None;
    let mut encoder = None;
    let mut sps = Vec::new();
    let mut pps = Vec::new();
    let mut samples: Vec<Sample> = Vec::new();
    let mut t: u64 = 0;
    for (i, frame) in decoder.into_frames().enumerate() {
        if i >= MAX_FRAMES || t >= MAX_MS {
            break;
        }
        let frame = frame.map_err(|e| format!("broken GIF frame: {e}"))?;
        let (numer, denom) = frame.delay().numer_denom_ms();
        let delay = delay_ms(numer, denom);
        let buf = frame.into_buffer();
        let (w, h) = *size.get_or_insert_with(|| even_size(buf.width(), buf.height()));
        let enc = match &mut encoder {
            Some(e) => e,
            None => {
                let fps = 1000.0 / delay as f32;
                let config = EncoderConfig::new()
                    .profile(Profile::Baseline)
                    .bitrate(BitRate::from_bps(1_500_000))
                    .max_frame_rate(FrameRate::from_hz(fps.clamp(1.0, 50.0)))
                    .skip_frames(false);
                let e = Encoder::with_api_config(openh264::OpenH264API::from_source(), config)
                    .map_err(|e| format!("encoder: {e}"))?;
                encoder.insert(e)
            }
        };
        let rgb = flatten(&buf, (w, h));
        let yuv = YUVBuffer::from_rgb8_source(RgbSliceU8::new(&rgb, (w as usize, h as usize)));
        let bits = enc
            .encode_at(&yuv, Timestamp::from_millis(t))
            .map_err(|e| format!("encoder: {e}"))?;
        let sync = matches!(bits.frame_type(), FrameType::IDR);
        let mut avcc = Vec::new();
        for l in 0..bits.num_layers() {
            let Some(layer) = bits.layer(l) else { continue };
            for n in 0..layer.nal_count() {
                let Some(nal) = layer.nal_unit(n).map(strip_start_code) else {
                    continue;
                };
                match nal.first().map(|b| b & 0x1f) {
                    Some(7) => sps = nal.to_vec(),
                    Some(8) => pps = nal.to_vec(),
                    Some(_) => {
                        avcc.extend_from_slice(&(nal.len() as u32).to_be_bytes());
                        avcc.extend_from_slice(nal);
                    }
                    None => {}
                }
            }
        }
        if avcc.is_empty() {
            // Skipped frame: the previous picture stays on screen longer.
            if let Some(prev) = samples.last_mut() {
                prev.duration += delay;
            }
        } else {
            samples.push(Sample {
                avcc,
                sync,
                start: t,
                duration: delay,
            });
        }
        t += u64::from(delay);
    }
    let (Some((w, h)), false, false) = (size, sps.is_empty(), pps.is_empty()) else {
        return Err("the GIF has no pictures".into());
    };
    if samples.len() == 1 {
        samples[0].duration = samples[0].duration.max(STILL_MS);
    }
    let out = File::create(output).map_err(|e| format!("cannot write the MP4: {e}"))?;
    let config = Mp4Config {
        major_brand: "isom".parse().map_err(|e| format!("{e}"))?,
        minor_version: 512,
        compatible_brands: ["isom", "iso2", "avc1", "mp41"]
            .iter()
            .filter_map(|b| b.parse().ok())
            .collect(),
        timescale: TIMESCALE,
    };
    let mut writer =
        Mp4Writer::write_start(BufWriter::new(out), &config).map_err(|e| format!("mp4: {e}"))?;
    writer
        .add_track(&TrackConfig {
            track_type: TrackType::Video,
            timescale: TIMESCALE,
            language: "und".into(),
            media_conf: MediaConfig::AvcConfig(AvcConfig {
                width: u16::try_from(w).map_err(|e| e.to_string())?,
                height: u16::try_from(h).map_err(|e| e.to_string())?,
                seq_param_set: sps,
                pic_param_set: pps,
            }),
        })
        .map_err(|e| format!("mp4: {e}"))?;
    for s in samples {
        writer
            .write_sample(
                1,
                &Mp4Sample {
                    start_time: s.start,
                    duration: s.duration,
                    rendering_offset: 0,
                    is_sync: s.sync,
                    bytes: s.avcc.into(),
                },
            )
            .map_err(|e| format!("mp4: {e}"))?;
    }
    writer.write_end().map_err(|e| format!("mp4: {e}"))?;
    let mut w = writer.into_writer();
    std::io::Write::flush(&mut w).map_err(|e| format!("cannot write the MP4: {e}"))?;
    Ok(())
}

/// Entry point for the `--gif-to-mp4` mode; never panics out.
pub fn run(input: &Path, output: &Path) -> ! {
    let result = std::panic::catch_unwind(|| convert(input, output));
    match result {
        Ok(Ok(())) => std::process::exit(0),
        Ok(Err(e)) => eprintln!("{e}"),
        Err(_) => eprintln!("the converter stopped on malformed data"),
    }
    let _ = std::fs::remove_file(output);
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delays_follow_browsers() {
        assert_eq!(delay_ms(70, 1), 70);
        assert_eq!(delay_ms(0, 1), 100);
        assert_eq!(delay_ms(10, 1), 100);
        assert_eq!(delay_ms(20, 1), 20);
        assert_eq!(delay_ms(5, 0), 100);
    }

    #[test]
    fn sizes_are_even_and_bounded() {
        assert_eq!(even_size(320, 240), (320, 240));
        assert_eq!(even_size(321, 241), (320, 240));
        assert_eq!(even_size(1280, 720), (640, 360));
        assert_eq!(even_size(1, 1), (2, 2));
        assert_eq!(even_size(0, 10), (2, 2));
        let (w, h) = even_size(999, 333);
        assert!(w <= MAX_SIDE && w % 2 == 0 && h % 2 == 0);
    }

    #[test]
    fn start_codes_are_stripped() {
        assert_eq!(strip_start_code(&[0, 0, 0, 1, 0x67, 1]), &[0x67, 1]);
        assert_eq!(strip_start_code(&[0, 0, 1, 0x68]), &[0x68]);
        assert_eq!(strip_start_code(&[0x65, 9]), &[0x65, 9]);
    }

    #[test]
    fn transparency_is_flattened_on_white() {
        let img = RgbaImage::from_raw(2, 1, vec![0, 0, 0, 0, 10, 20, 30, 255]).unwrap();
        assert_eq!(flatten(&img, (2, 1)), vec![255, 255, 255, 10, 20, 30]);
    }
}
