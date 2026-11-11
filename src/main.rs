//! Web radio player: streams internet radio stations through the router,
//! with a station manager and an optional proxy per station.
//!
//! Usage: internet-radio-proxifier -h ADDRESS [-c SETTINGS_FILE]
//! The settings file holds the proxy, the station list and the settings
//! page password.

mod http;
mod stations;

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio::time::{Instant, MissedTickBehavior, interval, timeout, timeout_at};

use http::Proxy;
use stations::Station;

const MASK: &str = "********";
const TITLE_INTERVAL: Duration = Duration::from_secs(5);

struct Session {
    station: usize,
    tx: mpsc::UnboundedSender<String>,
}

struct App {
    stations: RwLock<Arc<Vec<Station>>>,
    /// Raw proxy URL as saved, and its parsed form.
    proxy: RwLock<(String, Option<Proxy>)>,
    /// Start playing as soon as the player page opens.
    autoplay: std::sync::atomic::AtomicBool,
    /// With autoplay: start with a random station.
    random_start: std::sync::atomic::AtomicBool,
    /// Next / Previous pick a random station.
    random: std::sync::atomic::AtomicBool,
    /// Reconnects to a station that stopped, before the player skips it.
    retries: AtomicUsize,
    stations_file: String,
    sessions: Mutex<HashMap<u64, Session>>,
    /// Last known song title per stream URL.
    titles: Mutex<HashMap<String, String>>,
    /// Stream URLs with a title fetch running.
    inflight: Mutex<HashSet<String>>,
    save_lock: Mutex<()>,
    // usize: 32-bit MIPS has no 64-bit atomics.
    next_id: AtomicUsize,
}

type Shared = Arc<App>;

fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&#34;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

impl App {
    fn stations(&self) -> Arc<Vec<Station>> {
        self.stations.read().unwrap().clone()
    }

    fn proxy_raw(&self) -> String {
        self.proxy.read().unwrap().0.clone()
    }

    fn proxy_for(&self, st: &Station) -> Option<Proxy> {
        if st.use_proxy { self.proxy.read().unwrap().1.clone() } else { None }
    }

    /// The URL the player uses for station i.
    fn station_url(i: usize) -> String {
        format!("/station/{i}")
    }

    fn cached_title(&self, url: &str) -> String {
        self.titles.lock().unwrap().get(url).cloned().unwrap_or_default()
    }

    fn player_html(i: usize) -> String {
        format!(
            r#"<source id="audio-source" src="{}" data-index="{i}" type="audio/mpeg">"#,
            esc(&Self::station_url(i))
        )
    }

    fn info_html(st: &Station, title: &str) -> String {
        format!(
            "<div id=\"station-name\"><a class=\"stationurl\" href=\"{}\">[{}]</a></div>\n<div id=\"station-title\">{}</div>",
            esc(&st.url),
            esc(&st.name),
            esc(title)
        )
    }

    fn station_info(&self, list: &[Station], i: usize) -> Option<String> {
        list.get(i).map(|st| App::info_html(st, &self.cached_title(&st.url)))
    }

    fn send(&self, id: u64, html: String) {
        if let Some(s) = self.sessions.lock().unwrap().get(&id) {
            let _ = s.tx.send(html);
        }
    }

    fn broadcast(&self, station: usize, html: &str) {
        for s in self.sessions.lock().unwrap().values() {
            if s.station == station {
                let _ = s.tx.send(html.to_string());
            }
        }
    }

    /// Stations that connected listeners are on.
    fn listened(&self) -> HashSet<usize> {
        self.sessions.lock().unwrap().values().map(|s| s.station).collect()
    }
}

// ---------------------------------------------------------------- titles

