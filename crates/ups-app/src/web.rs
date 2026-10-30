//! axum HTTP layer — port of web/pages.py + web/api.py. Serves the
//! vendored frontend verbatim; REST shapes match what phone.js/host.js
//! consume. Where Python's sqlite-backed features map onto the
//! file-based catalog (manage songs, chart verification, uploads,
//! sync offsets), the response rows keep the same field names so the
//! frontend can't tell the difference.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path, Query, Request, State as AxState};
use axum::http::header;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Json, Response};
use axum::routing::{get, post};
use axum::Router;
use rust_embed::Embed;
use serde::Deserialize;
use serde_json::{json, Value};
use tower::ServiceExt;
use tower_http::services::ServeFile;

use crate::sock::{self, App};

#[derive(Embed)]
#[folder = "web/"]
struct Web;

// -- shared helpers ----------------------------------------------------------

fn sid_of(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    for pair in raw.split(';') {
        let pair = pair.trim();
        if let Some(rest) = pair.strip_prefix("ups_sid=") {
            return Some(rest.to_string());
        }
    }
    None
}

fn ensure_sid(app: &App, headers: &HeaderMap) -> (String, bool) {
    if let Some(sid) = sid_of(headers) {
        // adopt the client's sid — minting a new one here would orphan
        // every API caller after a restart (the Set-Cookie only ever
        // goes out on page loads), leaving joins that can't queue.
        app.st.sessions.lock().unwrap()
            .entry(sid.clone())
            .or_insert_with(crate::state::SessionCookie::default);
        return (sid, false);
    }
    let sid = format!("{:x}{:x}", crate::state::now_secs() as u64,
                      app.st.sessions.lock().unwrap().len());
    app.st.sessions.lock().unwrap().insert(
        sid.clone(),
        crate::state::SessionCookie::default(),
    );
    (sid, true)
}

fn sess(app: &App, sid: &str) -> crate::state::SessionCookie {
    app.st
        .sessions
        .lock()
        .unwrap()
        .get(sid)
        .cloned()
        .unwrap_or_default()
}

fn is_host(app: &App, headers: &HeaderMap, sid: &str) -> bool {
    if headers
        .get("x-host-key")
        .and_then(|v| v.to_str().ok())
        .map(|k| k == app.st.host_key)
        .unwrap_or(false)
    {
        return true;
    }
    sess(app, sid).is_host
}

fn is_singer(app: &App, sid: &str) -> bool {
    let Some(pid) = sess(app, sid).performer_id else {
        return false;
    };
    app.st
        .now_playing()
        .map(|i| i.performer_id == pid || i.performer2_id == Some(pid))
        .unwrap_or(false)
}

fn render(_app: &App, name: &str, ctx: minijinja::Value) -> String {
    // Jinja templates use {% extends "base.html" %} — register every
    // embedded template on a shared env so inheritance resolves.
    use std::sync::OnceLock;
    static ENV: OnceLock<minijinja::Environment<'static>> = OnceLock::new();
    let env = ENV.get_or_init(|| {
        let mut env = minijinja::Environment::new();
        // Flask url_for('static', filename=..., v=...) equivalent
        env.add_function(
            "url_for",
            |_ep: String, kwargs: minijinja::value::Kwargs| {
                let filename: String =
                    kwargs.get("filename").unwrap_or_default();
                let v: String = kwargs.get("v").unwrap_or_default();
                let _ = kwargs.assert_all_used();
                format!("/static/{filename}?v={v}")
            },
        );
        for path in Web::iter() {
            if let Some(tpl) = path.strip_prefix("templates/") {
                if let Some(f) = Web::get(&path) {
                    let src = String::from_utf8_lossy(f.data.as_ref());
                    let _ = env.add_template_owned(tpl.to_string(), src.into_owned());
                }
            }
        }
        env
    });
    env.get_template(name)
        .and_then(|t| t.render(&ctx))
        .unwrap_or_else(|e| format!("template error in {name}: {e}"))
}

// -- pages -------------------------------------------------------------------

async fn page_phone(AxState(app): AxState<Arc<App>>, headers: HeaderMap,
                    Query(q): Query<BTreeMap<String, String>>)
    -> impl IntoResponse {
    let (sid, fresh) = ensure_sid(&app, &headers);
    // ?key= host gate shortcut
    if q.get("key").map(|k| k.as_str()) == Some(app.st.host_key.as_str()) {
        let mut s = sess(&app, &sid);
        s.is_host = true;
        app.st.sessions.lock().unwrap().insert(sid.clone(), s);
    }
    let ctx = minijinja::context! {
        v => app.st.build_v.clone(),
        join_url => app.join_url.clone(),
        qr => "",
        host_key => app.st.host_key.clone(),
    };
    let html = render(&app, "phone.html", ctx);
    let mut resp = Html(html).into_response();
    if fresh {
        resp.headers_mut().insert(
            header::SET_COOKIE,
            format!("ups_sid={sid}; Path=/; Max-Age=2592000; SameSite=Lax")
                .parse().unwrap(),
        );
    }
    resp
}

async fn page_tv(AxState(app): AxState<Arc<App>>) -> impl IntoResponse {
    let qr = qr_svg(&app.join_url);
    let ctx = minijinja::context! {
        v => app.st.build_v.clone(),
        join_url => app.join_url.clone(),
        qr => qr,
        host_key => app.st.host_key.clone(),
    };
    Html(render(&app, "tv.html", ctx))
}

async fn page_host(AxState(app): AxState<Arc<App>>, headers: HeaderMap,
                   Query(q): Query<BTreeMap<String, String>>)
    -> impl IntoResponse {
    let (sid, fresh) = ensure_sid(&app, &headers);
    let mut cookie = sess(&app, &sid);
    if q.get("key").map(|k| k.as_str()) == Some(app.st.host_key.as_str()) {
        cookie.is_host = true;
        app.st.sessions.lock().unwrap().insert(sid.clone(), cookie.clone());
    }
    let name = if cookie.is_host { "host.html" } else { "host_gate.html" };
    let ctx = minijinja::context! {
        v => app.st.build_v.clone(),
        join_url => app.join_url.clone(),
        host_key => app.st.host_key.clone(),
    };
    let html = render(&app, name, ctx);
    let mut resp = Html(html).into_response();
    if fresh {
        resp.headers_mut().insert(
            header::SET_COOKIE,
            format!("ups_sid={sid}; Path=/; Max-Age=2592000; SameSite=Lax")
                .parse().unwrap(),
        );
    }
    resp
}

async fn static_file(Path(path): Path<String>) -> impl IntoResponse {
    let Some(file) = Web::get(&format!("static/{path}")) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    let mime = if path.ends_with(".js") {
        "application/javascript"
    } else if path.ends_with(".css") {
        "text/css"
    } else if path.ends_with(".png") {
        "image/png"
    } else if path.ends_with(".woff2") {
        "font/woff2"
    } else {
        "application/octet-stream"
    };
    (
        [(header::CONTENT_TYPE, mime),
         (header::CACHE_CONTROL, "no-cache")],
        file.data.to_vec(),
    )
        .into_response()
}

fn qr_svg(data: &str) -> String {
    use base64::Engine;
    let Ok(code) = qrcode::QrCode::new(data.as_bytes()) else {
        return String::new();
    };
    let svg = code
        .render::<qrcode::render::svg::Color>()
        .min_dimensions(260, 260)
        .build();
    format!(
        "data:image/svg+xml;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(svg)
    )
}

// -- media -------------------------------------------------------------------

