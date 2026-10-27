//! Audio output routing via PipeWire/WirePlumber (`wpctl`) — port of
//! `ultrapistar/audio.py`. On hosts without PipeWire everything
//! degrades to empty/absent so the host audio panel just hides.

use std::collections::BTreeMap;
use std::process::Command;

use serde_json::{json, Value};

pub fn available() -> bool {
    Command::new("wpctl")
        .arg("status")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn run(args: &[&str]) -> String {
    Command::new(args[0])
        .args(&args[1..])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default()
}

fn ok(args: &[&str]) -> bool {
    Command::new(args[0])
        .args(&args[1..])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// `wpctl status` → {section: [(id, name, is_default)]}. Item lines
/// look like ` │  *   53. Built-in Audio Analog Stereo [vol: 0.40]`.
fn sections() -> BTreeMap<String, Vec<(i64, String, bool)>> {
    let text = run(&["wpctl", "status"]);
    let mut out: BTreeMap<String, Vec<(i64, String, bool)>> =
        BTreeMap::new();
    let mut cur: Option<String> = None;
    for line in text.lines() {
        if let Some(pos) = line.find(':') {
            let head = line[..pos]
                .trim_start_matches(|c: char| "├─┤│ ".contains(c))
                .trim();
            if !head.is_empty()
                && head.chars().next().unwrap().is_ascii_uppercase()
                && line.contains('─')
            {
                cur = Some(head.trim_end_matches(':').to_string());
                out.entry(cur.clone().unwrap()).or_default();
                continue;
            }
        }
        let Some(sec) = &cur else { continue };
        if !(line.contains('│') || line.contains('*')) {
            continue;
        }
        let is_def = line.contains('*');
        // find the "<digits>." id token, then the name up to " ["
        let bytes = line.as_bytes();
        let mut i = 0usize;
        while i < bytes.len() {
            if bytes[i].is_ascii_digit() {
                let start = i;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                if i < bytes.len() && bytes[i] == b'.' {
                    if let Ok(id) = line[start..i].parse::<i64>() {
                        let name = line[i + 1..]
                            .split(" [")
                            .next()
                            .unwrap_or("")
                            .trim()
                            .to_string();
                        if !name.is_empty() {
                            out.entry(sec.clone())
                                .or_default()
                                .push((id, name, is_def));
                        }
                    }
                    break;
                }
            } else {
                i += 1;
            }
        }
    }
    out
}

/// pw-dump Node objects → (id, object.serial, node.name, media.class).
fn pw_nodes() -> Vec<(i64, i64, String, String)> {
    let text = run(&["pw-dump"]);
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    v.as_array()
        .map(|arr| {
            arr.iter()
                .filter(|o| o["type"] == "PipeWire:Interface:Node")
                .map(|o| {
                    let p = &o["info"]["props"];
                    (
                        o["id"].as_i64().unwrap_or(0),
                        p["object.serial"]
                            .as_str()
                            .and_then(|s| s.parse().ok())
                            .or_else(|| p["object.serial"].as_i64())
                            .unwrap_or(0),
                        p["node.name"].as_str().unwrap_or("").to_string(),
                        p["media.class"].as_str().unwrap_or("").to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// node.name -> wpctl-addressable serial (names survive restarts).
fn sink_names() -> BTreeMap<String, i64> {
    pw_nodes()
        .into_iter()
        .filter(|(_, _, _, mc)| mc == "Audio/Sink")
        .map(|(id, serial, name, _)| (name, if serial != 0 { serial } else { id }))
        .collect()
}

/// Live output sinks: id (wpctl serial), display name, default flag.
fn sinks() -> Vec<Value> {
    let names: BTreeMap<i64, String> = sink_names()
        .into_iter()
        .map(|(n, s)| (s, n))
        .collect();
    sections()
        .get("Sinks")
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|(id, name, def)| {
            json!({
                "id": id, "name": name, "default": def,
                "node": names.get(&id).cloned().unwrap_or_default(),
            })
        })
        .collect()
}

/// ALSA playback cards lacking a live PipeWire sink (profile off).
/// Reads monitor names from the HDMI ELD when present.
fn playback_cards() -> Vec<(i64, String)> {
    let mut out = Vec::new();
    for line in run(&["aplay", "-l"]).lines() {
        // "card 0: PCH [HDA Intel PCH], device 0: ..."
        let Some(rest) = line.strip_prefix("card ") else { continue };
        let Some(colon) = rest.find(':') else { continue };
        let Ok(card) = rest[..colon].parse::<i64>() else { continue };
        let name = rest[colon + 1..]
            .split('[')
            .nth(1)
            .and_then(|s| s.split(']').next())
            .unwrap_or("")
            .to_string();
        // ELD monitor name beats the codec name; an eld with no
        // monitor_name means nothing's plugged into that port
        let eld = format!("/proc/asound/card{card}/eld#0");
        let mut mon = String::new();
        let mut has_eld = false;
        if let Ok(txt) = std::fs::read_to_string(&eld) {
            has_eld = true;
            for l in txt.lines() {
                if let Some(m) = l.strip_prefix("monitor_name") {
                    mon = m.trim().to_string();
                    break;
                }
            }
        }
        if has_eld && mon.is_empty() {
            continue;
        }
        out.push((card, if !mon.is_empty() { mon } else { name }));
    }
    out
}

/// `alsa.card` index of a PipeWire device (`wpctl inspect`).
fn alsa_card_for(dev_id: i64) -> Option<i64> {
    let out = run(&["wpctl", "inspect", &dev_id.to_string()]);
    out.split("alsa.card")
        .nth(1)
        .and_then(|s| s.split('"').nth(1))
        .and_then(|s| s.parse().ok())
}

/// Selectable outputs for the host audio panel: live sinks + cards
/// whose profile needs enabling first.
pub fn playback_targets() -> Vec<Value> {
    let mut targets: Vec<Value> = sinks()
        .into_iter()
        .map(|s| {
            let mut s = s;
            s["kind"] = json!("sink");
            s
        })
        .collect();
    // alsa card of each pw device already exposing a sink
    let sink_devs: Vec<i64> = sinks()
        .iter()
        .filter_map(|s| s["id"].as_i64())
        .collect();
    let mut covered = std::collections::HashSet::new();
    let mut dev_cards = BTreeMap::new();
    for (dev_id, _, _) in sections().get("Devices").cloned().unwrap_or_default()
    {
        if let Some(c) = alsa_card_for(dev_id) {
            dev_cards.insert(dev_id, c);
        }
    }
    for sid in sink_devs {
        let out = run(&["wpctl", "inspect", &sid.to_string()]);
        if let Some(dev) = out
            .split("device.id")
            .nth(1)
            .and_then(|s| {
                s.split('=')
                    .nth(1)
                    .map(|x| x.trim().trim_matches('"'))
            })
            .and_then(|s| s.parse::<i64>().ok())
        {
            if let Some(c) = dev_cards.get(&dev) {
                covered.insert(*c);
            }
        }
    }
    for (card, name) in playback_cards() {
        if !covered.contains(&card) {
            targets.push(json!({
                "kind": "card", "card": card, "name": name,
                "default": false, "needs_enable": true,
            }));
        }
    }
    targets
}

fn enable_card(alsa_card: i64) -> bool {
    for (dev_id, _, _) in sections().get("Devices").cloned().unwrap_or_default()
    {
        if alsa_card_for(dev_id) == Some(alsa_card) {
            return ok(&["wpctl", "set-profile", &dev_id.to_string(), "1"]);
        }
    }
    false
}

/// Route all audio to a sink (or enable a card then take its sink).
/// kind "sink" = wpctl serial, "sink_name" = stable node.name (for
/// restoring saved choices — ids churn across restarts), "card" = ALSA
/// index. Returns {ok, name, node?} or {ok:false, error}.
pub fn select_output(kind: &str, ident: &Value) -> Value {
    let mut kind = kind.to_string();
    let mut ident = ident.clone();
    if kind == "sink_name" {
        let name = ident.as_str().unwrap_or("").to_string();
        match sink_names().get(&name).copied() {
            Some(sid) => {
                kind = "sink".into();
                ident = json!(sid);
            }
            None => return json!({"ok": false,
                                  "error": "saved sink not present"}),
        }
    }
    if kind == "sink" {
        let Some(sid) = ident.as_i64() else {
            return json!({"ok": false, "error": "sink id required"});
        };
        if !ok(&["wpctl", "set-default", &sid.to_string()]) {
            return json!({"ok": false,
                          "error": "wpctl set-default failed"});
        }
        let m = sinks()
            .into_iter()
            .find(|s| s["id"].as_i64() == Some(sid));
        return json!({
            "ok": true,
            "name": m.as_ref().map(|s| s["name"].clone())
                .unwrap_or(json!(sid)),
            "node": m.as_ref().map(|s| s["node"].clone())
                .unwrap_or(json!("")),
        });
    }
    if kind == "card" {
        let Some(card) = ident.as_i64() else {
            return json!({"ok": false, "error": "card id required"});
        };
        let before: std::collections::HashSet<i64> = sinks()
            .iter()
            .filter_map(|s| s["id"].as_i64())
            .collect();
        if !enable_card(card) {
            return json!({"ok": false,
                          "error": "could not enable card"});
        }
        for _ in 0..10 {   // the sink node appears asynchronously
            std::thread::sleep(std::time::Duration::from_millis(500));
            for s in sinks() {
                if let Some(id) = s["id"].as_i64() {
                    if !before.contains(&id) {
                        let _ = ok(&[
                            "wpctl", "set-default", &id.to_string(),
                        ]);
                        return json!({
                            "ok": true, "name": s["name"],
                            "node": s["node"],
                        });
                    }
                }
            }
        }
        return json!({"ok": false,
                      "error": "card enabled but no sink appeared"});
    }
    json!({"ok": false, "error": "unknown output kind"})
}