async fn stream_title(url: &str, proxy: Option<&Proxy>) -> std::io::Result<String> {
    let mut resp = http::get(url, proxy, &[("Icy-MetaData", "1")]).await?;
    let metaint: usize = resp
        .header("icy-metaint")
        .and_then(|v| v.trim().parse().ok())
        .ok_or_else(|| http::err(format!("no metadata for {url}")))?;
    resp.body.skip(metaint).await?;
    let mut len = [0u8];
    resp.body.read_full(&mut len).await?;
    let mut meta = vec![0u8; len[0] as usize * 16];
    resp.body.read_full(&mut meta).await?;
    for part in meta.split(|&b| b == b';') {
        let p = trim_bytes(part);
        if p.len() >= 14 && p.starts_with(b"StreamTitle='") {
            return Ok(String::from_utf8_lossy(&p[13..p.len() - 1]).into_owned());
        }
    }
    Ok(String::new())
}

fn trim_bytes(mut b: &[u8]) -> &[u8] {
    while let [first, rest @ ..] = b {
        if !matches!(first, b' ' | b'\t' | 0) {
            break;
        }
        b = rest;
    }
    while let [rest @ .., last] = b {
        if !matches!(last, b' ' | b'\t' | 0) {
            break;
        }
        b = rest;
    }
    b
}

/// Fetches station k's title and sends it to everyone listening to it.
async fn refresh_station(app: &App, list: &[Station], k: usize) {
    let st = &list[k];
    let title = match timeout(Duration::from_secs(10), stream_title(&st.url, app.proxy_for(st).as_ref())).await {
        Ok(Ok(t)) => {
            let t = stations::clean_title(&t);
            app.titles.lock().unwrap().insert(st.url.clone(), t.clone());
            t
        }
        Ok(Err(e)) => {
            if !e.to_string().starts_with("no metadata") {
                eprintln!("title of {}: {e}", st.name);
            }
            app.cached_title(&st.url)
        }
        Err(_) => {
            eprintln!("title of {}: timed out", st.name);
            app.cached_title(&st.url)
        }
    };
    app.broadcast(k, &App::info_html(st, &title));
}

/// Starts a title fetch for station k unless one for the same stream runs.
fn refresh_async(app: &Shared, list: &Arc<Vec<Station>>, k: usize) {
    let Some(st) = list.get(k) else { return };
    let url = st.url.clone();
    if !app.inflight.lock().unwrap().insert(url.clone()) {
        return;
    }
    let (app, list) = (app.clone(), list.clone());
    tokio::spawn(async move {
        refresh_station(&app, &list, k).await;
        app.inflight.lock().unwrap().remove(&url);
    });
}

/// Titles are fetched only for stations someone is listening to.
async fn title_loop(app: Shared) {
    let mut tick = interval(TITLE_INTERVAL);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let list = app.stations();
        for k in app.listened() {
            refresh_async(&app, &list, k);
        }
    }
}

// ---------------------------------------------------------------- websocket

#[derive(Deserialize)]
struct WsMsg {
    #[serde(default)]
    action: String,
    #[serde(default)]
    index: i64,
}

/// A random number below n (n > 0), from std's per-process random hash keys
/// and the clock; no crate needed for picking a station.
fn random_below(n: usize) -> usize {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    h.write_u128(nanos);
    (h.finish() % n as u64) as usize
}

fn handle_ws_message(app: &Shared, id: u64, text: &str) {
    let list = app.stations();
    let Ok(msg) = serde_json::from_str::<WsMsg>(text) else {
        eprintln!("Unparsable message: {text}");
        return;
    };
    let station = {
        let mut sessions = app.sessions.lock().unwrap();
        let Some(s) = sessions.get_mut(&id) else { return };
        let cur = if s.station < list.len() { s.station } else { 0 };
        let next = match msg.action.as_str() {
            "next" | "previous" if app.random.load(Ordering::Relaxed) && list.len() > 1 => {
                // any station but the current one
                let r = random_below(list.len() - 1);
                if r >= cur { r + 1 } else { r }
            }
            "next" => (cur + 1) % list.len(),
            "previous" => (cur + list.len() - 1) % list.len(),
            "select" => {
                if msg.index < 0 || msg.index as usize >= list.len() {
                    return;
                }
                msg.index as usize
            }
            other => {
                eprintln!("Unhandled message action {other:?}");
                cur
            }
        };
        s.station = next;
        next
    };
    // Name first: the player shows it as soon as the new source starts.
    if let Some(info) = app.station_info(&list, station) {
        app.send(id, info);
    }
    app.send(id, App::player_html(station));
    refresh_async(app, &list, station);
}

