//! axum HTTP layer — port of web/pages.py + web/api.py. Serves the
//! vendored frontend verbatim; REST shapes match what phone.js/host.js
//! consume. Pipeline-dependent endpoints (uploads, remote providers,
//! repair) return compatible stubs until the pipeline lands.

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
        if app.st.sessions.lock().unwrap().contains_key(&sid) {
            return (sid, false);
        }
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
    let Some(entry) = app.st.entries.iter().find(|e| e.id == id) else {
        return (StatusCode::NOT_FOUND, "no entry").into_response();
    };
    let path: PathBuf = if q.get("v").map(|v| v.as_str()) == Some("inst") {
        entry.inst.clone().unwrap_or_else(|| entry.media.clone())
    } else {
        entry.media.clone()
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
    let Some(entry) = app.st.entries.iter().find(|e| e.id == id) else {
        return (StatusCode::NOT_FOUND, "no art").into_response();
    };
    let Some(path) = &entry.art else {
        return (StatusCode::NOT_FOUND, "no art").into_response();
    };
    match ServeFile::new(path).oneshot(req).await {
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
        "results": app.st.entries.iter()
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
            let e = app.st.entries.iter().find(|e| e.song_id == h.song_id);
            let p1 = app.st.performer(h.performer_id);
            let p2 = h.performer2_id.and_then(|id| app.st.performer(id));
            let name = match (p1, p2) {
                (Some(a), Some(b)) => format!("{} & {}", a.name, b.name),
                (Some(a), None) => a.name,
                _ => "?".into(),
            };
            json!({
                "song_id": h.song_id,
                "artist": e.map(|e| e.artist.clone()).unwrap_or_default(),
                "title": e.map(|e| e.title.clone()).unwrap_or_default(),
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
    let Some(entry) = app.st.entries.iter().find(|e| {
        e.id == entry_id || e.song_id == entry_id
    }) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "unknown catalog entry"})),
        )
            .into_response();
    };
    let perf2_id = if entry.is_duet {
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
        entry.id,
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
        let entry = app.st.entries.iter().find(|e| e.id == it.entry_id);
        let open = entry.map(|e| e.is_duet).unwrap_or(false)
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
    let delta = body.delta_ms.unwrap_or(0) as f64;
    let mut slot = app.sing.lock().unwrap();
    let Some(sing) = slot.as_mut() else {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error": "nothing playing"})),
        )
            .into_response();
    };
    sing.session.set_sync_offset(delta);
    Json(json!({"ok": true, "sync_offset_ms": delta as i64})).into_response()
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
    Json(json!({
        "outputs": [], "mics": [],
        "performers": app.st.performers.lock().unwrap().iter()
            .map(|p| json!({"id": p.id, "name": p.name, "color": p.color}))
            .collect::<Vec<_>>(),
        "monitor": monitor,
    }))
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
        .route("/api/playback/sync", post(api_pb_sync))
        // mic/audio
        .route("/api/mic", get(api_mic).post(api_mic_set))
        .route("/api/mic/status", get(api_mic))
        .route("/api/mic/calibrate", post(api_mic_calibrate))
        .route("/api/mic/assign", post(api_mic_assign))
        .route("/api/audio", get(api_audio))
        .route("/api/audio/status", get(api_audio))
        .layer(sio_layer)
        .with_state(app)
}
