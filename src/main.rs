//! Web radio player: streams internet radio stations through the router,
//! with a station manager and an optional proxy per station.
//!
//! Usage: internet-radio-proxifier -a ADDRESS -p PORT [-c SETTINGS_FILE]
//! The settings file holds the proxy, the station list and the settings
//! page password.

mod audio;
mod http;
mod stations;

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc};
use tokio::time::{Instant, MissedTickBehavior, interval, timeout, timeout_at};

use http::Proxy;
use stations::Station;

const MASK: &str = "********";
const TITLE_INTERVAL: Duration = Duration::from_secs(5);
const HIDDEN_TITLE_INTERVAL: Duration = Duration::from_secs(30);
const BITRATE_ONLY_INTERVAL: Duration = Duration::from_secs(5 * 60);
const WS_QUEUE: usize = 8;
const WS_WRITE_TIMEOUT: Duration = Duration::from_secs(10);

struct Session {
    station: usize,
    playing: bool,
    hidden: bool,
    tx: mpsc::Sender<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct RelayKey {
    url: String,
    proxy: Option<Arc<Proxy>>,
    user_agent: Arc<str>,
}

impl RelayKey {
    fn new(st: &Station, proxy: Option<Arc<Proxy>>, user_agent: Arc<str>) -> Self {
        Self { url: st.url.clone(), proxy, user_agent }
    }
}

#[derive(Clone)]
struct RelayError {
    kind: std::io::ErrorKind,
    message: Arc<str>,
}

impl RelayError {
    fn new(e: std::io::Error) -> Self {
        Self { kind: e.kind(), message: Arc::from(e.to_string()) }
    }

    fn to_io(&self) -> std::io::Error {
        std::io::Error::new(self.kind, self.message.to_string())
    }
}

type RelayResult = Result<Bytes, RelayError>;

struct Relay {
    content_type: String,
    writable: tokio::sync::Notify,
    subscribers: Mutex<HashMap<usize, mpsc::Sender<RelayResult>>>,
    notify: tokio::sync::Notify,
}

struct App {
    stations: RwLock<Arc<Vec<Station>>>,
    /// Raw proxy URL as saved, and its parsed form.
    proxy: RwLock<(String, Option<Arc<Proxy>>)>,
    /// Start playing as soon as the player page opens.
    autoplay: std::sync::atomic::AtomicBool,
    /// With autoplay: start with a random station.
    random_start: std::sync::atomic::AtomicBool,
    /// Next / Previous pick a random station.
    random: std::sync::atomic::AtomicBool,
    /// Reconnects to a station that stopped, before the player skips it.
    retries: AtomicUsize,
    /// User-Agent override; empty uses the built-in default.
    user_agent: RwLock<Arc<str>>,
    stations_file: String,
    sessions: Mutex<HashMap<u64, Session>>,
    /// Live upstreams shared by everyone playing the same stream and proxy.
    relays: Mutex<HashMap<RelayKey, Arc<Relay>>>,
    opening: Mutex<HashMap<RelayKey, broadcast::Sender<OpenResult>>>,
    /// Last known song title per stream URL.
    titles: Mutex<HashMap<String, Arc<str>>>,
    /// Last known bitrate ("128") per stream URL, from icy-br.
    bitrates: Mutex<HashMap<String, Arc<str>>>,
    /// Stream URLs with a title fetch running.
    inflight: Mutex<HashSet<String>>,
    bitrate_only_until: Mutex<HashMap<String, Instant>>,
    save_lock: tokio::sync::Mutex<()>,
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

    fn user_agent(&self) -> Arc<str> {
        let raw = self.user_agent_override();
        if raw.is_empty() { Arc::from(http::DEFAULT_USER_AGENT) } else { raw }
    }

    fn user_agent_override(&self) -> Arc<str> {
        self.user_agent.read().unwrap().clone()
    }

    fn proxy_raw(&self) -> String {
        self.proxy.read().unwrap().0.clone()
    }

    fn proxy_for(&self, st: &Station) -> Option<Arc<Proxy>> {
        if st.use_proxy { self.proxy.read().unwrap().1.clone() } else { None }
    }

    /// The URL the player uses for station i.
    /// Stream of station `i`; the URL has its number (i + 1), like the player.
    fn station_url(i: usize) -> String {
        format!("/station/{}", i + 1)
    }

    fn cached_title(&self, url: &str) -> Arc<str> {
        self.titles.lock().unwrap().get(url).cloned().unwrap_or_default()
    }

    /// The stream element for station `i`. `data-station` is the stream URL:
    /// a removal can leave the number the same while the station changes.
    fn player_html(i: usize, st: &Station, count: usize) -> String {
        format!(
            r#"<source id="audio-source" src="{}" data-index="{i}" data-count="{count}" data-station="{}" data-name="{}" type="audio/mpeg">"#,
            esc(&Self::station_url(i)),
            esc(&st.url),
            esc(&st.name)
        )
    }

    /// Station name, song title and the corner marks (proxy, bitrate).
    /// Player box for station `i` (shown to people as number i + 1).
    fn info_html(&self, i: usize, st: &Station, title: &str) -> String {
        let proxy = if self.proxy_for(st).is_some() {
            r#"<span class="tag-proxy" title="Plays through the proxy">proxy</span>"#
        } else {
            "<span></span>"
        };
        let br = self.bitrates.lock().unwrap().get(&st.url).cloned().unwrap_or_default();
        let br = if br.is_empty() { String::new() } else { format!("{} kbps", esc(&br)) };
        let fav = if st.favorite {
            r#"<button id="station-fav" type="button" aria-pressed="true" title="Remove from favorites">&#x2605;</button>"#
        } else {
            r#"<button id="station-fav" type="button" aria-pressed="false" title="Add to favorites">&#x2606;</button>"#
        };
        format!(
            "<div id=\"station-tags\">{proxy}<span class=\"tag-bitrate\">{br}</span></div>\n<div id=\"station-name\" data-index=\"{i}\"><span class=\"station-num\">#{}</span> <a class=\"stationurl\" href=\"{}\">[{}]</a></div>\n<div id=\"station-title\">{}</div>\n{fav}",
            i + 1,
            esc(&st.url),
            esc(&st.name),
            esc(title)
        )
    }

    fn station_info(&self, list: &[Station], i: usize) -> Option<String> {
        list.get(i).map(|st| self.info_html(i, st, &self.cached_title(&st.url)))
    }

    fn send(&self, id: u64, html: String) {
        let mut sessions = self.sessions.lock().unwrap();
        let failed = sessions.get(&id).is_some_and(|s| s.tx.try_send(html).is_err());
        if failed {
            sessions.remove(&id);
        }
    }

    fn broadcast(&self, station: usize, html: &str) {
        self.sessions.lock().unwrap().retain(|_, s| {
            s.station != station || s.tx.try_send(html.to_string()).is_ok()
        });
    }