async fn ws_client(app: Shared, mut sock: WebSocket) {
    let id = app.next_id.fetch_add(1, Ordering::Relaxed) as u64;
    let (tx, mut rx) = mpsc::unbounded_channel();
    app.sessions.lock().unwrap().insert(id, Session { station: 0, tx });
    if let Some(info) = app.station_info(&app.stations(), 0) {
        app.send(id, info);
    }
    // Ping regularly so vanished clients are dropped (and their station is
    // no longer polled for titles).
    let mut ping = interval(Duration::from_secs(30));
    let mut last_seen = Instant::now();
    loop {
        tokio::select! {
            m = sock.recv() => match m {
                Some(Ok(Message::Text(t))) => {
                    last_seen = Instant::now();
                    handle_ws_message(&app, id, t.as_str());
                }
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => last_seen = Instant::now(),
            },
            Some(html) = rx.recv() => {
                if sock.send(Message::Text(html.into())).await.is_err() {
                    break;
                }
            }
            _ = ping.tick() => {
                if last_seen.elapsed() > Duration::from_secs(75)
                    || sock.send(Message::Ping(Bytes::new())).await.is_err()
                {
                    break;
                }
            }
        }
    }
    app.sessions.lock().unwrap().remove(&id);
}

async fn ws(State(app): State<Shared>, upgrade: WebSocketUpgrade) -> Response {
    upgrade.on_upgrade(move |sock| ws_client(app, sock))
}

// ---------------------------------------------------------------- pages

const INDEX: &str = include_str!("../assets/templates/index.html");
const SETTINGS: &str = include_str!("../assets/templates/settings.html");

macro_rules! assets {
    ($($name:literal => $ct:literal),* $(,)?) => {
        fn asset(name: &str) -> Option<(&'static [u8], &'static str)> {
            match name {
                $($name => Some((include_bytes!(concat!("../assets/static/", $name)), $ct)),)*
                _ => None,
            }
        }
    };
}

assets! {
    "settings.css" => "text/css; charset=utf-8",
    "settings.js" => "text/javascript; charset=utf-8",
    "htmx.min.js" => "text/javascript; charset=utf-8",
    "htmx-ws.js" => "text/javascript; charset=utf-8",
    "icon-192.png" => "image/png",
    "icon-512.png" => "image/png",
    "manifest.webmanifest" => "application/manifest+json",
    "script.js" => "text/javascript; charset=utf-8",
    "style.css" => "text/css; charset=utf-8",
    "sw.js" => "text/javascript; charset=utf-8",
}

