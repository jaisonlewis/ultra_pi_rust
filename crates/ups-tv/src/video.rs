//! Video background: spawn ffmpeg to decode scaled bgr0 frames into a
//! pipe; a reader thread keeps a small ring of decoded frames. Audio is
//! NOT decoded here — rodio owns it and is the master clock; the render
//! loop latches whatever frame index matches the current position.
//!
//! bgr0 is minifb's native u32 0RGB layout — the blit is a single
//! memcpy and ffmpeg's swscale does the conversion in SIMD. Frames
//! round-trip through `ret_tx` so we never re-allocate 8MB per frame.

use std::io::{BufReader, Read};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::thread::JoinHandle;

pub struct VideoPipe {
    pub rx: Receiver<Vec<u32>>,
    /// spent frame buffers go back to the reader for reuse
    pub ret_tx: SyncSender<Vec<u32>>,
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
    /// ffmpeg decodes at `fps` to `w`x`h` bgr0. Returns None if ffmpeg
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
                "-pix_fmt", "bgr0",
                "pipe:1",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .stdin(Stdio::null())
            .spawn()
            .ok()?;
        let stdout = child.stdout.take()?;
        let (tx, rx) = sync_channel::<Vec<u32>>(4); // ~4-frame buffer
        let (ret_tx, ret_rx) = sync_channel::<Vec<u32>>(4);
        let frame_len = w * h;
        let produced =
            std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let produced_r = produced.clone();
        let reader = std::thread::spawn(move || {
            let mut rd = BufReader::with_capacity(frame_len * 8, stdout);
            loop {
                // a recycled buffer when the render loop returned one,
                // else a fresh 8MB alloc — reuse keeps mmap churn off
                // the realtime path
                let mut buf = ret_rx
                    .try_recv()
                    .unwrap_or_else(|_| vec![0u32; frame_len]);
                buf.resize(frame_len, 0);
                // u32 is POD — view the buffer as bytes for read_exact
                let bytes = unsafe {
                    std::slice::from_raw_parts_mut(
                        buf.as_mut_ptr() as *mut u8,
                        frame_len * 4,
                    )
                };
                if rd.read_exact(bytes).is_err() {
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
            ret_tx,
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