/// GET /media/e<id>?v=inst — ServeFile handles Range so <video>
/// seeking and resumable loads work like Flask's send_file.
async fn media_file(AxState(app): AxState<Arc<App>>, Path(id): Path<i64>,
                    Query(q): Query<BTreeMap<String, String>>, req: Request)
    -> Response {
    let path: PathBuf = {
        let entries = app.st.entries.read().unwrap();
        let Some(entry) = entries.iter().find(|e| e.id == id) else {
            return (StatusCode::NOT_FOUND, "no entry").into_response();
        };
        if q.get("v").map(|v| v.as_str()) == Some("inst") {
            entry.inst.clone().unwrap_or_else(|| entry.media.clone())
        } else {
            entry.media.clone()
        }
    };
    match ServeFile::new(&path).oneshot(req).await {
        Ok(r) => r.into_response(),
        Err(e) => (StatusCode::NOT_FOUND, format!("{e}")).into_response(),
    }
}

async fn art_file(AxState(app): AxState<Arc<App>>, Path(fname): Path<String>,
                  req: Request)
    -> Response {
    // /api/art/<id>.jpg — id-keyed so no path traversal is possible
    let stem = fname.trim_end_matches(".jpg");
    let Ok(id) = stem.parse::<i64>() else {
        return (StatusCode::NOT_FOUND, "bad art id").into_response();
    };
    let path = {
        let entries = app.st.entries.read().unwrap();
        entries
            .iter()
            .find(|e| e.id == id)
            .and_then(|e| e.art.clone())
    };
    let Some(path) = path else {
        return (StatusCode::NOT_FOUND, "no art").into_response();
    };
    match ServeFile::new(&path).oneshot(req).await {
        Ok(r) => r.into_response(),
        Err(e) => (StatusCode::NOT_FOUND, format!("{e}")).into_response(),
    }
}

// -- identity ----------------------------------------------------------------

async fn api_me(AxState(app): AxState<Arc<App>>, headers: HeaderMap)
    -> impl IntoResponse {
    let (sid, _) = ensure_sid(&app, &headers);
    let c = sess(&app, &sid);
    let perf = c
        .performer_id
        .and_then(|id| app.st.performer(id))
        .map(|p| json!({"id": p.id, "name": p.name, "color": p.color}));
    Json(json!({
        "performer": perf,
        "is_host": is_host(&app, &headers, &sid),
        "difficulty": app.st.difficulty,
        "vocal_cut": app.st.vocal_cut_default,
        "build": {"v": app.st.build_v},
    }))
}

async fn api_join(AxState(app): AxState<Arc<App>>, headers: HeaderMap,
                  Json(body): Json<Value>)
    -> impl IntoResponse {
    let (sid, _) = ensure_sid(&app, &headers);
    let name = body["name"].as_str().unwrap_or("").trim().to_string();
    if name.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "name required"})),
        )
            .into_response();
    }
    let p = app.st.add_performer(&name[..name.len().min(24)]);
    let mut c = sess(&app, &sid);
    c.performer_id = Some(p.id);
    app.st.sessions.lock().unwrap().insert(sid.clone(), c);
    let _ = app.io.emit("state", &app.st.snapshot()).await;
    Json(json!({"performer": {"id": p.id, "name": p.name, "color": p.color}}))
        .into_response()
}

async fn api_host_login(AxState(app): AxState<Arc<App>>, headers: HeaderMap,
                        Json(body): Json<Value>)
    -> impl IntoResponse {
    let (sid, _) = ensure_sid(&app, &headers);
    let key = body["key"].as_str().unwrap_or("").trim();
    if key != app.st.host_key {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "wrong host key"})),
        )
            .into_response();
    }
    let mut c = sess(&app, &sid);
    if c.performer_id.is_none() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "join with a display name first"})),
        )
            .into_response();
    }
    if let Some(cur) = *app.st.host_performer_id.lock().unwrap() {
        if Some(cur) != c.performer_id {
            let name = app
                .st
                .performer(cur)
                .map(|p| p.name)
                .unwrap_or_else(|| "someone".into());
            return (
                StatusCode::CONFLICT,
                Json(json!({"error": format!("host is already {name}")})),
            )
                .into_response();
        }
    }
    c.is_host = true;
    let perf_id = c.performer_id;
    app.st.sessions.lock().unwrap().insert(sid.clone(), c);
    *app.st.host_performer_id.lock().unwrap() = perf_id;
    app.st.persist();
    let _ = app.io.emit("host:state", &json!({
        "claimed": true, "key": app.st.host_key,
    })).await;
    Json(json!({"ok": true})).into_response()
}

async fn api_host_release(AxState(app): AxState<Arc<App>>, headers: HeaderMap)
    -> impl IntoResponse {
    let (sid, _) = ensure_sid(&app, &headers);
    if !is_host(&app, &headers, &sid) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "host key required"})),
        )
            .into_response();
    }
    let mut c = sess(&app, &sid);
    c.is_host = false;
    app.st.sessions.lock().unwrap().insert(sid.clone(), c);
    *app.st.host_performer_id.lock().unwrap() = None;
    app.st.persist();
    let _ = app.io.emit("host:state", &json!({
        "claimed": false, "key": app.st.host_key,
    })).await;
    Json(json!({"ok": true})).into_response()
}

async fn api_session_end(AxState(app): AxState<Arc<App>>, headers: HeaderMap)
    -> impl IntoResponse {
    let (sid, _) = ensure_sid(&app, &headers);
    if !is_host(&app, &headers, &sid) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "host key required"})),
        )
            .into_response();
    }
    app.st.performers.lock().unwrap().clear();
    app.st.queue.lock().unwrap().clear();
    app.st.sessions.lock().unwrap().values_mut().for_each(|s| {
        s.performer_id = None;
        s.is_host = false;
    });
    *app.st.host_performer_id.lock().unwrap() = None;
    app.sing.lock().unwrap().take();
    app.st.persist();
    let _ = app.io.emit("session:ended", &Value::Null).await;
    let _ = app.io.emit("state", &app.st.snapshot()).await;
    Json(json!({"ok": true})).into_response()
}

// -- catalog -----------------------------------------------------------------

async fn api_search(AxState(app): AxState<Arc<App>>,
                    Query(q): Query<BTreeMap<String, String>>)
    -> impl IntoResponse {
    let query = q.get("q").cloned().unwrap_or_default().to_lowercase();
    let tops = app.st.top_scores();
    let results: Vec<Value> = app
        .st
        .entries
        .read()
        .unwrap()
        .iter()
        .filter(|e| {
            query.is_empty()
                || e.title.to_lowercase().contains(&query)
                || e.artist.to_lowercase().contains(&query)
        })
        .map(|e| e.row(tops.get(&e.song_id)))
        .collect();
    Json(json!({"results": results}))
}

async fn api_library(AxState(app): AxState<Arc<App>>) -> impl IntoResponse {
    let tops = app.st.top_scores();
    Json(json!({
        "results": app.st.entries.read().unwrap().iter()
            .map(|e| e.row(tops.get(&e.song_id))).collect::<Vec<_>>()
    }))
}