fn serve_asset(name: &str) -> Response {
    match asset(name) {
        Some((data, ct)) => ([(header::CONTENT_TYPE, ct)], data).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn static_file(Path(path): Path<String>) -> Response {
    serve_asset(&path)
}

async fn index(State(app): State<Shared>) -> Html<String> {
    let autoplay = match (app.autoplay.load(Ordering::Relaxed), app.random_start.load(Ordering::Relaxed)) {
        (true, true) => "autoplay data-autoplay data-random-start",
        (true, false) => "autoplay data-autoplay",
        _ => "",
    };
    let attrs = format!("{autoplay} data-retries=\"{}\"", app.retries.load(Ordering::Relaxed));
    Html(INDEX.replace("{{url}}", &esc(&App::station_url(0))).replace("{{autoplay}}", &attrs))
}

#[derive(Serialize)]
struct PickerEntry<'a> {
    name: &'a str,
    proxy: bool,
}

async fn api_stations(State(app): State<Shared>) -> Response {
    let list = app.stations();
    let out: Vec<PickerEntry> = list.iter().map(|s| PickerEntry { name: &s.name, proxy: s.use_proxy }).collect();
    Json(out).into_response()
}

/// The stream of station `index`, relayed through this server.
async fn station_stream(State(app): State<Shared>, Path(index): Path<String>) -> Response {
    let list = app.stations();
    let Some(st) = index.parse::<usize>().ok().and_then(|i| list.get(i)) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let proxy = app.proxy_for(st);
    // Short enough that the player moves on quickly from a dead station.
    let resp = match timeout(Duration::from_secs(10), http::get(&st.url, proxy.as_ref(), &[])).await {
        Ok(Ok(r)) if (200..300).contains(&r.status) => r,
        Ok(Ok(r)) => {
            eprintln!("{}: server answered {}", st.name, r.status_text);
            return StatusCode::BAD_GATEWAY.into_response();
        }
        Ok(Err(e)) => {
            eprintln!("{}: {e}", st.name);
            return StatusCode::BAD_GATEWAY.into_response();
        }
        Err(_) => {
            eprintln!("{}: no answer within 10 s", st.name);
            return StatusCode::BAD_GATEWAY.into_response();
        }
    };
    let ct = match resp.header("content-type") {
        None | Some("") | Some("application/octet-stream") => "audio/mpeg".to_string(),
        Some(c) => c.to_string(),
    };
    let (tx, rx) = mpsc::channel::<std::io::Result<Bytes>>(4);
    let mut body = resp.body;
    tokio::spawn(async move {
        loop {
            let mut buf = vec![0u8; 16 * 1024];
            // Stop as soon as the listener goes away, even if the station stalls.
            let n = tokio::select! {
                _ = tx.closed() => break,
                r = body.read(&mut buf) => r,
            };
            match n {
                Ok(0) => break,
                Ok(n) => {
                    buf.truncate(n);
                    if tx.send(Ok(Bytes::from(buf))).await.is_err() {
                        break;
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(e)).await;
                    break;
                }
            }
        }
    });
    Response::builder()
        .header(header::CONTENT_TYPE, ct)
        .body(Body::from_stream(ChannelStream(rx)))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

struct ChannelStream(mpsc::Receiver<std::io::Result<Bytes>>);

impl futures_core::Stream for ChannelStream {
    type Item = std::io::Result<Bytes>;
    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.0.poll_recv(cx)
    }
}

// ---------------------------------------------------------------- settings

#[derive(Serialize, Deserialize)]
struct StationJson {
    #[serde(default)]
    name: String,
    #[serde(default)]
    url: String,
    #[serde(default, rename = "useProxy")]
    use_proxy: bool,
}

#[derive(Serialize, Deserialize)]
struct ConfigJson {
    #[serde(default)]
    proxy: String,
    autoplay: bool,
    random_start: bool,
    random: bool,
    #[serde(default = "default_retries")]
    retries: u32,
    #[serde(default)]
    stations: Vec<StationJson>,
}

fn default_retries() -> u32 {
    stations::DEFAULT_RETRIES
}

/// Splits "scheme://user:pass@rest" into (prefix incl. "user:", pass, "@rest").
fn split_password(raw: &str) -> Option<(&str, &str, &str)> {
    let start = raw.find("://")? + 3;
    let rest = &raw[start..];
    let auth_end = start + rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let at = start + raw[start..auth_end].rfind('@')?;
    let colon = start + raw[start..at].find(':')?;
    Some((&raw[..colon + 1], &raw[colon + 1..at], &raw[at..]))
}

/// Hides the password so it is never sent back to the browser.
fn mask_proxy(raw: &str) -> String {
    match split_password(raw) {
        Some((pre, _, post)) => format!("{pre}{MASK}{post}"),
        None => raw.to_string(),
    }
}

/// Puts the saved password back if the browser sent the mask.
fn unmask_proxy(raw: &str, saved: &str) -> String {
    let raw = raw.trim();
    match (split_password(raw), split_password(saved)) {
        (Some((pre, MASK, post)), Some((_, pw, _))) => format!("{pre}{pw}{post}"),
        _ => raw.to_string(),
    }
}

fn config_json(app: &App) -> ConfigJson {
    let list = app.stations();
    ConfigJson {
        proxy: mask_proxy(&app.proxy_raw()),
        autoplay: app.autoplay.load(Ordering::Relaxed),
        random_start: app.random_start.load(Ordering::Relaxed),
        random: app.random.load(Ordering::Relaxed),
        retries: app.retries.load(Ordering::Relaxed) as u32,
        stations: list
            .iter()
            .map(|s| StationJson { name: s.name.clone(), url: s.url.clone(), use_proxy: s.use_proxy })
            .collect(),
    }
}

fn json_error(code: StatusCode, msg: impl Into<String>) -> Response {
    (code, Json(serde_json::json!({ "error": msg.into() }))).into_response()
}

fn check_auth(app: &App, headers: &HeaderMap) -> Result<(), Response> {
    // Read from the file every time: it is changed only by editing the file,
    // and that takes effect without a restart.
    let password = match stations::load(&app.stations_file) {
        Ok(s) => s.password,
        Err(e) => {
            eprintln!("Cannot read {}: {e}", app.stations_file);
            return Err(json_error(StatusCode::INTERNAL_SERVER_ERROR, "cannot read the settings file"));
        }
    };
    if password.is_empty() {
        return Ok(());
    }
    let expected = format!("Basic {}", http::base64(format!("admin:{password}").as_bytes()));
    if headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) == Some(expected.as_str()) {
        return Ok(());
    }
    Err((StatusCode::UNAUTHORIZED, [(header::WWW_AUTHENTICATE, "Basic realm=\"Authorization Required\"")])
        .into_response())
}