    /// Stations that actively playing clients are on, and whether every one
    /// of those clients is in a hidden tab.
    fn listened(&self) -> HashMap<usize, bool> {
        let mut stations = HashMap::new();
        for s in self.sessions.lock().unwrap().values().filter(|s| s.playing) {
            stations.entry(s.station).and_modify(|all_hidden| *all_hidden &= s.hidden).or_insert(s.hidden);
        }
        stations
    }

    /// Stations with connected player pages, including paused ones.
    fn connected(&self) -> HashSet<usize> {
        self.sessions.lock().unwrap().values().map(|s| s.station).collect()
    }
}

// ---------------------------------------------------------------- titles

/// Audio read to measure the bitrate of a stream without ICY metadata.
const MEASURE_BYTES: usize = 32 * 1024;

enum StreamTitle { NoMetadata, Unchanged, Title(String) }

/// The stream's song title and bitrate ("128", "~173" for VBR; empty when
/// unknown). The bitrate is measured from the audio frames; icy-br is only
/// used when no frames are recognised.
async fn stream_title(url: &str, proxy: Option<&Proxy>, agent: &str) -> std::io::Result<(StreamTitle, String)> {
    let mut resp = http::get(url, proxy, agent, &[("Icy-MetaData", "1")]).await?;
    if !(200..300).contains(&resp.status) {
        return Err(http::err(format!("stream returned {}", resp.status)));
    }
    let icy = bitrate(resp.header("icy-br"), resp.header("ice-audio-info"));
    let measured = |audio: &[u8]| audio::measure(audio).map(|b| b.label()).unwrap_or_else(|| icy.clone());
    // A stream without ICY metadata has no title, only the bitrate.
    let Some(metaint) = resp.header("icy-metaint").and_then(|v| v.trim().parse::<usize>().ok()) else {
        let mut audio = vec![0u8; MEASURE_BYTES];
        let br = match resp.body.read_full(&mut audio).await {
            Ok(()) => measured(&audio),
            Err(_) if !icy.is_empty() => icy,
            Err(e) => return Err(e),
        };
        return Ok((StreamTitle::NoMetadata, br));
    };
    // The audio before the first metadata block, kept to measure the bitrate.
    let mut audio = vec![0u8; metaint.min(64 * 1024)];
    resp.body.read_full(&mut audio).await?;
    resp.body.skip(metaint - audio.len()).await?;
    let br = measured(&audio);
    let mut len = [0u8];
    resp.body.read_full(&mut len).await?;
    let mut meta = vec![0u8; len[0] as usize * 16];
    resp.body.read_full(&mut meta).await?;
    Ok((icy_title(&meta).map_or(StreamTitle::Unchanged, StreamTitle::Title), br))
}

/// kbps from icy-br ("128", some servers send "128,128") or, failing that,
/// ice-audio-info ("ice-samplerate=44100;ice-bitrate=128;ice-channels=2").
fn bitrate(icy_br: Option<&str>, audio_info: Option<&str>) -> String {
    let num = |v: &str| {
        let v = v.trim();
        (!v.is_empty() && v.len() <= 5 && v.bytes().all(|b| b.is_ascii_digit()) && v != "0").then(|| v.to_string())
    };
    icy_br
        .and_then(|v| num(v.split(',').next().unwrap_or("")))
        .or_else(|| {
            audio_info?.split(';').find_map(|kv| {
                let (k, v) = kv.split_once('=')?;
                k.trim().ends_with("bitrate").then(|| num(v)).flatten()
            })
        })
        .unwrap_or_default()
}

/// ICY metadata has no declared charset: many stations send UTF-8, older
/// ones Windows-1252 / Latin-1 ("W\xfclli"). Anything that is not valid
/// UTF-8 is read as Windows-1252.
fn decode_title(b: &[u8]) -> String {
    if let Ok(s) = std::str::from_utf8(b) {
        return s.to_string();
    }
    // Windows-1252 0x80-0x9F; the rest of the bytes are Latin-1.
    const CP1252: [char; 32] = [
        '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8d}', 'Ž', '\u{8f}',
        '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9d}', 'ž', 'Ÿ',
    ];
    b.iter()
        .map(|&c| match c {
            0x80..=0x9f => CP1252[(c - 0x80) as usize],
            _ => c as char,
        })
        .collect()
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

fn icy_title(meta: &[u8]) -> Option<String> {
    for part in meta.split(|&b| b == b';') {
        let p = trim_bytes(part);
        if p.len() >= 14 && p.starts_with(b"StreamTitle='") {
            return Some(decode_title(&p[13..p.len() - 1]));
        }
    }
    None
}

fn stream_is_current(app: &App, url: &str) -> bool {
    app.stations().iter().any(|st| st.url == url)
}

fn prune_metadata_caches(app: &App, list: &[Station]) {
    let urls: HashSet<&str> = list.iter().map(|st| st.url.as_str()).collect();
    app.titles.lock().unwrap().retain(|url, _| urls.contains(url.as_str()));
    app.bitrates.lock().unwrap().retain(|url, _| urls.contains(url.as_str()));
    app.bitrate_only_until.lock().unwrap().retain(|url, _| urls.contains(url.as_str()));
}

fn broadcast_stream_info(app: &App, url: &str, title: &str) {
    let current = app.stations();
    for i in app.connected() {
        if let Some(now) = current.get(i).filter(|now| now.url == url) {
            app.broadcast(i, &app.info_html(i, now, title));
        }
    }
}

fn update_cached_value(cache: &Mutex<HashMap<String, Arc<str>>>, url: &str, value: &str) -> bool {
    let mut cache = cache.lock().unwrap();
    if cache.get(url).map_or("", Arc::as_ref) == value {
        return false;
    }
    cache.insert(url.to_string(), Arc::from(value));
    true
}

fn publish_metadata(app: &App, url: &str, title: StreamTitle, bitrate: Option<&str>) {
    if !stream_is_current(app, url) {
        return;
    }
    let bitrate_changed = bitrate.is_some_and(|br| update_cached_value(&app.bitrates, url, br));
    let title_changed = match title {
        StreamTitle::NoMetadata => {
            app.bitrate_only_until.lock().unwrap().insert(url.to_string(), Instant::now() + BITRATE_ONLY_INTERVAL);
            update_cached_value(&app.titles, url, "")
        }
        StreamTitle::Unchanged => false,
        StreamTitle::Title(title) => update_cached_value(&app.titles, url, &stations::clean_title(&title)),
    };
    if title_changed || bitrate_changed {
        broadcast_stream_info(app, url, &app.cached_title(url));
    }
}

/// Fetches station k's title and sends it to everyone listening to it.
async fn refresh_station(app: &App, list: &[Station], k: usize) {
    let st = &list[k];
    let proxy = app.proxy_for(st);
    match timeout(Duration::from_secs(10), stream_title(&st.url, proxy.as_deref(), &app.user_agent())).await {
        Ok(Ok((title, br))) => publish_metadata(app, &st.url, title, Some(&br)),
        Ok(Err(e)) => eprintln!("title of {}: {e}", st.name),
        Err(_) => eprintln!("title of {}: timed out", st.name),
    }
}

/// Starts a title fetch for station k unless one for the same stream runs.
fn refresh_async(app: &Shared, list: &Arc<Vec<Station>>, k: usize) {
    let Some(st) = list.get(k) else { return };
    let proxy = app.proxy_for(st);
    let key = RelayKey::new(st, proxy, app.user_agent());
    if app.relays.lock().unwrap().contains_key(&key) || app.opening.lock().unwrap().contains_key(&key) {
        return;
    }
    if app.bitrate_only_until.lock().unwrap().get(&st.url).is_some_and(|until| *until > Instant::now()) {
        return;
    }
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
    let hidden_every = (HIDDEN_TITLE_INTERVAL.as_secs() / TITLE_INTERVAL.as_secs()) as usize;
    let mut tick_count = 0usize;
    loop {
        tick.tick().await;
        tick_count = tick_count.wrapping_add(1);
        let list = app.stations();
        for (k, hidden) in app.listened() {
            if !hidden || tick_count % hidden_every == 0 {
                refresh_async(&app, &list, k);
            }
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
    #[serde(default)]
    playing: bool,
    #[serde(default)]
    hidden: bool,
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

async fn handle_ws_message(app: &Shared, id: u64, text: &str) {
    let list = app.stations();
    let Ok(msg) = serde_json::from_str::<WsMsg>(text) else {
        eprintln!("Unparsable message: {text}");
        return;
    };
    if msg.action == "favorite" {
        let index = app.sessions.lock().unwrap().get(&id).map(|s| s.station);
        if let Some(st) = index.and_then(|i| list.get(i)) {
            toggle_favorite(app, st).await;
        }
        return;
    }
    if msg.action == "playback" {
        let station = {
            let mut sessions = app.sessions.lock().unwrap();
            let Some(s) = sessions.get_mut(&id) else { return };
            s.playing = msg.playing;
            s.hidden = msg.hidden;
            s.station
        };
        if msg.playing {
            refresh_async(app, &list, station);
        }
        return;
    }
    let station = {
        let mut sessions = app.sessions.lock().unwrap();
        let Some(s) = sessions.get_mut(&id) else { return };
        if list.is_empty() {
            return;
        }
        let cur = if s.station < list.len() { s.station } else { 0 };
        let next = match msg.action.as_str() {
            "next" | "previous" if app.random.load(Ordering::Relaxed) && list.len() > 1 => {
                // any station but the current one
                let r = random_below(list.len() - 1);
                if r >= cur { r + 1 } else { r }
            }
            // Through the play order: favorites first, then the others.
            "next" | "previous" => {
                let order = stations::play_order(&list);
                let pos = order.iter().position(|&i| i == cur).unwrap_or(0);
                let step = if msg.action == "next" { 1 } else { order.len() - 1 };
                order[(pos + step) % order.len()]
            }
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
    app.send(id, App::player_html(station, &list[station], list.len()));
    refresh_async(app, &list, station);
}

/// Writes `list` with the current options. The settings page password is
/// re-read from the file: it can only be set by editing the file.
async fn save_station_list(app: &App, list: Vec<Station>) -> Result<stations::Settings, String> {
    let settings = stations::Settings {
        stations: list,
        proxy: app.proxy_raw(),
        user_agent: app.user_agent_override().to_string(),
        password: String::new(),
        autoplay: app.autoplay.load(Ordering::Relaxed),
        random_start: app.random_start.load(Ordering::Relaxed),
        random: app.random.load(Ordering::Relaxed),
        retries: app.retries.load(Ordering::Relaxed) as u32,
    };
    persist_settings(app.stations_file.clone(), settings).await
}

async fn persist_settings(path: String, mut settings: stations::Settings) -> Result<stations::Settings, String> {
    tokio::task::spawn_blocking(move || {
        settings.password = stations::load_password(&path).map_err(|e| format!("cannot read {path}: {e}"))?;
        stations::save(&path, &settings).map_err(|e| format!("could not save {path}: {e}"))?;
        Ok(settings)
    }).await.map_err(|e| format!("settings worker failed: {e}"))?
}

/// Stars or unstars the selected station from the player: saves the file and shows the
/// new star to everyone listening to it.
async fn toggle_favorite(app: &Shared, selected: &Station) {
    let _guard = app.save_lock.lock().await;
    let mut list = (*app.stations()).clone();
    let Some((i, st)) = list.iter_mut().enumerate().find(|(_, st)| st.name == selected.name && st.url == selected.url) else { return };
    st.favorite = !st.favorite;
    let settings = match save_station_list(app, list).await {
        Ok(s) => s,
        Err(e) => return eprintln!("Saving favorite failed: {e}"),
    };
    let list = Arc::new(settings.stations);
    *app.stations.write().unwrap() = list.clone();
    let st = &list[i];
    eprintln!("{} {} favorites", st.name, if st.favorite { "added to" } else { "removed from" });
    app.broadcast(i, &app.info_html(i, st, &app.cached_title(&st.url)));
}

/// Removes the station called `name` from the player's list and saves the
/// file. Listeners on it move to the next station that is left; the others
/// keep their stream through `remap_sessions`.
async fn remove_station(app: &Shared, name: &str) -> Result<usize, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("station name is missing".into());
    }
    let _guard = app.save_lock.lock().await;
    let old = app.stations();
    let list = stations::remove((*old).clone(), name)?;
    let settings = save_station_list(app, list).await?;
    let new = Arc::new(settings.stations);
    *app.stations.write().unwrap() = new.clone();
    prune_metadata_caches(app, new.as_slice());
    let had_proxy = app.proxy.read().unwrap().1.is_some();
    remap_sessions(app, &old, &new, had_proxy);
    eprintln!("Removed station {name:?} ({} stations left)", new.len());
    Ok(new.len())
}

async fn ws_client(app: Shared, mut sock: WebSocket) {
    let id = app.next_id.fetch_add(1, Ordering::Relaxed) as u64;
    let (tx, mut rx) = mpsc::channel(WS_QUEUE);
    app.sessions.lock().unwrap().insert(id, Session { station: 0, playing: false, hidden: false, tx });
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
                    handle_ws_message(&app, id, t.as_str()).await;
                }
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => last_seen = Instant::now(),
            },
            html = rx.recv() => match html {
                Some(html) => {
                    if !matches!(timeout(WS_WRITE_TIMEOUT, sock.send(Message::Text(html.into()))).await, Ok(Ok(()))) {
                        break;
                    }
                }
                None => break,
            },
            _ = ping.tick() => {
                if last_seen.elapsed() > Duration::from_secs(75)
                    || !matches!(timeout(WS_WRITE_TIMEOUT, sock.send(Message::Ping(Bytes::new()))).await, Ok(Ok(())))
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

struct Asset {
    data: &'static [u8],
    content_type: &'static str,
    etag: OnceLock<String>,
}

macro_rules! assets {
    ($($name:literal => $ct:literal),* $(,)?) => {
        static ASSETS: OnceLock<Vec<(&str, Asset)>> = OnceLock::new();

        fn asset(name: &str) -> Option<&'static Asset> {
            ASSETS.get_or_init(|| vec![
                $(($name, Asset {
                    data: include_bytes!(concat!("../assets/static/", $name)),
                    content_type: $ct,
                    etag: OnceLock::new(),
                })),*
            ]).iter().find(|(n, _)| *n == name).map(|(_, asset)| asset)
        }
    };
}

assets! {
    "settings.css" => "text/css; charset=utf-8",
    "settings.js" => "text/javascript; charset=utf-8",
    "htmx.min.js" => "text/javascript; charset=utf-8",
    "htmx-ws.js" => "text/javascript; charset=utf-8",
    "apple-touch-icon.png" => "image/png",
    "favicon.png" => "image/png",
    "favicon.svg" => "image/svg+xml",
    "icon-192.png" => "image/png",
    "icon-512.png" => "image/png",
    "manifest.webmanifest" => "application/manifest+json",
    "script.js" => "text/javascript; charset=utf-8",
    "style.css" => "text/css; charset=utf-8",
    "sw.js" => "text/javascript; charset=utf-8",
}

fn content_etag(data: &[u8]) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for &b in data {
        hash = (hash ^ b as u64).wrapping_mul(0x100000001b3);
    }
    format!("\"{:x}-{hash:016x}\"", data.len())
}

fn etag_matches(value: &str, etag: &str) -> bool {
    value.split(',').map(str::trim).any(|candidate| {
        candidate == "*" || candidate == etag || candidate.strip_prefix("W/") == Some(etag)
    })
}

fn serve_asset(name: &str, headers: &HeaderMap) -> Response {
    let Some(asset) = asset(name) else { return StatusCode::NOT_FOUND.into_response() };
    let etag = asset.etag.get_or_init(|| content_etag(asset.data));
    if headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()).is_some_and(|v| etag_matches(v, etag)) {
        return (StatusCode::NOT_MODIFIED, [(header::ETAG, etag.as_str())]).into_response();
    }
    ([(header::CONTENT_TYPE, asset.content_type), (header::ETAG, etag.as_str())], asset.data).into_response()
}

async fn static_file(Path(path): Path<String>, headers: HeaderMap) -> Response {
    serve_asset(&path, &headers)
}

async fn index(State(app): State<Shared>) -> Html<String> {
    let autoplay = match (app.autoplay.load(Ordering::Relaxed), app.random_start.load(Ordering::Relaxed)) {
        (true, true) => "autoplay data-autoplay data-random-start",
        (true, false) => "autoplay data-autoplay",
        _ => "",
    };
    let attrs = format!("{autoplay} data-retries=\"{}\"", app.retries.load(Ordering::Relaxed));
    let list = app.stations();
    let source = list.first().map(|st| App::player_html(0, st, list.len())).unwrap_or_default();
    Html(INDEX.replace("{{autoplay}}", &attrs).replace("{{source}}", &source))
}

#[derive(Serialize)]
struct PickerEntry<'a> {
    /// Position in the file (the station number is index + 1).
    index: usize,
    name: &'a str,
    proxy: bool,
    favorite: bool,
}

async fn api_stations(State(app): State<Shared>) -> Response {
    let list = app.stations();
    // In play order: favorites first.
    let out: Vec<PickerEntry> = stations::play_order(&list)
        .into_iter()
        .map(|i| PickerEntry { index: i, name: &list[i].name, proxy: list[i].use_proxy, favorite: list[i].favorite })
        .collect();
    Json(out).into_response()
}

#[derive(Deserialize)]
struct RemoveReq {
    #[serde(default)]
    name: String,
}

/// Removes a station from the picker. Like the favorite star it works without
/// the settings page password: the player is the open page. A JSON body means
/// a cross-site form cannot trigger it.
async fn api_remove_station(State(app): State<Shared>, body: Bytes) -> Response {
    let Ok(req) = serde_json::from_slice::<RemoveReq>(&body) else {
        return json_error(StatusCode::BAD_REQUEST, "invalid JSON");
    };
    // Finish publishing a durable change even if the HTTP client disconnects.
    match tokio::spawn(async move { remove_station(&app, &req.name).await }).await {
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        Ok(Ok(count)) => Json(serde_json::json!({ "ok": true, "count": count })).into_response(),
        Ok(Err(e)) => json_error(StatusCode::BAD_REQUEST, e),
    }
}

enum RelayRead {
    Audio(usize),
    Title(String),
    Eof,
}

/// Removes ICY metadata blocks and returns their titles separately.
struct IcyReader {
    body: http::Body,
    metaint: Option<usize>,
    audio_left: usize,
}

impl IcyReader {
    fn new(body: http::Body, metaint: Option<usize>) -> Self {
        Self { body, metaint, audio_left: metaint.unwrap_or(0) }
    }

