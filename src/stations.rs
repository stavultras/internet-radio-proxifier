//! The settings file (INI):
//!
//!   password  = secret        (settings page password; user name "admin")
//!   proxy_url = socks5h://user:pass@192.168.1.10:1080
//!   autoplay  = true          (start playing when the player page opens)
//!
//!   [Station name]
//!   url       = https://stream.example.com/live.mp3
//!   use_proxy = true

use std::collections::HashSet;
use std::io::{self, Write};
use std::path::Path;

#[derive(Clone, Debug, PartialEq)]
pub struct Station {
    pub name: String,
    pub url: String,
    pub use_proxy: bool,
}

/// Value of an INI line: """…""", `…`, "…" or '…' are taken
/// literally; otherwise everything from the first # or ; is a comment.
fn ini_value(v: &str) -> String {
    let v = v.trim();
    if let Some(r) = v.strip_prefix("\"\"\"") {
        if let Some(i) = r.find("\"\"\"") {
            return r[..i].to_string();
        }
    }
    if let Some(r) = v.strip_prefix('`') {
        if let Some(i) = r.find('`') {
            return r[..i].to_string();
        }
    }
    if v.len() >= 2 && (v.starts_with('"') && v.ends_with('"') || v.starts_with('\'') && v.ends_with('\'')) {
        return v[1..v.len() - 1].to_string();
    }
    v[..v.find(['#', ';']).unwrap_or(v.len())].trim().to_string()
}

fn parse_bool(v: &str) -> bool {
    matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "t" | "true" | "y" | "yes" | "on")
}

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub stations: Vec<Station>,
    /// Proxy URL, "" for none.
    pub proxy: String,
    /// Settings page password, "" for none.
    pub password: String,
    /// Start playing as soon as the player page opens.
    pub autoplay: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { stations: Vec::new(), proxy: String::new(), password: String::new(), autoplay: true }
    }
}

pub fn parse(text: &str) -> Settings {
    let mut list: Vec<Station> = Vec::new();
    let mut proxy = String::new();
    let mut password = String::new();
    let mut autoplay = true;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.rfind(']').map(|i| l[..i].trim())) {
            list.push(Station { name: name.to_string(), url: String::new(), use_proxy: false });
            continue;
        }
        let Some(i) = line.find('=') else { continue };
        let (key, value) = (line[..i].trim(), ini_value(&line[i + 1..]));
        // Keys before the first [station] section are global settings.
        match (list.last_mut(), key) {
            (None, "proxy_url") => proxy = value,
            (None, "password") => password = value,
            (None, "autoplay") => autoplay = parse_bool(&value),
            (Some(st), "url") => st.url = value,
            (Some(st), "use_proxy") => st.use_proxy = parse_bool(&value),
            _ => {}
        }
    }
    Settings { stations: list, proxy: proxy.trim().to_string(), password, autoplay }
}

pub fn load(path: &str) -> io::Result<Settings> {
    Ok(parse(&std::fs::read_to_string(path)?))
}

/// Normalises and checks a station list from the settings page.
pub fn validate(list: Vec<Station>) -> Result<Vec<Station>, String> {
    if list.is_empty() {
        return Err("at least one station is required".into());
    }
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(list.len());
    for (i, st) in list.into_iter().enumerate() {
        let name = st.name.trim().to_string();
        let url = st.url.trim().to_string();
        if name.is_empty() {
            return Err(format!("station {}: name is empty", i + 1));
        }
        if name.contains(['[', ']', '\r', '\n']) {
            return Err(format!("station {name:?}: name must not contain [ ] or line breaks"));
        }
        if !seen.insert(name.to_lowercase()) {
            return Err(format!("duplicate station name {name:?}"));
        }
        if !crate::http::is_stream_url(&url) {
            return Err(format!("station {name:?}: URL must start with http:// or https://"));
        }
        out.push(Station { name, url, use_proxy: st.use_proxy });
    }
    Ok(out)
}

fn ini_quote(v: &str) -> String {
    if v.contains(['\n', '`']) {
        format!("\"\"\"{v}\"\"\"")
    } else if v.contains(['#', ';']) || v.trim() != v || v.starts_with(['"', '\'']) {
        format!("`{v}`")
    } else {
        v.to_string()
    }
}

