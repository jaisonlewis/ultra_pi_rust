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
        let track = format
            .tracks()
            .iter()
            .find(|t| {
                t.codec_params.codec
                    != symphonia::core::codecs::CODEC_TYPE_NULL
                    && t.codec_params.sample_rate.is_some()
            })
            .cloned()
            .ok_or("no audio track")?;
        let decoder = symphonia::default::get_codecs()
            .make(&track.codec_params, &DecoderOptions::default())
            .map_err(|e| format!("codec: {e}"))?;
        let rate = track.codec_params.sample_rate.unwrap_or(44100);
        let channels = track
            .codec_params
            .channels
            .map(|c| c.count() as u16)
            .unwrap_or(2);
        Ok(StreamingSource {
            format,
            decoder,
            track_id: track.id,
            buf: Vec::new(),
            pos: 0,
            channels,
            rate,
            done: false,
        })
    }

    /// Seconds of audio if the container knows the frame count.
    pub fn duration(&self) -> Option<f64> {
        self.format
            .tracks()
            .iter()
            .find(|t| t.id == self.track_id)
            .and_then(|t| t.codec_params.n_frames)
            .map(|n| n as f64 / self.rate as f64)
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
        self.duration().map(Duration::from_secs_f64)
    }
}