    async fn read(&mut self, buf: &mut [u8]) -> std::io::Result<RelayRead> {
        if self.metaint.is_some() && self.audio_left == 0 {
            let mut len = [0u8];
            self.body.read_full(&mut len).await?;
            let mut meta = vec![0u8; len[0] as usize * 16];
            self.body.read_full(&mut meta).await?;
            self.audio_left = self.metaint.unwrap();
            if let Some(title) = icy_title(&meta) {
                return Ok(RelayRead::Title(title));
            }
        }
        let n = match self.metaint {
            Some(_) => buf.len().min(self.audio_left),
            None => buf.len(),
        };
        let n = self.body.read(&mut buf[..n]).await?;
        if n == 0 {
            return Ok(RelayRead::Eof);
        }
        self.audio_left = self.audio_left.saturating_sub(n);
        Ok(RelayRead::Audio(n))
    }
}

struct RelaySubscription {
    app: Shared,
    key: RelayKey,
    id: usize,
    relay: Arc<Relay>,
}

impl Drop for RelaySubscription {
    fn drop(&mut self) {
        // Joining holds the map lock too, so a new subscriber cannot appear
        // between the empty check and removing the relay.
        let mut relays = self.app.relays.lock().unwrap();
        let mut subscribers = self.relay.subscribers.lock().unwrap();
        subscribers.remove(&self.id);
        let empty = subscribers.is_empty();
        if empty && relays.get(&self.key).is_some_and(|current| Arc::ptr_eq(current, &self.relay)) {
            relays.remove(&self.key);
        }
        drop(subscribers);
        drop(relays);
        self.relay.writable.notify_one();
        if empty {
            self.relay.notify.notify_one();
        }
    }
}

fn shareable_audio(content_type: &str) -> bool {
    let mime = content_type.split(';').next().unwrap_or("").trim();
    ["audio/mpeg", "audio/aac", "audio/aacp", "audio/x-aac"].iter().any(|candidate| mime.eq_ignore_ascii_case(candidate))
}

fn relay_response(app: &Shared, key: RelayKey, relay: Arc<Relay>) -> Response {
    let Ok(content_type) = HeaderValue::from_str(&relay.content_type) else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let id = app.next_id.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = mpsc::channel(4);
    relay.subscribers.lock().unwrap().insert(id, tx);
    relay.writable.notify_one();
    let subscription = RelaySubscription { app: app.clone(), key, id, relay: relay.clone() };
    Response::builder()
        .header(header::CONTENT_TYPE, content_type)
        .body(Body::from_stream(ChannelStream { rx, subscription }))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

// A fast upstream can fill every queue before the HTTP writers are scheduled.
// Pace it to the fastest listener. Evict a lagging peer only after the writers
// have had a turn and at least one listener can accept the next chunk.
async fn send_audio(relay: &Relay, bytes: Bytes) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut yielded = false;
    loop {
        {
            let mut subscribers = relay.subscribers.lock().unwrap();
            if subscribers.is_empty() {
                return false;
            }
            let full = subscribers.values().filter(|tx| tx.capacity() == 0).count();
            if full == 0 || (yielded && full < subscribers.len()) {
                subscribers.retain(|_, tx| tx.try_send(Ok(bytes.clone())).is_ok());
                return !subscribers.is_empty();
            }
        }
        if yielded {
            if timeout_at(deadline, relay.writable.notified()).await.is_err() {
                return false;
            }
        } else {
            tokio::task::yield_now().await;
            yielded = true;
        }
    }
}

fn publish_relay_bitrate(app: &App, key: &RelayKey, audio: &[u8], icy: &str, has_metadata: bool) {
    let br = audio::measure(audio).map(|b| b.label()).unwrap_or_else(|| icy.to_string());
    let title = if has_metadata { StreamTitle::Unchanged } else { StreamTitle::NoMetadata };
    publish_metadata(app, &key.url, title, Some(&br));
}

async fn relay_loop(app: Shared, key: RelayKey, relay: Arc<Relay>, mut decoder: IcyReader, icy: String) {
    let has_metadata = decoder.metaint.is_some();
    let mut audio = Vec::with_capacity(MEASURE_BYTES);
    let mut measured = false;
    let mut buf = vec![0u8; 16 * 1024];
    let result: std::io::Result<()> = loop {
        let read = tokio::select! {
            _ = relay.notify.notified() => {
                if relay.subscribers.lock().unwrap().is_empty() {
                    break Ok(());
                }
                continue;
            }
            r = decoder.read(&mut buf) => r,
        };
        match read {
            Ok(RelayRead::Audio(n)) => {
                if !measured {
                    let take = (MEASURE_BYTES - audio.len()).min(n);
                    audio.extend_from_slice(&buf[..take]);
                    if audio.len() == MEASURE_BYTES {
                        measured = true;
                        publish_relay_bitrate(&app, &key, &audio, &icy, has_metadata);
                    }
                }
                buf.truncate(n);
                let bytes = Bytes::from(buf);
                if !send_audio(&relay, bytes).await {
                    break Ok(());
                }
                buf = vec![0u8; 16 * 1024];
            }
            Ok(RelayRead::Title(title)) => {
                publish_metadata(&app, &key.url, StreamTitle::Title(title), None);
            }
            Ok(RelayRead::Eof) => break Ok(()),
            Err(e) => break Err(e),
        }
    };
    if !measured && result.is_ok() {
        publish_relay_bitrate(&app, &key, &audio, &icy, has_metadata);
    }
    let mut relays = app.relays.lock().unwrap();
    if relays.get(&key).is_some_and(|current| Arc::ptr_eq(current, &relay)) {
        relays.remove(&key);
    }
    drop(relays);
    let error = result.err().map(RelayError::new);
    let mut subscribers = relay.subscribers.lock().unwrap();
    if let Some(error) = error {
        for tx in subscribers.values() {
            let _ = tx.try_send(Err(error.clone()));
        }
    }
    subscribers.clear();
}

/// The stream of station `index`, shared by everyone listening to the same URL.
async fn station_stream(State(app): State<Shared>, Path(index): Path<String>) -> Response {
    let list = app.stations();
    // /station/1 is the first station: the same numbers as in the player.
    let Some(st) = index.parse::<usize>().ok().and_then(|n| n.checked_sub(1)).and_then(|i| list.get(i)) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let proxy = app.proxy_for(st);
    let key = RelayKey::new(st, proxy, app.user_agent());
    loop {
        if let Some(relay) = app.relays.lock().unwrap().get(&key) {
            return relay_response(&app, key.clone(), relay.clone());
        }
        let waiting = {
            let mut opening = app.opening.lock().unwrap();
            match opening.entry(key.clone()) {
                std::collections::hash_map::Entry::Occupied(entry) => Some(entry.get().subscribe()),
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(broadcast::channel(1).0);
                    None
                }
            }
        };
        if let Some(mut waiting) = waiting {
            match waiting.recv().await {
                Ok(OpenResult::Private) => return open_relay(&app, &key, st).await.0,
                Ok(OpenResult::Failed) => return StatusCode::BAD_GATEWAY.into_response(),
                // Retry the lookup if the opener installed a relay or disconnected.
                Ok(OpenResult::Shared) | Err(_) => continue,
            }
        }
        let _opening = Opening { app: &app, key: &key };
        let (response, result) = open_relay(&app, &key, st).await;
        if let Some(tx) = app.opening.lock().unwrap().get(&key) {
            let _ = tx.send(result);
        }
        return response;
    }
}