async fn settings_page(State(app): State<Shared>, headers: HeaderMap) -> Response {
    if let Err(r) = check_auth(&app, &headers) {
        return r;
    }
    Html(SETTINGS).into_response()
}

async fn settings_get(State(app): State<Shared>, headers: HeaderMap) -> Response {
    if let Err(r) = check_auth(&app, &headers) {
        return r;
    }
    Json(config_json(&app)).into_response()
}

async fn settings_put(State(app): State<Shared>, headers: HeaderMap, body: Bytes) -> Response {
    if let Err(r) = check_auth(&app, &headers) {
        return r;
    }
    let Ok(input) = serde_json::from_slice::<ConfigJson>(&body) else {
        return json_error(StatusCode::BAD_REQUEST, "invalid JSON");
    };
    let list: Vec<Station> = input
        .stations
        .into_iter()
        .map(|s| Station { name: s.name, url: s.url, use_proxy: s.use_proxy })
        .collect();
    let list = match stations::validate(list) {
        Ok(l) => l,
        Err(e) => return json_error(StatusCode::BAD_REQUEST, e),
    };
    let _guard = app.save_lock.lock().unwrap();
    let raw = unmask_proxy(&input.proxy, &app.proxy_raw());
    let parsed = if raw.is_empty() {
        None
    } else {
        match Proxy::parse(&raw) {
            Ok(p) => Some(p),
            Err(e) => return json_error(StatusCode::BAD_REQUEST, e),
        }
    };
    // The password is set only by editing the file; keep what is there now.
    let password = match stations::load(&app.stations_file) {
        Ok(s) => s.password,
        Err(e) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, format!("cannot read {}: {e}", app.stations_file)),
    };
    let settings = stations::Settings { stations: list, proxy: raw, password, autoplay: input.autoplay,
        random_start: input.random_start,
        random: input.random,
        retries: input.retries.min(stations::MAX_RETRIES),
    };
    if let Err(e) = stations::save(&app.stations_file, &settings) {
        eprintln!("Saving settings failed: {e}");
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("could not save {}: {e}", app.stations_file),
        );
    }
    let has_proxy = parsed.is_some();
    let stations::Settings { stations: list, proxy: raw, autoplay, random_start, random, retries, .. } = settings;
    app.retries.store(retries as usize, Ordering::Relaxed);
    app.autoplay.store(autoplay, Ordering::Relaxed);
    app.random_start.store(random_start, Ordering::Relaxed);
    app.random.store(random, Ordering::Relaxed);
    *app.proxy.write().unwrap() = (raw, parsed);
    let new = Arc::new(list);
    let old = std::mem::replace(&mut *app.stations.write().unwrap(), new.clone());
    remap_sessions(&app, &old, &new);
    eprintln!("Settings saved ({} stations, proxy {has_proxy})", new.len());
    Json(config_json(&app)).into_response()
}

