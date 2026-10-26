//! ffmpeg/ffprobe helpers — decode, loudness normalize, remux.
//! Everything shells out to the user's ffmpeg, same as Python does.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// ffmpeg → mono/stereo float32 interleaved pcm. `channels` 1 or 2.
pub fn decode_f32(path: &Path, rate: u32, channels: u32) -> Result<Vec<f32>> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args([
            "-ac",
            &channels.to_string(),
            "-ar",
            &rate.to_string(),
            "-f",
            "f32le",
            "-",
        ])
        .output()
        .context("spawn ffmpeg")?;
    if !out.status.success() {
        bail!("ffmpeg decode {}: {}", path.display(),
              String::from_utf8_lossy(&out.stderr));
    }
    Ok(out
        .stdout
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect())
}

/// ffprobe duration seconds.
pub fn duration(path: &Path) -> Result<f64> {
    let out = Command::new("ffprobe")
        .args([
            "-v", "error", "-show_entries", "format=duration", "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()?;
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .context("parse duration")
}

/// True when the file carries a real video stream — an mp3's embedded
/// cover art is an mjpeg *attached pic*, not a playable track.
pub fn has_real_video(path: &Path) -> bool {
    Command::new("ffprobe")
        .args([
            "-v", "error", "-select_streams", "v", "-show_entries",
            "stream_disposition=attached_pic", "-of", "csv=p=0",
        ])
        .arg(path)
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .any(|l| l.trim() == "0")
        })
        .unwrap_or(false)
}

// ---- loudness normalize (port of loudness.py) -----------------------

const TARGET_I: f64 = -14.0;
const TARGET_TP: f64 = -1.5;
const TARGET_LRA: f64 = 11.0;
const SKIP_TOL: f64 = 0.75;

/// `.normed` sidecar — records measured values so normalize is
/// idempotent across re-runs.
fn normed_path(media: &Path) -> PathBuf {
    let mut p = media.as_os_str().to_os_string();
    p.push(".normed");
    PathBuf::from(p)
}

pub fn is_normed(media: &Path) -> bool {
    let Ok(txt) = std::fs::read_to_string(normed_path(media)) else {
        return false;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) else {
        return false;
    };
    (v["input_i"].as_f64().unwrap_or(-99.0) - TARGET_I).abs()
        < SKIP_TOL
}

fn loudnorm_measure(media: &Path) -> Result<serde_json::Value> {
    let out = Command::new("ffmpeg")
        .args(["-v", "info", "-i"])
        .arg(media)
        .args([
            "-af",
            &format!("loudnorm=I={TARGET_I}:TP={TARGET_TP}:\
                      LRA={TARGET_LRA}:print_format=json"),
            "-f", "null", "-",
        ])
        .output()
        .context("spawn loudnorm")?;
    let err = String::from_utf8_lossy(&out.stderr);
    let start = err.rfind('{').context("no loudnorm json")?;
    let end = err.rfind('}').context("no loudnorm json")?;
    serde_json::from_str(&err[start..=end]).context("loudnorm json")
}

/// One-pass measured normalize. Writes `<media>.normed` marker.
/// Rewrites in place via a temp file — same container, loudnorm'd
/// audio, video stream copied.
pub fn normalize(media: &Path) -> Result<()> {
    if is_normed(media) {
        return Ok(());
    }
    let m = loudnorm_measure(media)?;
    let i = m["input_i"].as_str().unwrap_or("0").parse::<f64>()?;
    if (i - TARGET_I).abs() < SKIP_TOL {
        std::fs::write(normed_path(media), serde_json::to_string(&m)?)?;
        return Ok(());
    }
    let tmp = media.with_extension("norm.tmp.mp4");
    let af = format!(
        "loudnorm=I={TARGET_I}:TP={TARGET_TP}:LRA={TARGET_LRA}:\
         measured_I={}:measured_TP={}:measured_LRA={}:\
         measured_thresh={}:offset={}:linear=true",
        m["input_i"].as_str().unwrap_or_default(),
        m["input_tp"].as_str().unwrap_or_default(),
        m["input_lra"].as_str().unwrap_or_default(),
        m["input_thresh"].as_str().unwrap_or_default(),
        m["target_offset"].as_str().unwrap_or_default(),
    );
    let st = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-i"])
        .arg(media)
        .args(["-af", &af, "-c:v", "copy", "-c:a", "aac",
               "-b:a", "192k"])
        .arg(&tmp)
        .status()
        .context("spawn loudnorm apply")?;
    if !st.success() {
        bail!("loudnorm apply failed");
    }
    std::fs::rename(&tmp, media)?;
    std::fs::write(normed_path(media), serde_json::to_string(&m)?)?;
    Ok(())
}

