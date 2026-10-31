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

use std::collections::HashMap;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;

use crate::state::FxProfile;

const OUTPUT_NODE: &str = "output.ups-monitor";

pub struct Monitor {
    child: Child,
    pub volume: Mutex<f64>,
    pub muted: Mutex<bool>,
    pub echo_ms: Mutex<i64>,
    /// room-default voice FX (no performer identity on the session)
    pub fx: Mutex<FxProfile>,
    /// device name -> active singer's profile — set when their queue
    /// item starts so their saved sound loads onto their mic
    pub src_fx: Mutex<HashMap<String, FxProfile>>,
}

/// Spawn the loopback. Returns None where PipeWire tools don't exist
/// (dev machines) — scoring capture keeps working without it.
/// `init` carries the persisted room levels + room FX from state.
pub fn start(init: (f64, bool, i64, FxProfile)) -> Option<Monitor> {
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
        volume: Mutex::new(init.0),
        muted: Mutex::new(init.1),
        echo_ms: Mutex::new(init.2),
        fx: Mutex::new(init.3),
        src_fx: Mutex::new(HashMap::new()),
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

    /// Room-default voice FX (sessions with no performer identity).
    /// Stored for the DSP chain and echoed back through /api/mic;
    /// merging here keeps untouched keys at their saved values.
    pub fn set_fx(&self, fx: &FxProfile) {
        *self.fx.lock().unwrap() = fx.clone();
    }

    /// A singer's saved profile lands on the device serving them —
    /// per-mic overrides sit above the room defaults (Python _src_fx).
    pub fn set_source_fx(&self, device: &str, fx: &FxProfile) {
        self.src_fx
            .lock()
            .unwrap()
            .insert(device.to_string(), fx.clone());
    }

    /// Effective FX for a device: its singer's profile where set,
    /// room defaults for anything they never customized. Read by the
    /// DSP chain once it lands (Python MicMonitor.fx_for).
    #[allow(dead_code)]
    pub fn fx_for(&self, device: &str) -> FxProfile {
        self.src_fx
            .lock()
            .unwrap()
            .get(device)
            .cloned()
            .unwrap_or_else(|| self.fx.lock().unwrap().clone())
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