/// Keeps every listener on the same stream after the list was reordered.
/// Listeners whose station was removed are moved to station 0.
fn remap_sessions(app: &App, old: &[Station], cur: &[Station]) {
    let mut index: HashMap<&str, usize> = HashMap::new();
    for (i, s) in cur.iter().enumerate() {
        index.entry(&s.url).or_insert(i);
    }
    for s in app.sessions.lock().unwrap().values_mut() {
        let idx = s.station;
        let found = old.get(idx).and_then(|st| index.get(st.url.as_str()).copied());
        let new_idx = found.unwrap_or(0);
        s.station = new_idx;
        // The relayed URL contains the index, so the player needs the new one.
        if found.is_none() || new_idx != idx {
            let _ = s.tx.send(App::info_html(&cur[new_idx], &app.cached_title(&cur[new_idx].url)));
            let _ = s.tx.send(App::player_html(new_idx));
        }
    }
}

#[derive(Deserialize)]
struct CheckReq {
    #[serde(default)]
    url: String,
    #[serde(default, rename = "useProxy")]
    use_proxy: bool,
    /// Proxy from the form, possibly not saved yet.
    #[serde(default)]
    proxy: String,
}

#[derive(Serialize, Default)]
struct CheckResult {
    ok: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    status: String,
    #[serde(rename = "contentType", skip_serializing_if = "String::is_empty")]
    content_type: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    bitrate: String,
    #[serde(rename = "icyName", skip_serializing_if = "String::is_empty")]
    icy_name: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    error: String,
}

fn check_error(msg: impl Into<String>) -> CheckResult {
    CheckResult { error: msg.into(), ..Default::default() }
}

/// Connects to a stream and reads a little audio to see if it plays.
async fn check_stream(url: &str, proxy: Option<&Proxy>) -> CheckResult {
    if !http::is_stream_url(url) {
        return check_error("URL must start with http:// or https://");
    }
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut resp = match timeout_at(deadline, http::get(url, proxy, &[])).await {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => return check_error(e.to_string()),
        Err(_) => return check_error("no answer within 15 s"),
    };
    let mut r = CheckResult {
        status: resp.status_text.clone(),
        content_type: resp.header("content-type").unwrap_or("").to_string(),
        bitrate: resp.header("icy-br").unwrap_or("").to_string(),
        icy_name: resp.header("icy-name").unwrap_or("").to_string(),
        ..Default::default()
    };
    if resp.status != 200 {
        r.error = format!("server answered {}", resp.status_text);
        return r;
    }
    // ~4 s of 128 kbps audio: catches streams that send a short burst and stall.
    const WANT: usize = 64 * 1024;
    let start = Instant::now();
    let mut buf = vec![0u8; 16 * 1024];
    let mut n = 0;
    while n < WANT {
        match timeout_at(deadline, resp.body.read(&mut buf)).await {
            Ok(Ok(0)) => {
                r.error = format!("stream stopped after {n} bytes");
                return r;
            }
            Ok(Ok(k)) => n += k,
            Ok(Err(e)) => {
                r.error = format!("stream stopped after {n} bytes: {e}");
                return r;
            }
            Err(_) => {
                r.error = if n > 0 {
                    format!("too slow to play: {:.1} KB/s", n as f64 / 1024.0 / start.elapsed().as_secs_f64())
                } else {
                    format!("stream stopped after {n} bytes")
                };
                return r;
            }
        }
    }
    r.ok = true;
    r
}