async fn api_scores(AxState(app): AxState<Arc<App>>) -> impl IntoResponse {
    // leaderboard(): per-song best + performer standings
    let hist = app.st.history.lock().unwrap().clone();
    let mut best: BTreeMap<i64, &crate::state::HistoryRow> = BTreeMap::new();
    for h in &hist {
        if h.score <= 0 {
            continue;
        }
        let e = best.entry(h.song_id).or_insert(h);
        if h.score > e.score {
            *e = h;
        }
    }
    let tops: Vec<Value> = best
        .values()
        .map(|h| {
            let e = {
                let entries = app.st.entries.read().unwrap();
                entries.iter().find(|e| e.song_id == h.song_id)
                    .map(|e| (e.artist.clone(), e.title.clone()))
            };
            let p1 = app.st.performer(h.performer_id);
            let p2 = h.performer2_id.and_then(|id| app.st.performer(id));
            let name = match (p1, p2) {
                (Some(a), Some(b)) => format!("{} & {}", a.name, b.name),
                (Some(a), None) => a.name,
                _ => "?".into(),
            };
            json!({
                "song_id": h.song_id,
                "artist": e.as_ref().map(|f| f.0.clone()).unwrap_or_default(),
                "title": e.as_ref().map(|f| f.1.clone()).unwrap_or_default(),
                "score": h.score,
                "scorer": name,
                "played_at": h.played_at,
            })
        })
        .collect();
    let mut totals: BTreeMap<i64, (i64, i64)> = BTreeMap::new();
    for h in &hist {
        if h.score <= 0 {
            continue;
        }
        for pid in [h.performer_id].into_iter().chain(h.performer2_id) {
            let e = totals.entry(pid).or_default();
            e.0 += h.score;
            e.1 += 1;
        }
    }
    let mut leaders: Vec<Value> = totals
        .into_iter()
        .map(|(pid, (pts, songs))| {
            let p = app.st.performer(pid);
            json!({
                "performer_id": pid,
                "name": p.map(|p| p.name).unwrap_or_else(|| "?".into()),
                "points": pts,
                "songs": songs,
            })
        })
        .collect();
    leaders.sort_by(|a, b| b["points"].as_i64().cmp(&a["points"].as_i64()));
    Json(json!({"top": tops, "leaders": leaders}))
}

// -- queue -------------------------------------------------------------------

#[derive(Deserialize)]
struct QueueBody {
    name: Option<String>,
    name2: Option<String>,
    part: Option<String>,
    song_id: Option<i64>,
    entry_id: Option<i64>,
    vocal_cut: Option<Value>,
    lyrics_rom: Option<Value>,
}

async fn api_queue_add(AxState(app): AxState<Arc<App>>, headers: HeaderMap,
                       Json(body): Json<QueueBody>)
    -> Response {
    let (sid, _) = ensure_sid(&app, &headers);
    let mut c = sess(&app, &sid);
    if let Some(name) = body.name.as_ref().map(|n| n.trim().to_string()) {
        if !name.is_empty() {
            let p = app.st.add_performer(&name[..name.len().min(24)]);
            c.performer_id = Some(p.id);
            app.st.sessions.lock().unwrap().insert(sid.clone(), c.clone());
        }
    }
    let Some(perf_id) = c.performer_id else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "join with a display name first"})),
        )
            .into_response();
    };
    let entry_id = body.entry_id.or(body.song_id).unwrap_or(-1);
    let entry = {
        let entries = app.st.entries.read().unwrap();
        entries
            .iter()
            .find(|e| e.id == entry_id || e.song_id == entry_id)
            .map(|e| (e.id, e.is_duet))
    };
    let Some((entry_id, is_duet)) = entry else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "unknown catalog entry"})),
        )
            .into_response();
    };
    let perf2_id = if is_duet {
        body.name2
            .as_ref()
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty())
            .map(|n| app.st.add_performer(&n[..n.len().min(24)]).id)
    } else {
        None
    };
    let to_flag = |v: &Option<Value>| -> Option<i64> {
        v.as_ref().map(|v| {
            if v.as_bool().unwrap_or(v.as_i64().unwrap_or(0) != 0) {
                1
            } else {
                0
            }
        })
    };
    let id = app.st.queue_add(
        entry_id,
        perf_id,
        perf2_id,
        body.part.clone().unwrap_or_default().trim().to_string(),
        to_flag(&body.vocal_cut),
        to_flag(&body.lyrics_rom),
    );
    // idle queue auto-starts — same as Python queue_add's tail
    if app.st.now_playing().is_none() {
        app.st.start_next();
        Box::pin(sock::push_now_playing(&app)).await;
    } else {
        let _ = app.io.emit("state", &app.st.snapshot()).await;
    }
    Json(json!({"ok": true, "queue_item_id": id, "state": app.st.snapshot()}))
        .into_response()
}

async fn api_queue_list(AxState(app): AxState<Arc<App>>) -> impl IntoResponse {
    Json(app.st.snapshot())
}

async fn api_queue_remove(AxState(app): AxState<Arc<App>>,
                          headers: HeaderMap, Path(id): Path<i64>)
    -> Response {
    let (sid, _) = ensure_sid(&app, &headers);
    let item = {
        let mut q = app.st.queue.lock().unwrap();
        let Some(pos) = q.iter().position(|i| {
            i.id == id && i.status == "queued"
        }) else {
            return (StatusCode::NOT_FOUND, Json(json!({"error": "not found"})))
                .into_response();
        };
        let it = q[pos].clone();
        let mine = sess(&app, &sid).performer_id == Some(it.performer_id);
        if !mine && !is_host(&app, &headers, &sid) {
            return (
                StatusCode::FORBIDDEN,
                Json(json!({"error": "not your song"})),
            )
                .into_response();
        }
        q.remove(pos);
        it
    };
    app.st.persist();
    let _ = app.io.emit("item:update", &json!({"id": item.id})).await;
    let _ = app.io.emit("state", &app.st.snapshot()).await;
    Json(json!({"ok": true})).into_response()
}

async fn api_queue_join_duet(AxState(app): AxState<Arc<App>>,
                             headers: HeaderMap, Path(id): Path<i64>)
    -> Response {
    let (sid, _) = ensure_sid(&app, &headers);
    let Some(pid) = sess(&app, &sid).performer_id else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "join with a display name first"})),
        )
            .into_response();
    };
    let row = {
        let mut q = app.st.queue.lock().unwrap();
        let Some(it) = q.iter_mut().find(|i| i.id == id) else {
            return (StatusCode::NOT_FOUND, Json(json!({"error": "not found"})))
                .into_response();
        };
        let open = app
            .st
            .entries
            .read()
            .unwrap()
            .iter()
            .find(|e| e.id == it.entry_id)
            .map(|e| e.is_duet)
            .unwrap_or(false)
            && it.performer2_id.is_none();
        if !open {
            return (
                StatusCode::CONFLICT,
                Json(json!({"error": "not an open duet slot"})),
            )
                .into_response();
        }
        it.performer2_id = Some(pid);
        app.st.item_row(it)
    };
    app.st.persist();
    let _ = app.io.emit("state", &app.st.snapshot()).await;
    Json(json!({"ok": true, "item": row})).into_response()
}

async fn api_queue_move(AxState(app): AxState<Arc<App>>, headers: HeaderMap,
                        Path(id): Path<i64>, Json(body): Json<Value>)
    -> Response {
    let (sid, _) = ensure_sid(&app, &headers);
    if !is_host(&app, &headers, &sid) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "host key required"})),
        )
            .into_response();
    }
    let dir = body["dir"].as_str().unwrap_or("");
    {
        let mut q = app.st.queue.lock().unwrap();
        let mut idx: Vec<usize> = q
            .iter()
            .enumerate()
            .filter(|(_, i)| i.status == "queued")
            .map(|(i, _)| i)
            .collect();
        idx.sort_by(|a, b| {
            q[*a].position.partial_cmp(&q[*b].position).unwrap()
        });
        let Some(pos) = idx.iter().position(|&i| q[i].id == id) else {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "cannot move"})),
            )
                .into_response();
        };
        let swap = if dir == "up" && pos > 0 {
            Some((idx[pos], idx[pos - 1]))
        } else if dir == "down" && pos + 1 < idx.len() {
            Some((idx[pos], idx[pos + 1]))
        } else {
            None
        };
        let Some((a, b)) = swap else {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "cannot move"})),
            )
                .into_response();
        };
        q.swap(a, b);
        let (pa, pb) = (q[a].position, q[b].position);
        q[a].position = pb;
        q[b].position = pa;
    }
    app.st.persist();
    let _ = app.io.emit("state", &app.st.snapshot()).await;
    Json(json!({"ok": true})).into_response()
}