// ---- wav io ---------------------------------------------------------

/// 16-bit PCM wav from interleaved f32 (channels = `ch`).
pub fn write_wav(path: &Path, samples: &[f32], sr: u32, ch: u16)
                 -> Result<()> {
    use std::io::Write;
    let n_data = (samples.len() * 2) as u32;
    let mut f = std::fs::File::create(path)?;
    let mut hdr = Vec::with_capacity(44);
    hdr.extend_from_slice(b"RIFF");
    hdr.extend_from_slice(&(36 + n_data).to_le_bytes());
    hdr.extend_from_slice(b"WAVEfmt ");
    hdr.extend_from_slice(&16u32.to_le_bytes());
    hdr.extend_from_slice(&1u16.to_le_bytes()); // pcm
    hdr.extend_from_slice(&ch.to_le_bytes());
    hdr.extend_from_slice(&sr.to_le_bytes());
    hdr.extend_from_slice(&(sr * ch as u32 * 2).to_le_bytes());
    hdr.extend_from_slice(&(ch * 2).to_le_bytes());
    hdr.extend_from_slice(&16u16.to_le_bytes());
    hdr.extend_from_slice(b"data");
    hdr.extend_from_slice(&n_data.to_le_bytes());
    f.write_all(&hdr)?;
    let mut buf = Vec::with_capacity(n_data as usize);
    for &s in samples {
        let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
        buf.extend_from_slice(&v.to_le_bytes());
    }
    f.write_all(&buf)?;
    Ok(())
}

/// stereo (2,n) wav — reads interleaved [L,R,L,R...] from two channels
pub fn read_wav_f32(path: &Path) -> Result<(Vec<f32>, u32, u16)> {
    let bytes = std::fs::read(path)?;
    let data = bytes
        .windows(4)
        .position(|w| w == b"data")
        .map(|i| i + 8)
        .context("wav data chunk")?;
    // minimal header parse — our own writes are PCM16
    let sr = u32::from_le_bytes(bytes[24..28].try_into()?);
    let ch = u16::from_le_bytes(bytes[22..24].try_into()?);
    let bits = u16::from_le_bytes(bytes[34..36].try_into()?);
    let raw = &bytes[data..];
    let samples: Vec<f32> = if bits == 16 {
        raw.chunks_exact(2)
            .map(|c| i16::from_le_bytes(c.try_into().unwrap())
                as f32 / 32768.0)
            .collect()
    } else if bits == 32 {
        raw.chunks_exact(4)
            .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
            .collect()
    } else {
        bail!("unsupported wav bits {bits}");
    };
    Ok((samples, sr, ch))
}

/// deinterleave stereo wav samples → (2, n) channel-major
pub fn deinterleave(inter: &[f32], ch: u16) -> Vec<Vec<f32>> {
    let ch = ch as usize;
    let mut out = vec![Vec::with_capacity(inter.len() / ch); ch];
    for (i, s) in inter.iter().enumerate() {
        out[i % ch].push(*s);
    }
    out
}

/// Remux the original video with a new audio stem — the karaoke-ready
/// .inst.mp4 the TV plays. Audio-only media gets an audio-only output.
pub fn karaoke_mp4(media: &Path, inst_stereo: &[f32], out: &Path)
                   -> Result<()> {
    let tmp = std::env::temp_dir().join(format!(
        "ups-prep-{}.wav", std::process::id()));
    write_wav(&tmp, inst_stereo, 44100, 2)?;
    let maps: Vec<String> = if has_real_video(media) {
        ["-map", "0:v", "-map", "1:a", "-shortest"]
            .iter().map(|s| s.to_string()).collect()
    } else {
        vec!["-map".into(), "1:a".into()]
    };
    let st = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-i"])
        .arg(media)
        .args(["-i"])
        .arg(&tmp)
        .args(&maps)
        .args(["-c:v", "copy", "-c:a", "aac", "-b:a", "192k"])
        .arg(out)
        .status()
        .context("spawn karaoke remux")?;
    let _ = std::fs::remove_file(&tmp);
    if !st.success() {
        bail!("karaoke remux failed");
    }
    Ok(())
}
