//! Speaker monitor — singers hear themselves through the speakers.
//! Port of Python's MicMonitor dry path: `pw-loopback` does the
//! mic→sink route inside PipeWire at ~10ms latency (a userspace
//! capture→playback loop reads as distracting slapback). No capture
//! target: WirePlumber auto-links to the default source, the same one
//! cpal's "pipewire" input opens — so monitor and scoring always hear
//! the same mic, and both follow wpctl's default-source selection.
//!
//! Volume/mute ride on the loopback's playback node via wpctl — same
//! mechanism Python's _apply_loopback_state uses.

use std::process::{Child, Command, Stdio};
use std::sync::Mutex;

const OUTPUT_NODE: &str = "output.ups-monitor";

pub struct Monitor {
    child: Child,
    pub volume: Mutex<f64>,
    pub muted: Mutex<bool>,
    pub echo_ms: Mutex<i64>,
}

/// Spawn the loopback. Returns None where PipeWire tools don't exist
/// (dev machines) — scoring capture keeps working without it.
pub fn start() -> Option<Monitor> {
    // orphaned loopbacks from previous runs survive us — they double
    // the monitored mic and pile up ~8 MB each. Our own argv doesn't
    // contain "ups-monitor" so this can't self-match.
    let _ = Command::new("pkill")
        .args(["-f", "pw-loopback.*ups-monitor"])
        .status();
    let child = Command::new("pw-loopback")
        .args(["-n", "ups-monitor", "-c", "1", "-l", "10"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    eprintln!("monitor: pw-loopback ups-monitor running");
    Some(Monitor {
        child,
        volume: Mutex::new(0.8),
        muted: Mutex::new(false),
        echo_ms: Mutex::new(120),
    })
}

impl Monitor {
    /// Live controls — POST /api/mic port. Only Some() fields change.
    pub fn set(&self, volume: Option<f64>, muted: Option<bool>,
               echo_ms: Option<i64>) {
        if let Some(v) = volume {
            *self.volume.lock().unwrap() = v.clamp(0.0, 2.0);
        }
        if let Some(m) = muted {
            *self.muted.lock().unwrap() = m;
        }
        if let Some(e) = echo_ms {
            *self.echo_ms.lock().unwrap() = e.clamp(0, 600);
        }
        self.apply();
    }

    /// Push current volume+mute onto the loopback's playback node.
    /// Node ids churn across PipeWire restarts — resolve fresh.
    pub fn apply(&self) {
        let Some(id) = node_id(OUTPUT_NODE) else { return };
        let vol = *self.volume.lock().unwrap();
        let muted = *self.muted.lock().unwrap();
        let id = id.to_string();
        let _ = Command::new("wpctl")
            .args(["set-mute", &id, if muted { "1" } else { "0" }])
            .status();
        let _ = Command::new("wpctl")
            .args(["set-volume", &id, &format!("{vol:.2}")])
            .status();
    }

    /// The playback node appears async after spawn — retry until it
    /// exists, then push Python's default state once.
    pub fn apply_when_ready(&self) {
        for _ in 0..40 {
            if node_id(OUTPUT_NODE).is_some() {
                self.apply();
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
    }
}

/// Port of audio.py node_id — registry id of a node by node.name.
fn node_id(name: &str) -> Option<i64> {
    let out = Command::new("pw-dump").output().ok()?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    for o in v.as_array()? {
        if o["info"]["props"]["node.name"].as_str() == Some(name) {
            return o["id"].as_i64();
        }
    }
    None
}

// Child's handles are nulled at spawn; only Drop touches the process.
unsafe impl Send for Monitor {}
unsafe impl Sync for Monitor {}

impl Drop for Monitor {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}