#[derive(Clone, Copy)]
enum OpenResult { Shared, Private, Failed }

struct Opening<'a> {
    app: &'a App,
    key: &'a RelayKey,
}

impl Drop for Opening<'_> {
    fn drop(&mut self) {
        self.app.opening.lock().unwrap().remove(self.key);
    }
}

async fn open_relay(app: &Shared, key: &RelayKey, st: &Station) -> (Response, OpenResult) {
    // Short enough that the player moves on quickly from a dead station.
    let resp = match timeout(Duration::from_secs(10), http::get(&st.url, key.proxy.as_deref(), &key.user_agent, &[("Icy-MetaData", "1")])).await {
        Ok(Ok(r)) if (200..300).contains(&r.status) => r,
        Ok(Ok(r)) => {
            eprintln!("{}: server answered {}", st.name, r.status_text);
            return (StatusCode::BAD_GATEWAY.into_response(), OpenResult::Failed);
        }
        Ok(Err(e)) => {
            eprintln!("{}: {e}", st.name);
            return (StatusCode::BAD_GATEWAY.into_response(), OpenResult::Failed);
        }
        Err(_) => {
            eprintln!("{}: no answer within 10 s", st.name);
            return (StatusCode::BAD_GATEWAY.into_response(), OpenResult::Failed);
        }
    };
    let shareable = resp.header("content-type").is_some_and(shareable_audio);
    let content_type = match resp.header("content-type") {
        None | Some("") | Some("application/octet-stream") => "audio/mpeg".to_string(),
        Some(c) => c.to_string(),
    };
    let icy = bitrate(resp.header("icy-br"), resp.header("ice-audio-info"));
    let metaint = resp.header("icy-metaint").and_then(|v| v.trim().parse::<usize>().ok()).filter(|&n| n > 0);
    let decoder = IcyReader::new(resp.body, metaint);
    let relay = Arc::new(Relay {
        content_type,
        writable: tokio::sync::Notify::new(),
        subscribers: Mutex::new(HashMap::new()),
        notify: tokio::sync::Notify::new(),
    });
    let response = relay_response(app, key.clone(), relay.clone());
    // MP3 and ADTS AAC can synchronise after joining mid-stream; container
    // formats such as Ogg need their initial headers, so keep those per client.
    if shareable {
        app.relays.lock().unwrap().insert(key.clone(), relay.clone());
    }
    tokio::spawn(relay_loop(app.clone(), key.clone(), relay, decoder, icy));
    (response, if shareable { OpenResult::Shared } else { OpenResult::Private })
}

