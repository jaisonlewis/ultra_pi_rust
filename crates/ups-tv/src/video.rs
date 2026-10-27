//! Video background: spawn ffmpeg to decode scaled RGB24 frames into a
//! pipe; a reader thread keeps a small ring of decoded frames. Audio is
//! NOT decoded here — rodio owns it and is the master clock; the render
//! loop latches whatever frame index matches the current position.

use std::io::{BufReader, Read};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{sync_channel, Receiver};
use std::thread::JoinHandle;

pub struct VideoPipe {
    pub rx: Receiver<Vec<u8>>,
    child: Child,
    _reader: JoinHandle<()>,
    pub fps: f64,
    pub frames: u64,
    /// frames ffmpeg has emitted (reader-thread side) — compare against
    /// `frames` (consumed) to see whether the decoder is ahead/behind
    pub produced: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

/// Optional hardware decode — `UPS_HWACCEL=v4l2m2m` on the Pi pushes
/// h264 onto the VideoCore block instead of a CPU core. Off by default:
/// a wrong hwaccel kills the whole picture, so it stays opt-in.
fn hwaccel() -> &'static [&'static str] {
    static HW: std::sync::OnceLock<Vec<&'static str>> =
        std::sync::OnceLock::new();
    HW.get_or_init(|| {
        let name = std::env::var("UPS_HWACCEL").unwrap_or_default();
        if name.is_empty() {
            return Vec::new();
        }
        let listed = Command::new("ffmpeg")
            .arg("-hwaccels")
            .stderr(Stdio::null())
            .output()
            .ok()
            .map(|o| {
                String::from_utf8_lossy(&o.stdout).contains(name.as_str())
            })
            .unwrap_or(false);
        if listed {
            vec!["-hwaccel", Box::leak(name.into_boxed_str())]
        } else {
            Vec::new()
        }
    })
}

impl VideoPipe {
    /// ffmpeg decodes at `fps` to `w`x`h` rgb24. Returns None if ffmpeg
    /// can't be spawned (audio-only path handles that gracefully).
    pub fn open(path: &std::path::Path, w: usize, h: usize, fps: f64)
        -> Option<Self> {
        let mut child = Command::new("ffmpeg")
            .args(["-v", "error"])
            .args(hwaccel())
            .args([
                "-i", &path.display().to_string(),
                "-an",
                "-vf", &format!("scale={w}:{h}:force_original_aspect_ratio=\
                                 decrease,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2"),
                "-r", &format!("{fps}"),
                "-f", "rawvideo",
                "-pix_fmt", "rgb24",
                "pipe:1",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .stdin(Stdio::null())
            .spawn()
            .ok()?;
        let stdout = child.stdout.take()?;
        let (tx, rx) = sync_channel::<Vec<u8>>(4); // ~4-frame buffer
        let frame_bytes = w * h * 3;
        let produced =
            std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let produced_r = produced.clone();
        let reader = std::thread::spawn(move || {
            let mut rd = BufReader::with_capacity(frame_bytes * 2, stdout);
            loop {
                let mut buf = vec![0u8; frame_bytes];
                if rd.read_exact(&mut buf).is_err() {
                    break; // stream ended
                }
                produced_r.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if tx.send(buf).is_err() {
                    break; // receiver gone
                }
            }
        });
        Some(VideoPipe {
            rx,
            child,
            _reader: reader,
            fps,
            frames: 0,
            produced,
        })
    }
}

impl Drop for VideoPipe {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}