async fn api_queue_clear(AxState(app): AxState<Arc<App>>, headers: HeaderMap)
    -> impl IntoResponse {
    let (sid, _) = ensure_sid(&app, &headers);
    if !is_host(&app, &headers, &sid) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "host key required"})),
        )
            .into_response();
    }
    let n = {
        let mut q = app.st.queue.lock().unwrap();
        let before = q.len();
        q.retain(|i| i.status == "playing");
        before - q.len()
    };
    app.st.persist();
    let _ = app.io.emit("state", &app.st.snapshot()).await;
    Json(json!({"ok": true, "cleared": n})).into_response()
}

/// POST /api/catalog/rescan — hot rescan of the media root: new song
/// folders appear live, dropped folders disappear, sidecars that
/// arrived mid-session upgrade their entry. Host-only like Python;
/// `charts_added` maps to entries that gained a chart (the host page
/// toasts that count).
async fn api_catalog_rescan(AxState(app): AxState<Arc<App>>,
                            headers: HeaderMap) -> Response {
    let (sid, _) = ensure_sid(&app, &headers);
    if !is_host(&app, &headers, &sid) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "host key required"})),
        )
            .into_response();
    }
    let a = app.clone();
    let r = match tokio::task::spawn_blocking(move || a.st.rescan()).await {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("rescan failed: {e}")})),
            )
                .into_response();
        }
    };
    if r.added + r.removed + r.updated > 0 {
        let _ = app.io.emit("state", &app.st.snapshot()).await;
    }
    Json(json!({
        "ok": true,
        "added": r.added,
        "removed": r.removed,
        "updated": r.updated,
        "charts_added": r.upgraded,
        "songs": r.total,
        "root": app.st.root(),
    }))
    .into_response()
}

// -- playback ----------------------------------------------------------------

async fn api_pb_next(AxState(app): AxState<Arc<App>>, headers: HeaderMap)
    -> impl IntoResponse {
    let (sid, _) = ensure_sid(&app, &headers);
    if !is_host(&app, &headers, &sid) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "host key required"})),
        )
            .into_response();
    }
    app.sing.lock().unwrap().take();
    let cur = if app.st.now_playing().is_some() {
        app.st.finish_current("skipped", None, None)
    } else {
        app.st.start_next()
    };
    sock::push_now_playing(&app).await;
    Json(json!({"ok": true, "now_playing":
        cur.map(|i| app.st.item_row(&i))})).into_response()
}

async fn api_pb_pause(AxState(app): AxState<Arc<App>>, headers: HeaderMap)
    -> impl IntoResponse {
    let (sid, _) = ensure_sid(&app, &headers);
    if !is_host(&app, &headers, &sid) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "host key required"})),
        )
            .into_response();
    }
    app.paused.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = app.io.emit("player:pause", &Value::Null).await;
    Json(json!({"ok": true})).into_response()
}

async fn api_pb_resume(AxState(app): AxState<Arc<App>>, headers: HeaderMap)
    -> impl IntoResponse {
    let (sid, _) = ensure_sid(&app, &headers);
    if !is_host(&app, &headers, &sid) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "host key required"})),
        )
            .into_response();
    }
    app.paused.store(false, std::sync::atomic::Ordering::Relaxed);
    let _ = app.io.emit("player:resume", &Value::Null).await;
    Json(json!({"ok": true})).into_response()
}

async fn api_pb_volume(AxState(app): AxState<Arc<App>>, headers: HeaderMap,
                       Json(body): Json<Value>)
    -> impl IntoResponse {
    let (sid, _) = ensure_sid(&app, &headers);
    if !is_host(&app, &headers, &sid) && !is_singer(&app, &sid) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "host or current singer required"})),
        )
            .into_response();
    }
    let Some(vol) = body["volume"].as_f64() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "volume 0..1 required"})),
        )
            .into_response();
    };
    let vol = vol.clamp(0.0, 1.0);
    *app.music_volume.lock().unwrap() = vol;
    let _ = app.io.emit("player:volume", &json!(vol)).await;
    Json(json!({"ok": true, "music_volume": vol})).into_response()
}

#[derive(Deserialize)]
struct SyncBody {
    delta_ms: Option<i64>,
}

/// lyrics payload for the entry now playing, honoring stored sync +
/// rom flags — re-sent on every sync nudge so the TV swaps lyrics
/// without restarting the media.
fn lyrics_payload_for(app: &App, item: &crate::state::QueueItem,
                     entry_id: i64) -> Option<Value> {
    let (chart_path, rom, off) = {
        let entries = app.st.entries.read().unwrap();
        let e = entries.iter().find(|e| e.id == entry_id)?;
        (
            e.chart.clone()?,
            item.lyrics_rom.map(|v| v != 0).unwrap_or(false),
            app.st.sync_offset(e.id),
        )
    };
    let c = ups_core::chart::load_chart(&chart_path);
    Some(ups_core::chart::lane_payload(&c, rom, off, &[]))
}

/// GET /api/playback/sync — current offset for the playing chart;
/// any client may read (the host UI polls it when lyrics appear).
async fn api_pb_sync_get(AxState(app): AxState<Arc<App>>)
    -> impl IntoResponse {
    let off = app
        .st
        .now_playing()
        .map(|it| app.st.sync_offset(it.entry_id));
    Json(json!({
        "sync_offset_ms": off,
        "autosync_mic": *app.st.autosync_mic.lock().unwrap(),
    }))
}

/// POST /api/playback/sync — delta_ms is a relative nudge onto the
/// entry's stored offset (chart row's sync_offset_ms in Python),
/// persisted across restarts and pushed live to session + TV.
async fn api_pb_sync(AxState(app): AxState<Arc<App>>, headers: HeaderMap,
                     Json(body): Json<SyncBody>)
    -> impl IntoResponse {
    let (sid, _) = ensure_sid(&app, &headers);
    if !is_host(&app, &headers, &sid) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "host key required"})),
        )
            .into_response();
    }
    let Some(item) = app.st.now_playing() else {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error": "nothing playing"})),
        )
            .into_response();
    };
    let has_chart = {
        let entries = app.st.entries.read().unwrap();
        entries
            .iter()
            .find(|e| e.id == item.entry_id)
            .map(|e| e.chart.is_some())
            .unwrap_or(false)
    };
    if !has_chart {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "no verified chart on this song"})),
        )
            .into_response();
    }
    let delta = body.delta_ms.unwrap_or(0) as f64;
    let new_off = app.st.sync_offset(item.entry_id) + delta;
    app.st.set_sync_offset(item.entry_id, new_off);
    if let Some(sing) = app.sing.lock().unwrap().as_mut() {
        sing.session.set_sync_offset(new_off);
    }
    // native-TV session rides the same absolute offset
    if app.native_tv {
        *app.sync_override.lock().unwrap() = Some(new_off);
    }
    if let Some(lyr) = lyrics_payload_for(&app, &item, item.entry_id) {
        let _ = app.io.emit("lyrics", &lyr).await;
    }
    Json(json!({"ok": true, "sync_offset_ms": new_off as i64}))
        .into_response()
}