struct ChannelStream {
    rx: mpsc::Receiver<RelayResult>,
    subscription: RelaySubscription,
}

impl futures_core::Stream for ChannelStream {
    type Item = std::io::Result<Bytes>;
    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        use std::task::Poll;
        match self.rx.poll_recv(cx) {
            Poll::Ready(Some(Ok(bytes))) => {
                self.subscription.relay.writable.notify_one();
                Poll::Ready(Some(Ok(bytes)))
            }
            Poll::Ready(Some(Err(e))) => Poll::Ready(Some(Err(e.to_io()))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
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
    #[serde(default)]
    favorite: bool,
}

#[derive(Serialize, Deserialize)]
struct ConfigJson {
    user_agent: Option<String>,
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
        user_agent: Some(app.user_agent_override().to_string()),
        autoplay: app.autoplay.load(Ordering::Relaxed),
        random_start: app.random_start.load(Ordering::Relaxed),
        random: app.random.load(Ordering::Relaxed),
        retries: app.retries.load(Ordering::Relaxed) as u32,
        stations: list
            .iter()
            .map(|s| StationJson { name: s.name.clone(), url: s.url.clone(), use_proxy: s.use_proxy, favorite: s.favorite })
            .collect(),
    }
}

fn json_error(code: StatusCode, msg: impl Into<String>) -> Response {
    (code, Json(serde_json::json!({ "error": msg.into() }))).into_response()
}

async fn check_auth(app: &App, headers: &HeaderMap) -> Result<(), Response> {
    // Read from the file every time: it is changed only by editing the file,
    // and that takes effect without a restart. Keep disk I/O off the executor.
    let path = app.stations_file.clone();
    let password = match tokio::task::spawn_blocking(move || stations::load_password(&path)).await {
        Ok(Ok(password)) => password,
        Ok(Err(e)) => {
            eprintln!("Cannot read {}: {e}", app.stations_file);
            return Err(json_error(StatusCode::INTERNAL_SERVER_ERROR, "cannot read the settings file"));
        }
        Err(e) => {
            eprintln!("Settings password worker failed: {e}");
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
    if let Err(r) = check_auth(&app, &headers).await {
        return r;
    }
    Html(SETTINGS.replace("{{user_agent_default}}", &esc(http::DEFAULT_USER_AGENT))).into_response()
}

async fn settings_get(State(app): State<Shared>, headers: HeaderMap) -> Response {
    if let Err(r) = check_auth(&app, &headers).await {
        return r;
    }
    Json(config_json(&app)).into_response()
}

async fn settings_put(State(app): State<Shared>, headers: HeaderMap, body: Bytes) -> Response {
    if let Err(r) = check_auth(&app, &headers).await {
        return r;
    }
    let Ok(input) = serde_json::from_slice::<ConfigJson>(&body) else {
        return json_error(StatusCode::BAD_REQUEST, "invalid JSON");
    };
    // The transaction owns the lock through publication, including after a disconnect.
    match tokio::spawn(apply_settings(app, input)).await {
        Ok(response) => response,
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn apply_settings(app: Shared, input: ConfigJson) -> Response {
    let list: Vec<Station> = input.stations
        .into_iter()
        .map(|s| Station { name: s.name, url: s.url, use_proxy: s.use_proxy, favorite: s.favorite })
        .collect();
    let list = match stations::validate(list) {
        Ok(l) => l,
        Err(e) => return json_error(StatusCode::BAD_REQUEST, e),
    };
    let _guard = app.save_lock.lock().await;
    let raw = unmask_proxy(&input.proxy, &app.proxy_raw());
    let parsed = if raw.is_empty() {
        None
    } else {
        match Proxy::parse(&raw) {
            Ok(p) => Some(Arc::new(p)),
            Err(e) => return json_error(StatusCode::BAD_REQUEST, e),
        }
    };
    let current_agent = app.user_agent_override();
    let user_agent = match http::validate_user_agent(input.user_agent.as_deref().unwrap_or(&current_agent)) {
        Ok(value) => value.to_string(),
        Err(e) => return json_error(StatusCode::BAD_REQUEST, e),
    };
    let settings = stations::Settings { user_agent, stations: list, proxy: raw, password: String::new(), autoplay: input.autoplay,
        random_start: input.random_start,
        random: input.random,
        retries: input.retries.min(stations::MAX_RETRIES),
    };
    let settings = match persist_settings(app.stations_file.clone(), settings).await {
        Ok(settings) => settings,
        Err(e) => {
            eprintln!("Saving settings failed: {e}");
            return json_error(StatusCode::INTERNAL_SERVER_ERROR, e);
        }
    };
    let has_proxy = parsed.is_some();
    let stations::Settings { stations: list, proxy: raw, autoplay, random_start, random, retries, user_agent, .. } = settings;
    *app.user_agent.write().unwrap() = Arc::from(user_agent);
    app.retries.store(retries as usize, Ordering::Relaxed);
    app.autoplay.store(autoplay, Ordering::Relaxed);
    app.random_start.store(random_start, Ordering::Relaxed);
    app.random.store(random, Ordering::Relaxed);
    let had_proxy = std::mem::replace(&mut *app.proxy.write().unwrap(), (raw, parsed)).1.is_some();
    let new = Arc::new(list);
    let old = std::mem::replace(&mut *app.stations.write().unwrap(), new.clone());
    prune_metadata_caches(&app, new.as_slice());
    remap_sessions(&app, &old, &new, had_proxy);
    eprintln!("Settings saved ({} stations, proxy {has_proxy})", new.len());
    Json(config_json(&app)).into_response()
}

/// The index a listener on `old[idx]` moves to after the list changed. If
/// that stream is gone, the next station in `old` that still exists, or the
/// last station when it was the last one.
fn next_station(old: &[Station], cur: &[Station], index: &HashMap<&str, usize>, idx: usize) -> usize {
    old.iter()
        .skip(idx.saturating_add(1))
        .find_map(|st| index.get(st.url.as_str()).copied())
        .unwrap_or_else(|| cur.len().saturating_sub(1))
}

/// Keeps every listener on the same stream after the list was reordered.
/// Listeners whose station was removed are moved to the next station in the
/// old list that still exists (the last one when it was the last station).
fn remap_sessions(app: &App, old: &[Station], cur: &[Station], had_proxy: bool) {
    let has_proxy = app.proxy.read().unwrap().1.is_some();
    let mut index: HashMap<&str, usize> = HashMap::new();
    for (i, s) in cur.iter().enumerate() {
        index.entry(&s.url).or_insert(i);
    }
    app.sessions.lock().unwrap().retain(|_, s| {
        let idx = s.station;
        let previous = old.get(idx);
        let new_idx = match (previous, cur.get(idx)) {
            // Distinct station names can share a stream URL.
            (Some(a), Some(b)) if a.name == b.name && a.url == b.url => idx,
            _ => previous.and_then(|st| index.get(st.url.as_str()).copied())
                .unwrap_or_else(|| next_station(old, cur, &index, idx)),
        };
        s.station = new_idx;
        let station = &cur[new_idx];
        let station_changed = idx != new_idx || previous.is_none_or(|st| st.name != station.name || st.url != station.url);
        let tags_changed = previous.is_some_and(|st| st.favorite != station.favorite
            || (st.use_proxy && had_proxy) != (station.use_proxy && has_proxy));
        if station_changed || tags_changed {
            if s.tx.try_send(app.info_html(new_idx, station, &app.cached_title(&station.url))).is_err() {
                return false;
            }
        }
        if station_changed || old.len() != cur.len() {
            if s.tx.try_send(App::player_html(new_idx, station, cur.len())).is_err() {
                return false;
            }
        }
        true
    });
}

#[derive(Deserialize)]
struct CheckReq {
    user_agent: Option<String>,
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
async fn check_stream(url: &str, proxy: Option<&Proxy>, agent: &str) -> CheckResult {
    if !http::is_stream_url(url) {
        return check_error("URL must start with http:// or https://");
    }
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut resp = match timeout_at(deadline, http::get(url, proxy, agent, &[])).await {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => return check_error(e.to_string()),
        Err(_) => return check_error("no answer within 15 s"),
    };
    let mut r = CheckResult {
        status: resp.status_text.clone(),
        content_type: resp.header("content-type").unwrap_or("").to_string(),
        bitrate: bitrate(resp.header("icy-br"), resp.header("ice-audio-info")),
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
    // Kept to measure the real bitrate from the audio frames.
    let mut audio = Vec::with_capacity(WANT + buf.len());
    let mut n = 0;
    while n < WANT {
        match timeout_at(deadline, resp.body.read(&mut buf)).await {
            Ok(Ok(0)) => {
                r.error = format!("stream stopped after {n} bytes");
                return r;
            }
            Ok(Ok(k)) => {
                audio.extend_from_slice(&buf[..k]);
                n += k;
            }
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
    if let Some(b) = audio::measure(&audio) {
        r.bitrate = b.label();
    }
    r.ok = true;
    r
}

async fn settings_check(State(app): State<Shared>, headers: HeaderMap, body: Bytes) -> Response {
    if let Err(r) = check_auth(&app, &headers).await {
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
    let current_agent = app.user_agent();
    Json(check_stream(&req.url, proxy.as_ref(), req.user_agent.as_deref().unwrap_or(&current_agent)).await).into_response()
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
        "Usage: internet-radio-proxifier -a ADDRESS -p PORT [-c SETTINGS_FILE]

  -a, --address ADDRESS  address to listen on: :: (all interfaces, IPv6 and
                         IPv4), 0.0.0.0 (all IPv4 interfaces) or one address
                         or host name, e.g. 192.168.1.1 or localhost
  -p, --port PORT        port to listen on, e.g. 9876
  -c, --config FILE      settings file (default: {DEFAULT_CONFIG}); created
                         with example stations if it does not exist
  -h, --help             show this help
  -V, --version          show the version

The settings file holds the proxy, the station list and the optional settings
page password (password = ..., user name admin)."
    )
}

/// Returns (address, port, station file); exits on bad arguments.
fn parse_args(mut args: impl Iterator<Item = String>) -> (String, u16, String) {
    let bad = |msg: &str| -> ! { fail(format!("internet-radio-proxifier: {msg}\n\n{}", usage())) };
    let (mut address, mut port, mut config) = (None, None, None);
    while let Some(arg) = args.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f.to_string(), Some(v.to_string())),
            _ => (arg.clone(), None),
        };
        let slot = match flag.as_str() {
            "-a" | "--address" => &mut address,
            "-p" | "--port" => &mut port,
            "-c" | "--config" => &mut config,
            "-h" | "--help" => {
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
    let address = address.unwrap_or_else(|| bad("-a/--address is required"));
    // "[::1]" is accepted as well as "::1".
    let address = address.strip_prefix('[').and_then(|a| a.strip_suffix(']')).unwrap_or(&address).to_string();
    let port = port.unwrap_or_else(|| bad("-p/--port is required"));
    let port = match port.parse::<u16>() {
        Ok(p) if p > 0 => p,
        _ => bad(&format!("-p/--port: {port:?} is not a port number (1-65535)")),
    };
    (address, port, config.unwrap_or_else(|| DEFAULT_CONFIG.into()))
}

fn main() {
    let (address, port, stations_file) = parse_args(std::env::args().skip(1));
    match stations::create_if_missing(&stations_file, EXAMPLE_SETTINGS) {
        Ok(true) => eprintln!("Created {stations_file} with example stations"),
        Ok(false) => {}
        Err(e) => fail(format!("Cannot create {stations_file}: {e}")),
    }
    let stations::Settings { stations: list, proxy: proxy_raw, password, autoplay, random_start, random, retries, user_agent } =
        stations::load(&stations_file).unwrap_or_else(|e| fail(format!("Cannot read {stations_file}: {e}")));
    if list.is_empty() {
        fail(format!("No stations found in {stations_file}"));
    }
    let proxy = if proxy_raw.is_empty() {
        None
    } else {
        Some(Arc::new(Proxy::parse(&proxy_raw).unwrap_or_else(|e| fail(e))))
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
        user_agent: RwLock::new(Arc::from(http::validate_user_agent(&user_agent).unwrap_or_else(|e| fail(e)))),
        retries: AtomicUsize::new(retries as usize),
        stations_file,
        sessions: Mutex::new(HashMap::new()),
        relays: Mutex::new(HashMap::new()),
        opening: Mutex::new(HashMap::new()),
        titles: Mutex::new(HashMap::new()),
        bitrates: Mutex::new(HashMap::new()),
        inflight: Mutex::new(HashSet::new()),
        bitrate_only_until: Mutex::new(HashMap::new()),
        save_lock: tokio::sync::Mutex::new(()),
        next_id: AtomicUsize::new(1),
    });

    let router = Router::new()
        .route("/", get(index))
        .route("/api/stations", get(api_stations))
        .route("/api/stations/remove", post(api_remove_station))
        .route("/ws", get(ws))
        .route("/static/{*path}", get(static_file))
        .route("/sw.js", get(|headers: HeaderMap| async move { serve_asset("sw.js", &headers) }))
        .route("/manifest.webmanifest", get(|headers: HeaderMap| async move { serve_asset("manifest.webmanifest", &headers) }))
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
        let cannot = |e: std::io::Error| -> ! { fail(format!("Cannot listen on {address} port {port}: {e}")) };
        let listener = match tokio::net::TcpListener::bind((address.as_str(), port)).await {
            Ok(l) => l,
            // "::" on a system without IPv6: all IPv4 interfaces instead.
            Err(_) if address.parse() == Ok(std::net::Ipv6Addr::UNSPECIFIED) => {
                tokio::net::TcpListener::bind((std::net::Ipv4Addr::UNSPECIFIED, port)).await.unwrap_or_else(|e| cannot(e))
            }
            Err(e) => cannot(e),
        };
        eprintln!("Listening on {}", listener.local_addr().map(|a| a.to_string()).unwrap_or_else(|_| format!("{address} port {port}")));
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
    fn bitrates() {
        assert_eq!(bitrate(Some("128"), None), "128");
        assert_eq!(bitrate(Some(" 192,192"), None), "192");
        assert_eq!(bitrate(None, Some("ice-samplerate=44100;ice-bitrate=320;ice-channels=2")), "320");
        assert_eq!(bitrate(None, Some("bitrate=64")), "64");
        assert_eq!(bitrate(Some("Quality 0"), Some("channels=2")), "");
        assert_eq!(bitrate(None, None), "");
    }

    #[test]
    fn title_charsets() {
        assert_eq!(decode_title("Wölli – Bühne".as_bytes()), "Wölli – Bühne");
        assert_eq!(decode_title(b"W\xf6lli Und Die Band Des Jahres"), "Wölli Und Die Band Des Jahres");
        assert_eq!(decode_title(b"Rock \x96 Caf\xe9 \x93Live\x94"), "Rock – Café “Live”");
    }

    #[test]
    fn relay_strips_icy_metadata() {
        assert!(shareable_audio("audio/mpeg"));
        assert!(shareable_audio("Audio/AAC; charset=binary"));
        assert!(!shareable_audio("audio/ogg"));
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
            let addr = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut req = [0u8; 1024];
                sock.read(&mut req).await.unwrap();
                let mut meta = [0u8; 32];
                meta[..19].copy_from_slice(b"StreamTitle='Song';");
                let mut body = b"aaaa".to_vec();
                body.push(2);
                body.extend(meta);
                body.extend(b"bbbb");
                body.push(0);
                body.extend(b"cccc");
                body.push(1);
                body.extend(b"StreamTitle='';\0");
                body.extend(b"dd");
                sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: audio/mpeg\r\nicy-metaint: 4\r\n\r\n").await.unwrap();
                sock.write_all(&body).await.unwrap();
                sock.shutdown().await.unwrap();
            });
            let resp = http::get(&format!("http://{addr}/"), None, http::DEFAULT_USER_AGENT, &[]).await.unwrap();
            let mut decoder = IcyReader::new(resp.body, Some(4));
            let mut audio: Vec<u8> = Vec::new();
            let mut titles = Vec::new();
            let mut buf = [0u8; 3];
            loop {
                match decoder.read(&mut buf).await {
                    Ok(RelayRead::Audio(n)) => audio.extend(&buf[..n]),
                    Ok(RelayRead::Title(title)) => titles.push(title),
                    Ok(RelayRead::Eof) => break,
                    Err(e) => panic!("audio {audio:?}, titles {titles:?}: {e}"),
                }
            }
            server.await.unwrap();
            assert_eq!(audio, b"aaaabbbbccccdd");
            assert_eq!(titles, ["Song", ""]);
        });
    }

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
    fn player_source_identifies_station() {
        let st = Station { name: "A & B".into(), url: "http://x/a?b=1&c=2".into(), use_proxy: false, favorite: false };
        let html = App::player_html(2, &st, 3);
        assert!(html.contains(r#"src="/station/3""#));
        assert!(html.contains(r#"data-index="2""#));
        assert!(html.contains(r#"data-name="A &amp; B""#));
        assert!(html.contains(r#"data-count="3""#));
        assert!(html.contains(r#"data-station="http://x/a?b=1&amp;c=2""#));
    }

    #[test]
    fn station_urls_count_from_one() {
        assert_eq!(App::station_url(0), "/station/1");
        assert_eq!(App::station_url(446), "/station/447");
    }

    #[test]
    fn removed_station_moves_to_next() {
        fn index_of(list: &[Station]) -> HashMap<&str, usize> {
            list.iter().enumerate().map(|(i, s)| (s.url.as_str(), i)).collect()
        }
        let st = |n: &str| Station { name: n.into(), url: format!("http://x/{n}"), use_proxy: false, favorite: false };
        let old = [st("A"), st("B"), st("C"), st("D")];
        // B removed: C is next (D when C is gone as well).
        let cur = [st("A"), st("C"), st("D")];
        assert_eq!(next_station(&old, &cur, &index_of(&cur), 1), 1);
        let cur = [st("A"), st("D")];
        assert_eq!(next_station(&old, &cur, &index_of(&cur), 1), 1);
        // The last station was removed: stay on the last one that is left.
        let cur = [st("A"), st("B"), st("C")];
        assert_eq!(next_station(&old, &cur, &index_of(&cur), 3), 2);
    }

    #[test]
    fn static_assets_revalidate() {
        let response = serve_asset("script.js", &HeaderMap::new());
        assert_eq!(response.status(), StatusCode::OK);
        let etag = response.headers().get(header::ETAG).unwrap().to_str().unwrap().to_string();
        assert!(etag.starts_with('"') && etag.ends_with('"'));
        let mut headers = HeaderMap::new();
        headers.insert(header::IF_NONE_MATCH, HeaderValue::from_str(&format!("\"other\", W/{etag}")).unwrap());
        let response = serve_asset("script.js", &headers);
        assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(response.headers().get(header::ETAG).unwrap().to_str().unwrap(), etag);
    }

    #[test]
    fn escaping() {
        assert_eq!(esc(r#"<a href="x">&'"#), "&lt;a href=&#34;x&#34;&gt;&amp;&#39;");
    }
}