async fn settings_check(State(app): State<Shared>, headers: HeaderMap, body: Bytes) -> Response {
    if let Err(r) = check_auth(&app, &headers) {
        return r;
    }
    let Ok(req) = serde_json::from_slice::<CheckReq>(&body) else {
        return json_error(StatusCode::BAD_REQUEST, "invalid JSON");
    };
    let proxy = if req.use_proxy {
        let raw = unmask_proxy(&req.proxy, &app.proxy_raw());
        if raw.is_empty() {
            return Json(check_error("proxy is ticked but no proxy is set")).into_response();
        }
        match Proxy::parse(&raw) {
            Ok(p) => Some(p),
            Err(e) => return Json(check_error(e)).into_response(),
        }
    } else {
        None
    };
    Json(check_stream(&req.url, proxy.as_ref()).await).into_response()
}

// ---------------------------------------------------------------- main

/// Embedded files have no modification time, so tell browsers to always
/// revalidate; otherwise they keep running old scripts after an upgrade.
async fn no_cache(mut r: Response) -> Response {
    r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    r
}

fn fail(msg: impl std::fmt::Display) -> ! {
    eprintln!("{msg}");
    std::process::exit(1)
}

/// Release version, from the VERSION file at the top of the repository.
const VERSION: &str = include_str!("../VERSION");

#[cfg(windows)]
const DEFAULT_CONFIG: &str = "settings.ini";
#[cfg(not(windows))]
const DEFAULT_CONFIG: &str = "/etc/internet-radio-proxifier/settings.ini";

/// Written to the settings file path when the file does not exist yet.
const EXAMPLE_SETTINGS: &str = include_str!("../settings.example.ini");

fn usage() -> String {
    format!(
        "Usage: internet-radio-proxifier -h ADDRESS [-c SETTINGS_FILE]

  -h, --host ADDRESS     listen address, e.g. :8123 (all interfaces) or 192.168.1.1:8123
  -c, --config FILE      settings file (default: {DEFAULT_CONFIG}); created
                         with example stations if it does not exist
      --help             show this help
  -V, --version          show the version

The settings file holds the proxy, the station list and the optional settings
page password (password = ..., user name admin)."
    )
}

/// Returns (address, station file); exits on bad arguments.
fn parse_args(mut args: impl Iterator<Item = String>) -> (String, String) {
    let bad = |msg: &str| -> ! { fail(format!("internet-radio-proxifier: {msg}\n\n{}", usage())) };
    let (mut address, mut config) = (None, None);
    while let Some(arg) = args.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f.to_string(), Some(v.to_string())),
            _ => (arg.clone(), None),
        };
        let slot = match flag.as_str() {
            "-h" | "--host" => &mut address,
            "-c" | "--config" => &mut config,
            "--help" => {
                println!("{}", usage());
                std::process::exit(0)
            }
            "-V" | "--version" => {
                println!("internet-radio-proxifier {}", VERSION.trim());
                std::process::exit(0)
            }
            _ => bad(&format!("unknown argument {arg:?}")),
        };
        match inline.or_else(|| args.next()).filter(|v| !v.is_empty()) {
            Some(v) => *slot = Some(v),
            None => bad(&format!("{flag} needs a value")),
        }
    }
    let address = address.unwrap_or_else(|| bad("-h/--host is required"));
    (address, config.unwrap_or_else(|| DEFAULT_CONFIG.into()))
}

