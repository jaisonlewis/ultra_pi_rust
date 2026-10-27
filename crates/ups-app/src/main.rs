//! ups-app — the Rust karaoke server. Serves the same web frontend and
//! REST/socket contract as the Python app: phones join on /, the kiosk
//! drives <video> on /tv, and player:position ticks feed scoring.

mod audio;
mod mic;
mod monitor;
mod sock;
mod state;
mod web;

use std::net::{SocketAddr, UdpSocket};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;

use socketioxide::SocketIo;

use sock::App;
use state::State;

fn lan_addr(port: u16) -> String {
    // no traffic is sent — connect() just picks the outbound interface
    let ip = UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| {
            s.connect("192.168.255.255:80")?;
            s.local_addr()
        })
        .map(|a| a.ip().to_string())
        .unwrap_or_else(|_| "127.0.0.1".into());
    format!("http://{ip}:{port}")
}

fn host_key() -> String {
    if let Ok(k) = std::env::var("UPS_HOST_KEY") {
        if !k.is_empty() {
            return k;
        }
    }
    // 4-char code from the unambiguous alphabet, same as Python
    const ABC: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut key = String::with_capacity(4);
    let mut seed = crate::state::now_secs() as u64;
    for _ in 0..4 {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        key.push(ABC[(seed >> 33) as usize % ABC.len()] as char);
    }
    key
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut root = PathBuf::from(".");
    let mut port: u16 = 5000;
    let mut bind = "0.0.0.0".to_string();
    let mut difficulty: i64 = 1;
    let mut intermission_sec = 30.0f64;
    let mut native_tv = false;
    let mut vocal_cut = std::env::var("UPS_VOCAL_CUT")
        .map(|v| v != "0")
        .unwrap_or(true);
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--tv" => native_tv = true,
            "--root" | "--media" => {
                i += 1;
                root = PathBuf::from(&args[i]);
            }
            "--port" => {
                i += 1;
                port = args[i].parse().unwrap_or(5000);
            }
            "--bind" => {
                i += 1;
                bind = args[i].clone();
            }
            "--difficulty" => {
                i += 1;
                difficulty = args[i].parse().unwrap_or(1);
            }
            "--intermission" => {
                i += 1;
                intermission_sec = args[i].parse().unwrap_or(30.0);
            }
            "--no-vocal-cut" => vocal_cut = false,
            "--help" | "-h" => {
                eprintln!(
                    "ups-app — Ultra Pi Star karaoke server\n\
                     usage: ups-app --root <library> [--port 5000] \\\n\
                     \x20      [--bind 0.0.0.0] [--difficulty 0..2] \\\n\
                     \x20      [--intermission sec] [--no-vocal-cut]"
                );
                return;
            }
            other if !other.starts_with('-') => root = PathBuf::from(other),
            _ => {}
        }
        i += 1;
    }
    if let Ok(v) = std::env::var("UPS_PORT") {
        port = v.parse().unwrap_or(port);
    }
    let build_v = format!(
        "rust-{}-{}",
        env!("CARGO_PKG_VERSION"),
        crate::state::now_secs() as u64 % 1_000_000
    );
    let state_path = root.join("ups-state.json");
    let st = State::new(
        root.clone(),
        host_key(),
        build_v,
        difficulty.clamp(0, 2),
        vocal_cut,
        intermission_sec,
        state_path,
    );
    eprintln!(
        "ups-app: {} songs under {}",
        st.entries.read().unwrap().len(),
        root.display()
    );
    eprintln!("ups-app: host key {}", st.host_key);
    eprintln!("ups-app: join url {}/", lan_addr(port));
    // mic passthrough — singers hear themselves (PipeWire only)
    let mon = monitor::start();
    // restore the host's saved audio output (sink_name survives
    // restarts where wpctl ids don't)
    if let Some(saved) = st.output_target.lock().unwrap().clone() {
        if audio::available() {
            if let Ok(v) =
                serde_json::from_str::<serde_json::Value>(&saved)
            {
                if let Some(n) = v["sink_name"].as_str() {
                    let _ = audio::select_output(
                        "sink_name",
                        &serde_json::json!(n),
                    );
                } else if let Some(c) = v["card"].as_i64() {
                    let _ = audio::select_output(
                        "card",
                        &serde_json::json!(c),
                    );
                }
            }
        }
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    std::thread::spawn(move || {
        rt.block_on(async move {
            let join_url = lan_addr(port) + "/";
            let (layer, io) = SocketIo::new_layer();
            let app = Arc::new(App {
                st,
                io: io.clone(),
                join_url,
                sing: Mutex::new(None),
                tv_sid: Mutex::new(None),
                music_volume: Mutex::new(1.0),
                mic_delay_ms: Mutex::new(40.0),
                auto_next: Mutex::new(0.0),
                native_tv,
                paused: std::sync::atomic::AtomicBool::new(false),
                sync_override: Mutex::new(None),
                resync_req: std::sync::atomic::AtomicBool::new(false),
                monitor: mon,
            });
            // the loopback's playback node lands a beat after spawn —
            // push the stored volume/mute onto it once it exists
            if app.monitor.is_some() {
                let a = app.clone();
                std::thread::spawn(move || {
                    a.monitor.as_ref().unwrap().apply_when_ready();
                });
            }
            sock::register(&io, app.clone());
            // hot rescan — drop a folder on the stick and it shows up
            // without a restart. UPS_RESCAN_SEC=0 disables.
            let rescan_sec = std::env::var("UPS_RESCAN_SEC")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(15);
            if rescan_sec > 0 {
                let a = app.clone();
                tokio::spawn(async move {
                    let mut iv = tokio::time::interval(
                        std::time::Duration::from_secs(rescan_sec),
                    );
                    iv.tick().await; // first tick is immediate — skip it
                    loop {
                        iv.tick().await;
                        let aa = a.clone();
                        let r = tokio::task::spawn_blocking(move || {
                            aa.st.rescan()
                        })
                        .await;
                        if let Ok(r) = r {
                            if r.added + r.removed + r.updated > 0 {
                                eprintln!(
                                    "rescan: +{} -{} ~{} songs ({} total)",
                                    r.added, r.removed, r.updated, r.total
                                );
                                let _ = a
                                    .io
                                    .emit("state", &a.st.snapshot())
                                    .await;
                            }
                        }
                    }
                });
            }
            let router = web::router(app.clone(), layer);
            let addr: SocketAddr = format!("{bind}:{port}").parse().unwrap();
            let listener = tokio::net::TcpListener::bind(addr)
                .await
                .expect("bind");
            eprintln!("ups-app: listening on http://{addr}  (tv: /tv)");
            // hand the app back so the main thread can run the renderer
            APP_SLOT.set(app).ok();
            axum::serve(listener, router).await.expect("serve");
        });
    });

    if native_tv {
        // wait until the server thread publishes the App handle
        let app = loop {
            if let Some(a) = APP_SLOT.get() {
                break a.clone();
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        };
        eprintln!("ups-app: native tv window starting");
        loop {
            let drv = app.clone();
            // catch_unwind: a window-creation panic (no display yet,
            // wayland restart) must not take the server down with it
            let r = std::panic::catch_unwind(
                std::panic::AssertUnwindSafe(move || ups_tv::run(drv)),
            );
            match r {
                Ok(ups_tv::Exit::Quit) => break,
                Ok(ups_tv::Exit::Lost) => {
                    eprintln!("ups-app: tv window lost — reopening in 3s")
                }
                Err(_) => eprintln!(
                    "ups-app: tv renderer panicked — retrying in 3s"
                ),
            }
            std::thread::sleep(std::time::Duration::from_secs(3));
        }
    } else {
        // no renderer — park main thread forever
        loop {
            std::thread::sleep(std::time::Duration::from_secs(3600));
        }
    }
}

static APP_SLOT: std::sync::OnceLock<Arc<App>> = std::sync::OnceLock::new();

