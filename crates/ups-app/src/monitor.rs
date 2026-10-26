//! Speaker monitor — singers hear themselves through the speakers.
//! Port of Python's MicMonitor dry path: `pw-loopback` does the
//! mic→sink route inside PipeWire at ~10ms latency (a userspace
//! capture→playback loop reads as distracting slapback). No capture
//! target: WirePlumber auto-links to the default source, the same one
//! cpal's "pipewire" input opens — so monitor and scoring always hear
//! the same mic, and both follow wpctl's default-source selection.

use std::process::{Child, Command, Stdio};

pub struct Monitor {
    child: Child,
}

/// Spawn the loopback. Returns None where PipeWire tools don't exist
/// (dev machines) — scoring capture keeps working without it.
pub fn start() -> Option<Monitor> {
    let child = Command::new("pw-loopback")
        .args(["-n", "ups-monitor", "-c", "1", "-l", "10"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    // playback node appears async — apply Python's default 0.8 gain
    std::thread::spawn(|| apply_volume("output.ups-monitor", 0.8));
    eprintln!("monitor: pw-loopback ups-monitor running");
    Some(Monitor { child })
}

fn apply_volume(node_name: &str, volume: f64) {
    for _ in 0..40 {
        if let Some(id) = node_id(node_name) {
            let id = id.to_string();
            let _ = Command::new("wpctl")
                .args(["set-mute", &id, "0"])
                .status();
            let _ = Command::new("wpctl")
                .args(["set-volume", &id, &format!("{volume:.2}")])
                .status();
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
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

impl Drop for Monitor {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}