fn main() {
    let (address, stations_file) = parse_args(std::env::args().skip(1));
    match stations::create_if_missing(&stations_file, EXAMPLE_SETTINGS) {
        Ok(true) => eprintln!("Created {stations_file} with example stations"),
        Ok(false) => {}
        Err(e) => fail(format!("Cannot create {stations_file}: {e}")),
    }
    let stations::Settings { stations: list, proxy: proxy_raw, password, autoplay, random_start, random, retries } =
        stations::load(&stations_file).unwrap_or_else(|e| fail(format!("Cannot read {stations_file}: {e}")));
    if list.is_empty() {
        fail(format!("No stations found in {stations_file}"));
    }
    let proxy = if proxy_raw.is_empty() {
        None
    } else {
        Some(Proxy::parse(&proxy_raw).unwrap_or_else(|e| fail(e)))
    };
    if password.is_empty() {
        eprintln!("Settings page has no password (no password = ... in {stations_file})");
    }

    let app = Arc::new(App {
        stations: RwLock::new(Arc::new(list)),
        proxy: RwLock::new((proxy_raw, proxy)),
        autoplay: std::sync::atomic::AtomicBool::new(autoplay),
        random_start: std::sync::atomic::AtomicBool::new(random_start),
        random: std::sync::atomic::AtomicBool::new(random),
        retries: AtomicUsize::new(retries as usize),
        stations_file,
        sessions: Mutex::new(HashMap::new()),
        titles: Mutex::new(HashMap::new()),
        inflight: Mutex::new(HashSet::new()),
        save_lock: Mutex::new(()),
        next_id: AtomicUsize::new(1),
    });

    let router = Router::new()
        .route("/", get(index))
        .route("/api/stations", get(api_stations))
        .route("/ws", get(ws))
        .route("/static/{*path}", get(static_file))
        .route("/sw.js", get(|| async { serve_asset("sw.js") }))
        .route("/manifest.webmanifest", get(|| async { serve_asset("manifest.webmanifest") }))
        .route("/settings", get(settings_page))
        .route("/settings/api/stations", get(settings_get).put(settings_put))
        .route("/settings/api/check", post(settings_check))
        .route("/station/{index}", get(station_stream))
        .layer(axum::middleware::map_response(no_cache)).with_state(app.clone());

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(4)
        .build()
        .unwrap_or_else(|e| fail(e));
    rt.block_on(async move {
        // ":8123" means all interfaces, IPv6 and IPv4.
        let listener = if let Some(port) = address.strip_prefix(':') {
            // On Windows an IPv6 socket does not accept IPv4, so use IPv4 there.
            let any = if cfg!(windows) { "0.0.0.0" } else { "[::]" };
            match tokio::net::TcpListener::bind(format!("{any}:{port}")).await {
                Ok(l) => Ok(l),
                Err(_) => tokio::net::TcpListener::bind(format!("0.0.0.0:{port}")).await,
            }
            .unwrap_or_else(|e| fail(format!("Cannot listen on {address}: {e}")))
        } else {
            tokio::net::TcpListener::bind(&address)
                .await
                .unwrap_or_else(|e| fail(format!("Cannot listen on {address}: {e}")))
        };
        eprintln!("Listening on {address}");
        tokio::spawn(title_loop(app));
        // Send small websocket updates right away instead of waiting for ACKs.
        let listener = axum::serve::ListenerExt::tap_io(listener, |tcp| {
            let _ = tcp.set_nodelay(true);
        });
        axum::serve(listener, router).await.unwrap_or_else(|e| fail(e));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_mask() {
        let raw = "socks5h://guest:s3cr%40t@192.168.1.10:1080";
        assert_eq!(mask_proxy(raw), "socks5h://guest:********@192.168.1.10:1080");
        assert_eq!(unmask_proxy("socks5h://guest:********@192.168.1.10:1080", raw), raw);
        assert_eq!(unmask_proxy("socks5h://guest:new@192.168.1.10", raw), "socks5h://guest:new@192.168.1.10");
        assert_eq!(mask_proxy("socks5://192.168.1.10:1080"), "socks5://192.168.1.10:1080");
        assert_eq!(mask_proxy("socks5://user@192.168.1.10"), "socks5://user@192.168.1.10");
    }

    #[test]
    fn escaping() {
        assert_eq!(esc(r#"<a href="x">&'"#), "&lt;a href=&#34;x&#34;&gt;&amp;&#39;");
    }
}
