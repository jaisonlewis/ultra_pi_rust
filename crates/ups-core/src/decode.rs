//! Media decode via symphonia directly — rodio's Decoder wrapper
//! mis-probes our ffmpeg-produced mp4s, so we decode ourselves.
//! `StreamingSource` implements `rodio::Source`, pulling one packet at
//! a time — peak memory is a decode buffer, not the whole song.

use std::fs::File;
use std::path::Path;
use std::time::Duration;

use rodio::Source;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{Decoder, DecoderOptions};
use symphonia::core::errors::Error as SymphError;
use symphonia::core::formats::{FormatOptions, FormatReader};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

pub struct StreamingSource {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,
    track_id: u32,
    buf: Vec<f32>,
    pos: usize,
    channels: u16,
    rate: u32,
    done: bool,
}

impl StreamingSource {
    /// Open the first audio stream of `path`.
    pub fn open(path: &Path) -> Result<Self, String> {
        let file = File::open(path).map_err(|e| format!("open: {e}"))?;
        let mss = MediaSourceStream::new(Box::new(file), Default::default());
        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }
        let probed = symphonia::default::get_probe()
            .format(&hint, mss, &FormatOptions::default(),
                    &MetadataOptions::default())
            .map_err(|e| format!("probe: {e}"))?;
        let format = probed.format;
        // a track with a real sample rate wins, but some muxers write
        // 0 into the mp4a sample-entry rate field and put the truth in
        // the esds (observed on ffmpeg-produced 96kHz AAC) — so accept
        // any non-NULL codec too. Symphonia only registers audio
        // decoders, so a video track can't survive `make()` anyway.
        let track = format
            .tracks()
            .iter()
            .find(|t| {
                t.codec_params.codec
                    != symphonia::core::codecs::CODEC_TYPE_NULL
                    && t.codec_params
                        .sample_rate
                        .is_some_and(|r| r > 0)
            })
            .or_else(|| {
                format.tracks().iter().find(|t| {
                    t.codec_params.codec
                        != symphonia::core::codecs::CODEC_TYPE_NULL
                })
            })
            .cloned()
            .ok_or("no audio track")?;
        let decoder = symphonia::default::get_codecs()
            .make(&track.codec_params, &DecoderOptions::default())
            .map_err(|e| format!("codec: {e}"))?;
        let rate = track.codec_params.sample_rate.unwrap_or(0);
        let channels = track
            .codec_params
            .channels
            .map(|c| c.count() as u16)
            .unwrap_or(0);
        let mut src = StreamingSource {
            format,
            decoder,
            track_id: track.id,
            buf: Vec::new(),
            pos: 0,
            channels,
            rate,
            done: false,
        };
        // decode one packet eagerly so rate/channels reflect the real
        // spec (sample-entry fields can be 0) before callers read them
        src.pull();
        Ok(src)
    }

    /// Seconds of audio if the container knows the frame count.
    /// `rate` 0 (reported by some malformed streams) would give inf —
    /// `Duration::from_secs_f64(inf)` panics, so this must return None.
    pub fn duration(&self) -> Option<f64> {
        if self.rate == 0 {
            return None;
        }
        self.format
            .tracks()
            .iter()
            .find(|t| t.id == self.track_id)
            .and_then(|t| t.codec_params.n_frames)
            .map(|n| n as f64 / self.rate as f64)
            .filter(|d| d.is_finite() && *d > 0.0)
    }

    /// Pull + decode the next packet into `buf`. Returns false at EOF.
    fn pull(&mut self) -> bool {
        loop {
            let packet = match self.format.next_packet() {
                Ok(p) => p,
                Err(_) => {
                    self.done = true;
                    return false;
                }
            };
            if packet.track_id() != self.track_id {
                continue;
            }
            let decoded = match self.decoder.decode(&packet) {
                Ok(d) => d,
                Err(SymphError::DecodeError(_)) => continue,
                Err(_) => {
                    self.done = true;
                    return false;
                }
            };
            let n_frames = decoded.frames();
            if n_frames == 0 {
                continue;
            }
            // truth lives in the stream, not the container — a zeroed
            // sample entry heals itself here
            let spec = *decoded.spec();
            self.rate = spec.rate;
            self.channels = spec.channels.count() as u16;
            let mut sbuf =
                SampleBuffer::<f32>::new(n_frames as u64, *decoded.spec());
            sbuf.copy_interleaved_ref(decoded);
            self.buf.clear();
            self.buf.extend_from_slice(sbuf.samples());
            self.pos = 0;
            return !self.buf.is_empty();
        }
    }
}