/// POST /api/playback/rom — host flips romaji/native lyrics mid-song.
/// Updates the queue item's stored flag, pushes a fresh lyrics payload
/// to every client, and mails the native renderer a rebuild request.
async fn api_pb_rom(AxState(app): AxState<Arc<App>>,
                    headers: HeaderMap,
                    body: axum::body::Bytes)
    -> impl IntoResponse {
    let (sid, _) = ensure_sid(&app, &headers);
    if !is_host(&app, &headers, &sid) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "host key required"})),
        )
            .into_response();
    }
    let Some(item) = app.st.now_playing() else {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error": "nothing playing"})),
        )
            .into_response();
    };
    let rom = serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .and_then(|b| b.get("rom").and_then(|v| v.as_bool()))
        .unwrap_or(!item.lyrics_rom.map(|v| v != 0).unwrap_or(false));
    app.st.set_item_rom(item.id, rom);
    if let Some(sing) = app.sing.lock().unwrap().as_mut() {
        sing.lyrics_rom = rom;
    }
    *app.rom_override.lock().unwrap() = Some(rom);
    if let Some(fresh) = app.st.now_playing() {
        if let Some(lyr) = lyrics_payload_for(&app, &fresh, fresh.entry_id) {
            let _ = app.io.emit("lyrics", &lyr).await;
        }
    }
    app.st.persist();
    Json(json!({"ok": true, "rom": rom})).into_response()
}

/// POST /api/playback/resync — drop prior sync state and collect ~10s
/// of fresh room audio before re-solving (host or current singer).
async fn api_pb_resync(AxState(app): AxState<Arc<App>>,
                       headers: HeaderMap)
    -> impl IntoResponse {
    let (sid, _) = ensure_sid(&app, &headers);
    if !is_host(&app, &headers, &sid) && !is_singer(&app, &sid) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "host or current singer required"})),
        )
            .into_response();
    }
    {
        let mut slot = app.sing.lock().unwrap();
        if let Some(sing) = slot.as_mut() {
            sing.session.request_resync(sing.last_ms);
            return Json(json!({"ok": true,
                "collect_sec": ups_core::session::RESYNC_COLLECT_MS / 1000.0}))
                .into_response();
        }
    }
    // native-TV mode owns its session — flag the driver loop
    if app.native_tv && app.st.now_playing().is_some() {
        app.resync_req
            .store(true, std::sync::atomic::Ordering::Relaxed);
        return Json(json!({"ok": true,
                "collect_sec": ups_core::session::RESYNC_COLLECT_MS / 1000.0}))
            .into_response();
    }
    (
        StatusCode::CONFLICT,
        Json(json!({"error": "no scored song playing"})),
    )
        .into_response()
}

/// POST /api/playback/autosync — toggle the pitch-solve autosync.
async fn api_pb_autosync(AxState(app): AxState<Arc<App>>,
                         headers: HeaderMap, Json(body): Json<Value>)
    -> impl IntoResponse {
    let (sid, _) = ensure_sid(&app, &headers);
    if !is_host(&app, &headers, &sid) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "host key required"})),
        )
            .into_response();
    }
    let en = body["enabled"].as_bool().unwrap_or(true);
    app.st.set_autosync_mic(en);
    Json(json!({"autosync_mic": en})).into_response()
}

// -- mic/audio ----------------------------------------------------------------

async fn api_mic(AxState(app): AxState<Arc<App>>, headers: HeaderMap)
    -> impl IntoResponse {
    let (sid, _) = ensure_sid(&app, &headers);
    let capturing = app.sing.lock().unwrap().is_some();
    let mon = app.monitor.as_ref();
    Json(json!({
        "capture": capturing,
        "monitor": mon.is_some(),
        "device": if capturing { "default" } else { "" },
        "level": 0,
        "mics": [],
        "error": "",
        "volume": mon.map(|m| *m.volume.lock().unwrap()).unwrap_or(0.0),
        "echo_ms": mon.map(|m| *m.echo_ms.lock().unwrap()).unwrap_or(0),
        "echo_wet": 0, "pitch": 0,
        "reverb_wet": 0, "autotune": 0, "polish": false,
        "my_fx": null,
        "muted": mon.map(|m| *m.muted.lock().unwrap()).unwrap_or(true),
        "passthrough": mon.is_some(),
        "mic_delay_ms": *app.mic_delay_ms.lock().unwrap(),
        "music_volume": *app.music_volume.lock().unwrap(),
        "can_mix": is_host(&app, &headers, &sid) || is_singer(&app, &sid),
    }))
}

/// POST /api/mic — live monitor controls (host or current singer).
/// Volume/mute ride on the loopback's playback node via wpctl; voice
/// FX fields are accepted for compatibility but inert for now.
async fn api_mic_set(AxState(app): AxState<Arc<App>>, headers: HeaderMap,
                     Json(body): Json<Value>)
    -> impl IntoResponse {
    let (sid, _) = ensure_sid(&app, &headers);
    if !is_host(&app, &headers, &sid) && !is_singer(&app, &sid) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "host or current singer required"})),
        )
            .into_response();
    }
    let Some(mon) = app.monitor.as_ref() else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "no mic monitor running"})),
        )
            .into_response();
    };
    mon.set(
        body["volume"].as_f64(),
        body["muted"].as_bool(),
        body["echo_ms"].as_i64(),
    );
    Json(json!({
        "ok": true,
        "volume": *mon.volume.lock().unwrap(),
        "echo_ms": *mon.echo_ms.lock().unwrap(),
        "echo_wet": 0, "pitch": 0, "reverb_wet": 0,
        "autotune": 0, "polish": false,
        "my_fx": null,
        "muted": *mon.muted.lock().unwrap(),
    }))
    .into_response()
}

/// POST /api/mic/calibrate — Python listens to the room to measure mic
/// latency; until that lands, return the stored delay so the UI works.
async fn api_mic_calibrate(AxState(app): AxState<Arc<App>>)
    -> impl IntoResponse {
    Json(json!({"ok": true,
                "mic_delay_ms": *app.mic_delay_ms.lock().unwrap()}))
}

/// POST /api/mic/assign — single-mic setups have nothing to route yet;
/// accept the call so the phone UI doesn't error.
async fn api_mic_assign() -> impl IntoResponse {
    Json(json!({"ok": true}))
}

async fn api_audio(AxState(app): AxState<Arc<App>>) -> impl IntoResponse {
    let monitor = app.monitor.as_ref().map(|m| {
        json!({
            "running": true,
            "volume": *m.volume.lock().unwrap(),
            "echo_wet": 0,
            "muted": *m.muted.lock().unwrap(),
        })
    });
    let outputs = if crate::audio::available() {
        crate::audio::playback_targets()
    } else {
        Vec::new()
    };
    Json(json!({
        "outputs": outputs, "mics": [],
        "performers": app.st.performers.lock().unwrap().iter()
            .map(|p| json!({"id": p.id, "name": p.name, "color": p.color}))
            .collect::<Vec<_>>(),
        "monitor": monitor,
    }))
}

/// POST /api/audio/output — route all audio (music + mic monitor) to a
/// sink or enable a card. Body: {sink} | {sink_name} | {card}.
async fn api_audio_output(AxState(app): AxState<Arc<App>>,
                          headers: HeaderMap, Json(body): Json<Value>)
    -> impl IntoResponse {
    let (sid, _) = ensure_sid(&app, &headers);
    if !is_host(&app, &headers, &sid) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "host key required"})),
        )
            .into_response();
    }
    let res = if let Some(v) = body.get("sink") {
        crate::audio::select_output("sink", v)
    } else if let Some(v) = body.get("sink_name") {
        crate::audio::select_output("sink_name", v)
    } else if let Some(v) = body.get("card") {
        crate::audio::select_output("card", v)
    } else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "sink or card required"})),
        )
            .into_response();
    };
    if res["ok"].as_bool().unwrap_or(false) {
        // persist the stable node name — wpctl ids churn across restarts
        let saved = if res["node"].as_str().map(|s| !s.is_empty())
            .unwrap_or(false)
        {
            json!({"sink_name": res["node"]})
        } else if body.get("card").is_some() {
            json!({"card": body["card"]})
        } else {
            Value::Null
        };
        if !saved.is_null() {
            *app.st.output_target.lock().unwrap() =
                Some(saved.to_string());
            app.st.persist();
        }
    }
    if res["ok"].as_bool().unwrap_or(false) {
        Json(res).into_response()
    } else {
        (StatusCode::BAD_GATEWAY, Json(res)).into_response()
    }
}

