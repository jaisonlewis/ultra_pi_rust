//! ups-app — the Rust karaoke server. Serves the same web frontend and
//! REST/socket contract as the Python app: phones join on /, the kiosk
//! drives <video> on /tv, and player:position ticks feed scoring.

mod mic;
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
    let mut vocal_cut = std::env::var("UPS_VOCAL_CUT")
        .map(|v| v != "0")
        .unwrap_or(true);
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
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
        st.entries.len(),
        root.display()
    );
    eprintln!("ups-app: host key {}", st.host_key);
    eprintln!("ups-app: join url {}/", lan_addr(port));

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
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
        });
        sock::register(&io, app.clone());
        let router = web::router(app, layer);
        let addr: SocketAddr = format!("{bind}:{port}").parse().unwrap();
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .expect("bind");
        eprintln!("ups-app: listening on http://{addr}  (tv: /tv)");
        axum::serve(listener, router).await.expect("serve");
    });
}