impl Iterator for StreamingSource {
    type Item = f32;
    fn next(&mut self) -> Option<f32> {
        loop {
            if self.pos < self.buf.len() {
                let s = self.buf[self.pos];
                self.pos += 1;
                return Some(s);
            }
            if self.done || !self.pull() {
                return None;
            }
        }
    }
}

impl Source for StreamingSource {
    fn current_frame_len(&self) -> Option<usize> {
        None
    }
    fn channels(&self) -> u16 {
        self.channels
    }
    fn sample_rate(&self) -> u32 {
        self.rate
    }
    fn total_duration(&self) -> Option<Duration> {
        self.duration()
            .and_then(|d| Duration::try_from_secs_f64(d).ok())
    }
}

/// Piped-PCM decode via ffmpeg — the fallback for streams symphonia
/// can't handle at all (container probes that find no usable track,
/// codecs outside its registry, malformed muxing it rejects). ffmpeg
/// is already a hard dependency for the video pipe, so leaning on it
/// here costs nothing and covers every stream ffmpeg itself can read.
/// Output is normalized to 48kHz stereo float so the sink always sees
/// a sane spec; rodio resamples to the device rate downstream anyway.
pub struct FfmpegSource {
    child: std::process::Child,
    out: std::io::BufReader<std::process::ChildStdout>,
    buf: Vec<f32>,
    pos: usize,
    done: bool,
}

impl FfmpegSource {
    pub const RATE: u32 = 48_000;
    pub const CHANNELS: u16 = 2;

    pub fn open(path: &Path) -> Result<Self, String> {
        use std::process::{Command, Stdio};
        let mut child = Command::new("ffmpeg")
            .arg("-v").arg("error")
            .arg("-nostdin")
            .arg("-i").arg(path)
            .arg("-vn")
            .arg("-map").arg("0:a:0")
            .arg("-f").arg("f32le")
            .arg("-acodec").arg("pcm_f32le")
            .arg("-ac").arg(Self::CHANNELS.to_string())
            .arg("-ar").arg(Self::RATE.to_string())
            .arg("pipe:1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            // errors go straight to the service journal — the whole
            // point of this fallback is making failures visible
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("ffmpeg spawn: {e}"))?;
        let out = match child.stdout.take() {
            Some(o) => o,
            None => {
                let _ = child.kill();
                return Err("ffmpeg stdout".to_string());
            }
        };
        Ok(FfmpegSource {
            child,
            out: std::io::BufReader::new(out),
            buf: Vec::new(),
            pos: 0,
            done: false,
        })
    }

    /// Refill `buf` from the pipe. Returns false at EOF/error.
    fn pull(&mut self) -> bool {
        use std::io::Read;
        let mut raw = vec![0u8; 4096 * 4];
        let mut n = 0;
        while n < raw.len() {
            match self.out.read(&mut raw[n..]) {
                Ok(0) => {
                    self.done = true;
                    break;
                }
                Ok(k) => n += k,
                Err(e)
                    if e.kind() == std::io::ErrorKind::Interrupted =>
                {
                    continue;
                }
                Err(_) => {
                    self.done = true;
                    break;
                }
            }
        }
        self.buf.clear();
        self.buf
            .extend(raw[..n - n % 4].chunks_exact(4).map(|b| {
                f32::from_le_bytes([b[0], b[1], b[2], b[3]])
            }));
        self.pos = 0;
        !self.buf.is_empty()
    }
}

impl Drop for FfmpegSource {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Iterator for FfmpegSource {
    type Item = f32;
    fn next(&mut self) -> Option<f32> {
        loop {
            if self.pos < self.buf.len() {
                let s = self.buf[self.pos];
                self.pos += 1;
                return Some(s);
            }
            if self.done || !self.pull() {
                return None;
            }
        }
    }
}

impl Source for FfmpegSource {
    fn current_frame_len(&self) -> Option<usize> {
        None
    }
    fn channels(&self) -> u16 {
        Self::CHANNELS
    }
    fn sample_rate(&self) -> u32 {
        Self::RATE
    }
    fn total_duration(&self) -> Option<Duration> {
        None
    }
}

/// One entry point for "give me this file's audio": symphonia first
/// (no extra process), ffmpeg's PCM pipe when symphonia can't. The
/// tag says which won, for the journal.
pub fn open_audio(
    path: &Path,
) -> Result<(Box<dyn Source<Item = f32> + Send>, &'static str), String>
{
    match StreamingSource::open(path) {
        Ok(s) => Ok((Box::new(s), "symphonia")),
        Err(e) => FfmpegSource::open(path)
            .map(|s| (Box::new(s) as _, "ffmpeg"))
            .map_err(|e2| format!("{e}; ffmpeg: {e2}")),
    }
}