// -- song/version management (host) ------------------------------------------

/// GET /api/manage/songs — every song + its media versions and
/// queue/chart state, for the phone Songs tab.
async fn api_manage_songs(AxState(app): AxState<Arc<App>>)
    -> impl IntoResponse {
    let entries = app.st.entries.read().unwrap().clone();
    let q = app.st.queue.lock().unwrap();
    let rows: Vec<Value> = entries
        .iter()
        .map(|e| {
            let dir = e.media.parent().map(|p| p.to_path_buf())
                .unwrap_or_else(|| app.st.root());
            let stem = e.media.file_stem()
                .unwrap_or_default().to_string_lossy().to_string();
            let size = std::fs::metadata(&e.media)
                .map(|m| m.len()).unwrap_or(0);
            let has_stem =
                dir.join(format!("{stem}.vocals.wav")).is_file();
            let queued = q.iter()
                .filter(|i| i.entry_id == e.id && i.status == "queued")
                .count();
            let playing = q.iter()
                .filter(|i| i.entry_id == e.id && i.status == "playing")
                .count();
            let chart_source = e.chart.as_ref().and_then(|p| {
                let c = ups_core::chart::load_chart(p);
                c.headers.get("CREATOR").map(|v| {
                    if v.contains("genchart") {
                        "generated".to_string()
                    } else {
                        v.clone()
                    }
                })
            });
            json!({
                "song_id": e.song_id, "artist": e.artist,
                "title": e.title, "is_duet": e.is_duet,
                "entry_id": e.id, "provider": e.provider,
                "source_id": e.source_id, "status": "available",
                "duration_sec": e.duration_sec,
                "local_path": e.media.file_name()
                    .unwrap_or_default().to_string_lossy(),
                "thumbnail": if e.art.is_some() {
                    json!(format!("/api/art/{}.jpg", e.id))
                } else { Value::Null },
                "inst_path": e.inst.as_ref().map(|_| "x"),
                "scored": e.chart.is_some() as i64,
                "chart_source": chart_source,
                "possible_chart": false,
                "playing": playing, "queued": queued,
                "size_bytes": size, "has_stem": has_stem,
                "has_inst": e.inst.is_some(), "processing": false,
            })
        })
        .collect();
    Json(json!({"songs": rows}))
}

/// The disk files backing an entry — media + derived sidecars.
fn entry_files(e: &crate::state::Entry) -> Vec<PathBuf> {
    let mut out = vec![e.media.clone()];
    if let Some(p) = &e.inst {
        out.push(p.clone());
    }
    if let Some(p) = &e.chart {
        out.push(p.clone());
    }
    if let Some(p) = &e.art {
        out.push(p.clone());
    }
    let stem = e.media.file_stem().unwrap_or_default().to_string_lossy();
    let dir = e.media.parent().map(|p| p.to_path_buf())
        .unwrap_or_default();
    for suffix in [".vocals.wav", ".analysis.json", ".lrc", ".txt"] {
        out.push(dir.join(format!("{stem}{suffix}")));
    }
    out
}

/// Remove an entry's queued items, catalog row, and files. Caller has
/// already guarded the playing case.
fn delete_entry(app: &App, entry_id: i64) {
    let (dir, song_id) = {
        let mut entries = app.st.entries.write().unwrap();
        let Some(e) = entries.iter().find(|e| e.id == entry_id).cloned()
        else {
            return;
        };
        let dir = e.media.parent().map(|p| p.to_path_buf());
        for f in entry_files(&e) {
            let _ = std::fs::remove_file(f);
        }
        let sid = e.song_id;
        entries.retain(|e| e.id != entry_id);
        (dir, sid)
    };
    {
        let mut q = app.st.queue.lock().unwrap();
        q.retain(|i| i.entry_id != entry_id || i.status != "queued");
    }
    // one-song-per-folder layouts leave an empty dir behind
    if let Some(d) = dir {
        let _ = std::fs::remove_dir(&d);
    }
    let _ = song_id;
    app.st.persist();
}

/// POST /api/manage/entry/<id>/delete — remove one media version.
async fn api_manage_entry_delete(
    AxState(app): AxState<Arc<App>>, headers: HeaderMap,
    Path(entry_id): Path<i64>)
    -> impl IntoResponse {
    let (sid, _) = ensure_sid(&app, &headers);
    if !is_host(&app, &headers, &sid) {
        return (StatusCode::FORBIDDEN,
                Json(json!({"error": "host key required"})))
            .into_response();
    }
    {
        let entries = app.st.entries.read().unwrap();
        if !entries.iter().any(|e| e.id == entry_id) {
            return (StatusCode::NOT_FOUND,
                    Json(json!({"error": "not found"})))
                .into_response();
        }
    }
    if app.st.now_playing().map(|i| i.entry_id) == Some(entry_id) {
        return (StatusCode::CONFLICT,
                Json(json!({"error": "can't delete — this song is playing"})))
            .into_response();
    }
    delete_entry(&app, entry_id);
    let _ = app.io.emit("state", &app.st.snapshot()).await;
    Json(json!({"ok": true})).into_response()
}

/// POST /api/manage/song/<id>/delete — remove all versions of a song.
/// (File-based catalog: song_id == entry_id here.)
async fn api_manage_song_delete(
    AxState(app): AxState<Arc<App>>, headers: HeaderMap,
    Path(song_id): Path<i64>)
    -> impl IntoResponse {
    api_manage_entry_delete(AxState(app), headers, Path(song_id))
        .await
        .into_response()
}

/// POST /api/manage/entry/<id>/repair — re-derive this version's
/// sidecars. With an `ups-prep` binary alongside us (or on PATH) the
/// full pipeline runs on the media file in the background; otherwise
/// a targeted rescan refreshes whatever files appeared by hand.
async fn api_manage_entry_repair(
    AxState(app): AxState<Arc<App>>, headers: HeaderMap,
    Path(entry_id): Path<i64>)
    -> impl IntoResponse {
    let (sid, _) = ensure_sid(&app, &headers);
    if !is_host(&app, &headers, &sid) {
        return (StatusCode::FORBIDDEN,
                Json(json!({"error": "host key required"})))
            .into_response();
    }
    let media = {
        let entries = app.st.entries.read().unwrap();
        entries.iter().find(|e| e.id == entry_id).map(|e| e.media.clone())
    };
    let Some(media) = media else {
        return (StatusCode::NOT_FOUND,
                Json(json!({"error": "not found"})))
            .into_response();
    };
    let prep = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| {
            d.join(if cfg!(windows) { "ups-prep.exe" } else { "ups-prep" })
        }))
        .filter(|p| p.is_file())
        .or_else(|| {
            std::process::Command::new(
                if cfg!(windows) { "ups-prep.exe" } else { "ups-prep" })
                .arg("--version")
                .output()
                .ok()
                .map(|_| PathBuf::from("ups-prep"))
        });
    if let Some(bin) = prep {
        let root = app.st.root();
        std::thread::spawn(move || {
            let _ = std::process::Command::new(bin)
                .arg(&media)
                .arg("--library")
                .arg(&root)
                .status();
        });
        Json(json!({"ok": true, "regenerating": true})).into_response()
    } else {
        let _ = app.st.rescan();
        let _ = app.io.emit("state", &app.st.snapshot()).await;
        Json(json!({"ok": true, "regenerating": false})).into_response()
    }
}

// -- chart library (host) -----------------------------------------------------