pub fn render(s: &Settings) -> String {
    let mut out = String::new();
    if !s.password.is_empty() {
        out += &format!("password  = {}\n", ini_quote(&s.password));
    }
    if !s.proxy.is_empty() {
        out += &format!("proxy_url = {}\n", ini_quote(&s.proxy));
    }
    out += &format!("autoplay  = {}\n\n", s.autoplay);
    for st in &s.stations {
        out += &format!("[{}]\n", st.name);
        if st.use_proxy {
            out += &format!("url       = {}\nuse_proxy = true\n\n", ini_quote(&st.url));
        } else {
            out += &format!("url = {}\n\n", ini_quote(&st.url));
        }
    }
    out
}

/// Creates a new file readable only by the owner (on Unix).
fn private_file(path: &Path) -> io::Result<std::fs::File> {
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    o.open(path)
}

/// Creates the settings file from `text` (the bundled example) if it does
/// not exist yet. Returns true if it was created.
pub fn create_if_missing(path: &str, text: &str) -> io::Result<bool> {
    let p = Path::new(path);
    if p.exists() {
        return Ok(false);
    }
    if let Some(dir) = p.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = private_file(p)?;
    f.write_all(text.as_bytes())?;
    f.sync_all()?;
    Ok(true)
}

/// Writes the list atomically (temp file + rename), readable only by the
/// service user since it contains passwords.
pub fn save(path: &str, s: &Settings) -> io::Result<()> {
    let dir = Path::new(path).parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let tmp = dir.join(format!(".settings-{}-{nanos}.ini", std::process::id()));
    let res = (|| {
        let mut f = private_file(&tmp)?;
        f.write_all(render(s).as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

/// Tidies metadata some stations send as
/// "Title~Artist~~Year~~Length~Start~End~Station~...". Normal titles pass through.
pub fn clean_title(t: &str) -> String {
    let parts: Vec<&str> = t.split('~').collect();
    if parts.len() < 4 {
        return t.to_string();
    }
    let (title, artist) = (parts[0].trim(), parts[1].trim());
    match (title.is_empty(), artist.is_empty()) {
        (false, false) => format!("{artist} - {title}"),
        (false, true) => title.to_string(),
        _ => t.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "proxy_url = socks5h://u:p@192.168.1.10:1080\n\n[ROCK FM]\nurl = http://a/rock-128.mp3\n\n\
        [0N Classic Rock]\nurl       = https://b/x.mp3\nuse_proxy = true\n\n\
        [My]\nurl       = `http://s39.myradiostream.com:11590/listen/;.mp3`\n; comment\n[Q]\nurl = \"http://q/a\" \n";

    #[test]
    fn ini_round_trip() {
        let s = parse(SAMPLE);
        assert_eq!(s.proxy, "socks5h://u:p@192.168.1.10:1080");
        assert_eq!(s.password, "");
        assert!(s.autoplay);
        assert!(!parse("autoplay = false\n[A]\nurl = http://a\n").autoplay);
        let off = Settings { autoplay: false, ..s.clone() };
        assert_eq!(parse(&render(&off)), off);
        let list = &s.stations;
        assert_eq!(list.len(), 4);
        assert!(list[1].use_proxy && !list[0].use_proxy);
        assert_eq!(list[2].url, "http://s39.myradiostream.com:11590/listen/;.mp3");
        assert_eq!(list[3].url, "http://q/a");
        assert_eq!(parse(&render(&s)), s);
        for pw in ["p#w;x", " sp ", "\"q\"", "'q'", "b`t", "plain"] {
            let s = Settings { password: pw.into(), ..s.clone() };
            assert_eq!(parse(&render(&s)), s, "{pw}");
        }
    }

    #[test]
    fn titles() {
        assert_eq!(clean_title("Song~Band~~1999~~3:00~x~y~Virgin~z"), "Band - Song");
        assert_eq!(clean_title("Band - Song"), "Band - Song");
        assert_eq!(clean_title("Song~~~"), "Song");
    }

    #[test]
    fn validation() {
        let st = |n: &str, u: &str| Station { name: n.into(), url: u.into(), use_proxy: false };
        assert!(validate(vec![st(" A ", " http://x/ ")]).is_ok());
        assert!(validate(vec![st("A", "http://x"), st("a", "http://y")]).unwrap_err().contains("duplicate"));
        assert!(validate(vec![st("A[", "http://x")]).is_err());
        assert!(validate(vec![st("A", "ftp://x")]).is_err());
        assert!(validate(vec![]).is_err());
    }
}
