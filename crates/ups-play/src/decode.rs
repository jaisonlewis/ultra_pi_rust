//! Media decode via symphonia directly — rodio's Decoder wrapper
//! mis-probes our ffmpeg-produced mp4s, so we decode to interleaved
//! f32 PCM ourselves and hand a `SamplesBuffer` to the sink.

use std::fs::File;
use std::path::Path;

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error as SymphError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// Decode the first audio stream of `path` → (interleaved f32 samples,
/// channels, sample_rate). Errors return Err(String).
pub fn decode_audio(path: &Path) -> Result<(Vec<f32>, u16, u32), String> {
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
    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != symphonia::core::codecs::CODEC_TYPE_NULL
              && t.codec_params.sample_rate.is_some())
        .cloned()
        .ok_or("no audio track")?;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| format!("codec: {e}"))?;
    let rate = track.codec_params.sample_rate.unwrap_or(44100);
    let channels = track
        .codec_params
        .channels
        .map(|c| c.count() as u16)
        .unwrap_or(2);

    let mut out: Vec<f32> = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymphError::IoError(e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(SymphError::ResetRequired) => break,
            Err(_) => break,
        };
        if packet.track_id() != track.id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(SymphError::DecodeError(_)) => continue,
            Err(_) => break,
        };
        let spec = *decoded.spec();
        let n_frames = decoded.frames();
        if n_frames == 0 {
            continue;
        }
        let mut buf = SampleBuffer::<f32>::new(n_frames as u64, spec);
        buf.copy_interleaved_ref(decoded);
        out.extend_from_slice(buf.samples());
    }
    if out.is_empty() {
        return Err("decoded zero samples".into());
    }
    Ok((out, channels, rate))
}