/// Normalize for chart<->song matching (Python's normalized
/// artist+title key): lowercase alphanumeric.
fn norm_key(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// .txt chart files under chart-library/ + charts/ — the unverified
/// drops Python's chart_library table tracks. id is a stable hash of
/// the path so /verify can find it again.
fn pending_charts(app: &App) -> Vec<Value> {
    fn walk(dir: &PathBuf, out: &mut Vec<PathBuf>, depth: u32) {
        if depth > 4 || !dir.is_dir() {
            return;
        }
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out, depth + 1);
                } else if p.extension()
                    .map(|x| x == "txt")
                    .unwrap_or(false)
                {
                    out.push(p);
                }
            }
        }
    }
    let mut files = Vec::new();
    for d in ["chart-library", "charts"] {
        walk(&app.st.root().join(d), &mut files, 0);
    }
    let entries = app.st.entries.read().unwrap().clone();
    files
        .into_iter()
        .map(|p| {
            let c = ups_core::chart::load_chart(&p);
            let (artist, title) = if !c.artist().is_empty() {
                (c.artist().to_string(), c.title().to_string())
            } else {
                let stem = p.file_stem()
                    .unwrap_or_default().to_string_lossy().to_string();
                crate::state::split_artist_title(&stem)
            };
            let m = entries.iter().find(|e| {
                norm_key(&e.artist) == norm_key(&artist)
                    && norm_key(&e.title) == norm_key(&title)
            });
            let id = {
                use std::collections::hash_map::DefaultHasher;
                use std::hash::{Hash, Hasher};
                let mut h = DefaultHasher::new();
                p.hash(&mut h);
                h.finish() as i64
            };
            json!({
                "id": id,
                "artist": artist, "title": title,
                "file_path": p.display().to_string(),
                "source_repo": c.headers.get("SOURCE")
                    .cloned().unwrap_or_else(|| "local".to_string()),
                "is_duet": c.is_duet(),
                "media_entry_id": m.map(|e| e.id),
            })
        })
        .collect()
}

async fn api_pending_charts(AxState(app): AxState<Arc<App>>,
                            headers: HeaderMap)
    -> impl IntoResponse {
    let (sid, _) = ensure_sid(&app, &headers);
    if !is_host(&app, &headers, &sid) {
        return (StatusCode::FORBIDDEN,
                Json(json!({"error": "host key required"})))
            .into_response();
    }
    Json(json!({"charts": pending_charts(&app)})).into_response()
}

/// POST /api/catalog/chart/<id>/verify — promote a chart-library .txt
/// to the entry's canonical .upsong sidecar (import + native save).
async fn api_chart_verify(AxState(app): AxState<Arc<App>>,
                          headers: HeaderMap, Path(chart_id): Path<i64>,
                          Json(body): Json<Value>)
    -> impl IntoResponse {
    let (sid, _) = ensure_sid(&app, &headers);
    if !is_host(&app, &headers, &sid) {
        return (StatusCode::FORBIDDEN,
                Json(json!({"error": "host key required"})))
            .into_response();
    }
    let pending = pending_charts(&app);
    let Some(row) = pending.iter()
        .find(|c| c["id"].as_i64() == Some(chart_id)).cloned()
    else {
        return (StatusCode::NOT_FOUND,
                Json(json!({"error": "chart not found"})))
            .into_response();
    };
    let entry_id = body["media_entry_id"].as_i64()
        .or_else(|| row["media_entry_id"].as_i64());
    let Some(entry_id) = entry_id else {
        return (StatusCode::BAD_REQUEST,
                Json(json!({"error": "no media entry for this song"})))
            .into_response();
    };
    let media = {
        let entries = app.st.entries.read().unwrap();
        entries.iter().find(|e| e.id == entry_id).map(|e| e.media.clone())
    };
    let Some(media) = media else {
        return (StatusCode::NOT_FOUND,
                Json(json!({"error": "media entry not found"})))
            .into_response();
    };
    let c = ups_core::chart::load_chart(
        &PathBuf::from(row["file_path"].as_str().unwrap_or_default()));
    let dest = media.with_extension("upsong");
    if ups_core::chart::save_native(&c, &dest).is_err() {
        return (StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "could not write .upsong"})))
            .into_response();
    }
    let _ = app.st.rescan();
    let _ = app.io.emit("state", &app.st.snapshot()).await;
    Json(json!({"ok": true})).into_response()
}

// -- upload + playlist --------------------------------------------------------

const UPLOAD_EXTS: &[&str] =
    &["mp3", "m4a", "ogg", "wav", "flac", "mp4", "webm", "mkv"];
const UPLOAD_MAX_BYTES: usize = 400 * 1024 * 1024;

fn sanitize_stem(s: &str) -> String {
    let out: String = s
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || " -_()[]&".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let out = out.trim_matches(|c| c == '_' || c == ' ');
    if out.is_empty() { "song".to_string() } else { out.to_string() }
}

/// POST /api/upload — multipart {file, artist?, title?}. Saves under
/// the media root as upload-<ts>-<stem>.<ext>, then the normal rescan
/// picks it up. Returns the new entry's search-row shape for the
/// phone's queue-confirm sheet.
async fn api_upload(AxState(app): AxState<Arc<App>>,
                    mut mp: axum::extract::Multipart)
    -> impl IntoResponse {
    let mut file: Option<(String, Vec<u8>)> = None;
    let (mut artist, mut title) = (String::new(), String::new());
    while let Ok(Some(field)) = mp.next_field().await {
        let name = field.name().unwrap_or("").to_string();
        if name == "file" {
            let fname = field.file_name().unwrap_or("").to_string();
            match field.bytes().await {
                Ok(b) => file = Some((fname, b.to_vec())),
                Err(_) => {
                    return (StatusCode::PAYLOAD_TOO_LARGE,
                            Json(json!({"error": "file too large (400 MB max)"})))
                        .into_response();
                }
            }
        } else if name == "artist" {
            artist = field.text().await.unwrap_or_default();
        } else if name == "title" {
            title = field.text().await.unwrap_or_default();
        }
    }
    let Some((fname, bytes)) = file else {
        return (StatusCode::BAD_REQUEST,
                Json(json!({"error": "pick a file to upload"})))
            .into_response();
    };
    let ext = std::path::Path::new(&fname)
        .extension()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase();
    if !UPLOAD_EXTS.contains(&ext.as_str()) {
        return (StatusCode::BAD_REQUEST,
                Json(json!({"error": format!(
                    "unsupported file type '{ext}' — \
                     mp3/m4a/ogg/wav/flac/mp4/webm/mkv")})))
            .into_response();
    }
    if bytes.len() > UPLOAD_MAX_BYTES {
        return (StatusCode::PAYLOAD_TOO_LARGE,
                Json(json!({"error": "file too large (400 MB max)"})))
            .into_response();
    }
    // "Artist - Title.ext" filename convention fills missing fields
    let raw_stem = std::path::Path::new(&fname)
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    if let Some((a, t)) = raw_stem.split_once(" - ") {
        if artist.trim().is_empty() {
            artist = a.trim().to_string();
        }
        if title.trim().is_empty() {
            title = t.trim().to_string();
        }
    }
    let artist = if artist.trim().is_empty() {
        "Unknown".to_string()
    } else {
        artist.trim().to_string()
    };
    let title = if title.trim().is_empty() {
        raw_stem.clone()
    } else {
        title.trim().to_string()
    };
    let stem = sanitize_stem(&raw_stem);
    let name = format!(
        "upload-{}-{}{}",
        crate::state::now_secs() as u64,
        &stem[..stem.len().min(60)],
        if ext.is_empty() { String::new() } else { format!(".{ext}") },
    );
    // one "Artist - Title" folder per upload — the catalog's folder
    // convention gives rescan the right display names
    let dir = app.st.root().join(format!(
        "{} - {}",
        sanitize_stem(&artist),
        sanitize_stem(&title)
    ));
    let _ = std::fs::create_dir_all(&dir);
    let dest = dir.join(&name);
    if std::fs::write(&dest, &bytes).is_err() {
        return (StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "could not save file"})))
            .into_response();
    }
    let _ = app.st.rescan();
    let entry_row = {
        let entries = app.st.entries.read().unwrap();
        entries.iter().find(|e| e.media == dest).map(|e| e.row(None))
    };
    let _ = app.io.emit("state", &app.st.snapshot()).await;
    Json(json!({"ok": true, "entry": entry_row})).into_response()
}

/// POST /api/playlist — expand a remote playlist via yt-dlp
/// (flat-playlist) and queue every song already in the library.
/// The Rust app has no download provider, so entries without a local
/// match count as skipped — prep them on a station to make them
/// playable. Progress rides playlist:progress/playlist:done like
/// Python's job.
async fn api_playlist(AxState(app): AxState<Arc<App>>,
                      headers: HeaderMap, Json(body): Json<Value>)
    -> impl IntoResponse {
    let (sid, _) = ensure_sid(&app, &headers);
    let url = body["url"].as_str().unwrap_or("").trim().to_string();
    if url.is_empty() {
        return (StatusCode::BAD_REQUEST,
                Json(json!({"error": "url required"})))
            .into_response();
    }
    let name = body["name"].as_str().unwrap_or("").trim().to_string();
    let perf_id = if !name.is_empty() {
        let p = app.st.add_performer(&name[..name.len().min(24)]);
        app.st.sessions.lock().unwrap()
            .get_mut(&sid)
            .map(|s| s.performer_id = Some(p.id));
        p.id
    } else {
        let Some(pid) = sess(&app, &sid).performer_id else {
            return (StatusCode::BAD_REQUEST,
                    Json(json!({"error": "join with a display name first"})))
                .into_response();
        };
        pid
    };
    let out = match tokio::process::Command::new("yt-dlp")
        .args(["--flat-playlist", "-J", &url])
        .output()
        .await
    {
        Ok(o) if o.status.success() => o,
        _ => {
            return (StatusCode::BAD_GATEWAY,
                    Json(json!({"error":
                        "yt-dlp required for playlist import"})))
                .into_response();
        }
    };
    let doc: Value =
        match serde_json::from_slice(&out.stdout) {
            Ok(v) => v,
            Err(_) => {
                return (StatusCode::BAD_GATEWAY,
                        Json(json!({"error": "could not read playlist"})))
                    .into_response();
            }
        };
    let pl_title =
        doc["title"].as_str().unwrap_or("playlist").to_string();
    let entries: Vec<Value> =
        doc["entries"].as_array().cloned().unwrap_or_default();
    let total = entries.len();
    let job = format!("{:x}", crate::state::now_secs() as u64 & 0xffff_ffff);
    let app2 = app.clone();
    let job2 = job.clone();
    let title2 = pl_title.clone();
    tokio::spawn(async move {
        let mut done = 0usize;
        let (mut queued, mut skipped) = (0, 0);
        for e in &entries {
            let title = e["title"].as_str().unwrap_or("");
            // match on normalized title (or "artist - title" form)
            let want = norm_key(
                title
                    .rsplit_once(" - ")
                    .map(|(_, t)| t)
                    .unwrap_or(title),
            );
            let found = {
                let ents = app2.st.entries.read().unwrap();
                ents.iter()
                    .find(|en| {
                        norm_key(&en.title) == want
                            || norm_key(&format!("{} - {}", en.artist, en.title))
                                == norm_key(title)
                    })
                    .map(|en| en.id)
            };
            if let Some(entry_id) = found {
                app2.st.queue_add(
                    entry_id, perf_id, None,
                    String::new(), None, None,
                );
                queued += 1;
            } else {
                skipped += 1;
            }
            done += 1;
            let _ = app2.io.emit("playlist:progress", &json!({
                "job": job2, "done": done, "total": total,
                "queued": queued, "skipped": skipped,
            })).await;
        }
        // queue_add can't fail per-entry locally — Python's failures
        // were download errors; ours land under `skipped`
        let failed = 0;
        // idle queue auto-starts like a manual queue add
        if app2.st.now_playing().is_none() {
            app2.st.start_next();
            Box::pin(sock::push_now_playing(&app2)).await;
        }
        let _ = app2.io.emit("state", &app2.st.snapshot()).await;
        let _ = app2.io.emit("playlist:done", &json!({
            "job": job2, "title": title2, "queued": queued,
            "skipped": skipped, "failed": failed,
        })).await;
    });
    Json(json!({"ok": true, "job": job, "title": pl_title,
                "total": total}))
    .into_response()
}

// -- router ------------------------------------------------------------------

pub fn router(app: Arc<App>, sio_layer: socketioxide::layer::SocketIoLayer) -> Router {
    Router::new()
        // pages
        .route("/", get(page_phone))
        .route("/tv", get(page_tv))
        .route("/host", get(page_host))
        .route("/static/{*path}", get(static_file))
        // media + art
        .route("/media/e{id}", get(media_file))
        .route("/api/art/{filename}", get(art_file))
        // identity
        .route("/api/me", get(api_me))
        .route("/api/join", post(api_join))
        .route("/api/host/login", post(api_host_login))
        .route("/api/host/release", post(api_host_release))
        .route("/api/session/end", post(api_session_end))
        // catalog
        .route("/api/search", get(api_search))
        .route("/api/library", get(api_library))
        .route("/api/scores", get(api_scores))
        .route("/api/catalog/rescan", post(api_catalog_rescan))
        // queue
        .route("/api/queue", get(api_queue_list).post(api_queue_add))
        .route("/api/queue/{id}/remove", post(api_queue_remove))
        .route("/api/queue/{id}/join-duet", post(api_queue_join_duet))
        .route("/api/queue/{id}/move", post(api_queue_move))
        .route("/api/queue/clear", post(api_queue_clear))
        // playback
        .route("/api/playback/next", post(api_pb_next))
        .route("/api/playback/pause", post(api_pb_pause))
        .route("/api/playback/resume", post(api_pb_resume))
        .route("/api/playback/volume", post(api_pb_volume))
        .route("/api/playback/sync",
               get(api_pb_sync_get).post(api_pb_sync))
        .route("/api/playback/resync", post(api_pb_resync))
        .route("/api/playback/rom", post(api_pb_rom))
        .route("/api/playback/autosync", post(api_pb_autosync))
        // mic/audio
        .route("/api/mic", get(api_mic).post(api_mic_set))
        .route("/api/mic/status", get(api_mic))
        .route("/api/mic/calibrate", post(api_mic_calibrate))
        .route("/api/mic/assign", post(api_mic_assign))
        .route("/api/audio", get(api_audio))
        .route("/api/audio/status", get(api_audio))
        .route("/api/audio/output", post(api_audio_output))
        // library management (host) + uploads + playlists
        .route("/api/manage/songs", get(api_manage_songs))
        .route("/api/manage/entry/{id}/delete",
               post(api_manage_entry_delete))
        .route("/api/manage/entry/{id}/repair",
               post(api_manage_entry_repair))
        .route("/api/manage/song/{id}/delete",
               post(api_manage_song_delete))
        .route("/api/catalog/pending-charts", get(api_pending_charts))
        .route("/api/catalog/chart/{id}/verify", post(api_chart_verify))
        .route("/api/upload",
               post(api_upload).layer(
                   axum::extract::DefaultBodyLimit::max(
                       UPLOAD_MAX_BYTES)))
        .route("/api/playlist", post(api_playlist))
        .layer(sio_layer)
        .with_state(app)
}
