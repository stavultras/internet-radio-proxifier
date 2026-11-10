//! A small HTTP/1.1 client for radio streams: http and https, ICY responses
//! ("ICY 200 OK"), redirects, chunked bodies, and SOCKS5 / HTTP(S) proxies.

use std::io;
use std::net::IpAddr;
use std::sync::{Arc, OnceLock};

use rustls::pki_types::{CertificateDer, ServerName, pem::PemObject};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

// Some stream hosts (e.g. SomaFM) reject unknown clients without a User-Agent.
pub const USER_AGENT: &str = "internet-radio-proxifier (+https://github.com/stavultras/internet-radio-proxifier)";

pub trait Conn: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Conn for T {}
pub type BoxConn = Box<dyn Conn>;

pub fn err(msg: impl Into<String>) -> io::Error {
    io::Error::other(msg.into())
}

#[derive(Clone, Debug)]
pub struct Url {
    pub scheme: String,
    pub user: Option<String>,
    pub pass: Option<String>,
    pub host: String,
    pub port: u16,
    /// Path and query, always starting with "/".
    pub target: String,
}

fn default_port(scheme: &str) -> Option<u16> {
    match scheme {
        "http" => Some(80),
        "https" => Some(443),
        "socks5" | "socks5h" => Some(1080),
        _ => None,
    }
}

fn hex(b: u8) -> Option<u8> {
    (b as char).to_digit(16).map(|d| d as u8)
}

fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push(h << 4 | l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

impl Url {
    pub fn parse(s: &str) -> Option<Url> {
        let s = s.trim();
        let (scheme, rest) = s.split_once("://")?;
        let scheme = scheme.to_ascii_lowercase();
        if scheme.is_empty() || !scheme.chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c)) {
            return None;
        }
        let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let (authority, tail) = rest.split_at(end);
        let tail = tail.split('#').next().unwrap_or("");
        let target = match tail {
            "" => "/".to_string(),
            t if t.starts_with('?') => format!("/{t}"),
            t => t.to_string(),
        };
        let (userinfo, hostport) = match authority.rfind('@') {
            Some(i) => (Some(&authority[..i]), &authority[i + 1..]),
            None => (None, authority),
        };
        let (user, pass) = match userinfo {
            Some(u) => match u.split_once(':') {
                Some((a, b)) => (Some(pct_decode(a)), Some(pct_decode(b))),
                None => (Some(pct_decode(u)), None),
            },
            None => (None, None),
        };
        let (host, port) = if let Some(r) = hostport.strip_prefix('[') {
            let (h, after) = r.split_once(']')?;
            let port = match after {
                "" => None,
                p => Some(p.strip_prefix(':')?),
            };
            (h.to_string(), port)
        } else {
            match hostport.rsplit_once(':') {
                Some((h, p)) => (h.to_string(), Some(p)),
                None => (hostport.to_string(), None),
            }
        };
        if host.is_empty() {
            return None;
        }
        let port = match port {
            None | Some("") => default_port(&scheme)?,
            Some(p) => p.parse().ok()?,
        };
        Some(Url { scheme, user, pass, host, port, target })
    }

    /// host[:port] for Host headers and CONNECT; the port is left out when it
    /// is the scheme's default, unless `always_port`.
    pub fn authority(&self, always_port: bool) -> String {
        let h = if self.host.contains(':') { format!("[{}]", self.host) } else { self.host.clone() };
        if always_port || Some(self.port) != default_port(&self.scheme) {
            format!("{h}:{}", self.port)
        } else {
            h
        }
    }

    /// Resolves a redirect Location against this URL.
    fn join(&self, loc: &str) -> Option<Url> {
        let loc = loc.trim();
        if loc.contains("://") {
            return Url::parse(loc);
        }
        if let Some(r) = loc.strip_prefix("//") {
            return Url::parse(&format!("{}://{}", self.scheme, r));
        }
        let mut u = self.clone();
        u.user = None;
        u.pass = None;
        if loc.starts_with('/') {
            u.target = loc.to_string();
        } else {
            let path = self.target.split('?').next().unwrap_or("/");
            let dir = &path[..path.rfind('/').map(|i| i + 1).unwrap_or(0)];
            u.target = format!("{}{}", if dir.is_empty() { "/" } else { dir }, loc);
        }
        Some(u)
    }
}

/// True for an absolute http(s) URL with a host.
pub fn is_stream_url(s: &str) -> bool {
    Url::parse(s).is_some_and(|u| u.scheme == "http" || u.scheme == "https")
}

pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

// ---------------------------------------------------------------- proxies

#[derive(Clone, Debug, PartialEq)]
enum Kind {
    Socks5,
    Http,
    Https,
}

#[derive(Clone, Debug)]
pub struct Proxy {
    kind: Kind,
    url: Url,
}

impl Proxy {
    /// Parses a proxy URL. Errors never echo the URL, which may hold a password.
    pub fn parse(raw: &str) -> Result<Proxy, String> {
        let raw = raw.trim();
        let scheme = raw.split_once("://").map(|(s, _)| s.to_ascii_lowercase()).unwrap_or_default();
        let kind = match scheme.as_str() {
            "socks5" | "socks5h" => Kind::Socks5,
            "http" => Kind::Http,
            "https" => Kind::Https,
            _ => return Err("proxy must start with socks5://, socks5h://, http:// or https://".into()),
        };
        let rest = &raw[scheme.len() + 3..];
        let authority = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
        let hostport = authority.rsplit('@').next().unwrap_or("");
        if hostport.is_empty() || hostport.starts_with(':') {
            return Err("proxy needs a host, e.g. socks5://192.168.1.10:1080".into());
        }
        let url = Url::parse(raw).ok_or("proxy URL is not valid")?;
        Ok(Proxy { kind, url })
    }
}

async fn tcp(host: &str, port: u16) -> io::Result<TcpStream> {
    let s = TcpStream::connect((host, port)).await?;
    let _ = s.set_nodelay(true);
    Ok(s)
}

fn tls_config() -> Arc<rustls::ClientConfig> {
    static CFG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    CFG.get_or_init(|| {
        let mut roots = rustls::RootCertStore::empty();
        let files = [
            std::env::var("SSL_CERT_FILE").unwrap_or_default(),
            "/etc/ssl/certs/ca-certificates.crt".into(),
            "/etc/ssl/cert.pem".into(),
            "/etc/pki/tls/certs/ca-bundle.crt".into(),
        ];
        for f in files.iter().filter(|f| !f.is_empty()) {
            if let Ok(it) = CertificateDer::pem_file_iter(f) {
                if roots.add_parsable_certificates(it.flatten()).0 > 0 {
                    break;
                }
            }
        }
        // Windows has no CA file: use the bundled Mozilla roots.
        #[cfg(windows)]
        if roots.is_empty() {
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        }
        if roots.is_empty() {
            eprintln!("No CA certificates found; https stations will fail (install ca-bundle / ca-certificates)");
        }
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        Arc::new(
            rustls::ClientConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .expect("TLS setup")
                .with_root_certificates(roots)
                .with_no_client_auth(),
        )
    })
    .clone()
}

async fn tls(conn: BoxConn, host: &str) -> io::Result<BoxConn> {
    let name = ServerName::try_from(host.to_string()).map_err(|_| err(format!("invalid TLS host name {host}")))?;
    let s = tokio_rustls::TlsConnector::from(tls_config()).connect(name, conn).await?;
    Ok(Box::new(s))
}

fn socks_error(code: u8) -> &'static str {
    match code {
        1 => "general failure",
        2 => "not allowed by the proxy's rules",
        3 => "network unreachable",
        4 => "host unreachable",
        5 => "connection refused",
        6 => "TTL expired",
        7 => "command not supported",
        8 => "address type not supported",
        _ => "unknown error",
    }
}

/// Connects through a SOCKS5 proxy. Host names are resolved by the proxy
/// (for socks5:// as well as socks5h://).
async fn socks5(p: &Url, host: &str, port: u16) -> io::Result<BoxConn> {
    let mut s = tcp(&p.host, p.port).await?;
    let auth = p.user.is_some();
    s.write_all(if auth { &[5, 2, 0, 2] } else { &[5, 1, 0] }).await?;
    let mut r = [0u8; 2];
    s.read_exact(&mut r).await?;
    if r[0] != 5 {
        return Err(err("proxy is not a SOCKS5 server"));
    }
    match r[1] {
        0 => {}
        2 if auth => {
            let u = p.user.as_deref().unwrap_or("");
            let pw = p.pass.as_deref().unwrap_or("");
            if u.len() > 255 || pw.len() > 255 {
                return Err(err("proxy username or password is too long"));
            }
            let mut m = vec![1, u.len() as u8];
            m.extend(u.as_bytes());
            m.push(pw.len() as u8);
            m.extend(pw.as_bytes());
            s.write_all(&m).await?;
            s.read_exact(&mut r).await?;
            if r[1] != 0 {
                return Err(err("proxy rejected the username or password"));
            }
        }
        _ => return Err(err("proxy requires a username and password")),
    }
    let mut m = vec![5, 1, 0];
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(a)) => {
            m.push(1);
            m.extend(a.octets());
        }
        Ok(IpAddr::V6(a)) => {
            m.push(4);
            m.extend(a.octets());
        }
        Err(_) => {
            if host.len() > 255 {
                return Err(err("host name too long"));
            }
            m.push(3);
            m.push(host.len() as u8);
            m.extend(host.as_bytes());
        }
    }
    m.extend(port.to_be_bytes());
    s.write_all(&m).await?;
    let mut h = [0u8; 4];
    s.read_exact(&mut h).await?;
    if h[1] != 0 {
        return Err(err(format!("proxy could not connect: {}", socks_error(h[1]))));
    }
    let addr_len = match h[3] {
        1 => 4,
        4 => 16,
        3 => {
            let mut l = [0u8];
            s.read_exact(&mut l).await?;
            l[0] as usize
        }
        _ => return Err(err("bad SOCKS5 reply")),
    };
    let mut rest = vec![0u8; addr_len + 2];
    s.read_exact(&mut rest).await?;
    Ok(Box::new(s))
}

fn proxy_auth(p: &Url) -> String {
    match &p.user {
        Some(u) => format!(
            "Proxy-Authorization: Basic {}\r\n",
            base64(format!("{u}:{}", p.pass.as_deref().unwrap_or("")).as_bytes())
        ),
        None => String::new(),
    }
}

/// Opens a connection for `u`. Returns the connection, whether the request
/// must use the absolute URL (plain HTTP proxy), and extra request headers.
async fn open(u: &Url, proxy: Option<&Proxy>) -> io::Result<(BoxConn, bool, String)> {
    let https = u.scheme == "https";
    let conn: BoxConn = match proxy {
        None => Box::new(tcp(&u.host, u.port).await?),
        Some(p) if p.kind == Kind::Socks5 => socks5(&p.url, &u.host, u.port).await?,
        Some(p) => {
            let raw: BoxConn = Box::new(tcp(&p.url.host, p.url.port).await?);
            let mut c = if p.kind == Kind::Https { tls(raw, &p.url.host).await? } else { raw };
            if !https {
                return Ok((c, true, proxy_auth(&p.url)));
            }
            let a = u.authority(true);
            c.write_all(format!("CONNECT {a} HTTP/1.1\r\nHost: {a}\r\n{}\r\n", proxy_auth(&p.url)).as_bytes())
                .await?;
            // Read the reply byte by byte so nothing of the tunnel is consumed.
            let mut head = Vec::new();
            let mut b = [0u8];
            while !head.ends_with(b"\r\n\r\n") && !head.ends_with(b"\n\n") {
                if c.read(&mut b).await? == 0 || head.len() > 16384 {
                    return Err(err("proxy closed the connection"));
                }
                head.push(b[0]);
            }
            let line = String::from_utf8_lossy(&head);
            let code = line.split_whitespace().nth(1).unwrap_or("");
            if !code.starts_with('2') {
                let status = line.lines().next().unwrap_or("").trim();
                return Err(err(format!("proxy refused the connection: {status}")));
            }
            c
        }
    };
    let conn = if https { tls(conn, &u.host).await? } else { conn };
    Ok((conn, false, String::new()))
}

// ---------------------------------------------------------------- responses

enum Mode {
    Eof,
    Len(u64),
    Chunked { left: u64, started: bool, done: bool },
}

pub struct Body {
    r: BufReader<BoxConn>,
    mode: Mode,
}

impl Body {
    /// Reads decoded body bytes; 0 means the end of the body.
    pub async fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let res = self.read_inner(buf).await;
        match res {
            // Many stream servers close TLS connections without close_notify.
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(0),
            r => r,
        }
    }

    async fn read_inner(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let Body { r, mode } = self;
        match mode {
            Mode::Eof => r.read(buf).await,
            Mode::Len(n) => {
                if *n == 0 {
                    return Ok(0);
                }
                let m = buf.len().min(*n as usize);
                let k = r.read(&mut buf[..m]).await?;
                *n -= k as u64;
                Ok(k)
            }
            Mode::Chunked { left, started, done } => {
                if *done {
                    return Ok(0);
                }
                if *left == 0 {
                    let mut line = Vec::new();
                    if *started {
                        r.read_until(b'\n', &mut line).await?; // CRLF after the previous chunk
                        line.clear();
                    }
                    *started = true;
                    if r.read_until(b'\n', &mut line).await? == 0 {
                        return Ok(0);
                    }
                    let text = String::from_utf8_lossy(&line);
                    let size = text.split(';').next().unwrap_or("").trim();
                    let size = u64::from_str_radix(size, 16).map_err(|_| err("bad chunked encoding"))?;
                    if size == 0 {
                        *done = true;
                        return Ok(0);
                    }
                    *left = size;
                }
                let m = buf.len().min(*left as usize);
                let k = r.read(&mut buf[..m]).await?;
                *left -= k as u64;
                Ok(k)
            }
        }
    }

    pub async fn read_full(&mut self, buf: &mut [u8]) -> io::Result<()> {
        let mut got = 0;
        while got < buf.len() {
            let k = self.read(&mut buf[got..]).await?;
            if k == 0 {
                return Err(err("stream ended prematurely"));
            }
            got += k;
        }
        Ok(())
    }

    pub async fn skip(&mut self, mut n: usize) -> io::Result<()> {
        let mut buf = [0u8; 4096];
        while n > 0 {
            let k = self.read(&mut buf[..n.min(4096)]).await?;
            if k == 0 {
                return Err(err("stream ended prematurely"));
            }
            n -= k;
        }
        Ok(())
    }
}

pub struct Response {
    pub status: u16,
    /// e.g. "200 OK"
    pub status_text: String,
    headers: Vec<(String, String)>,
    pub body: Body,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

async fn read_line(r: &mut BufReader<BoxConn>) -> io::Result<String> {
    let mut line = Vec::new();
    (&mut *r).take(8192).read_until(b'\n', &mut line).await?;
    if line.is_empty() {
        return Err(err("connection closed before the response"));
    }
    Ok(String::from_utf8_lossy(&line).trim_end_matches(['\r', '\n']).to_string())
}

async fn get_once(u: &Url, proxy: Option<&Proxy>, extra: &[(&str, &str)]) -> io::Result<Response> {
    let (mut c, absolute, more) = open(u, proxy).await?;
    let host = u.authority(false);
    let target = if absolute { format!("http://{host}{}", u.target) } else { u.target.clone() };
    let mut req = format!(
        "GET {target} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: {USER_AGENT}\r\nAccept: */*\r\nConnection: close\r\n{more}"
    );
    if let Some(user) = &u.user {
        let cred = format!("{user}:{}", u.pass.as_deref().unwrap_or(""));
        req += &format!("Authorization: Basic {}\r\n", base64(cred.as_bytes()));
    }
    for (k, v) in extra {
        req += &format!("{k}: {v}\r\n");
    }
    req += "\r\n";
    c.write_all(req.as_bytes()).await?;
    c.flush().await?;

    let mut r = BufReader::with_capacity(16 * 1024, c);
    let line = read_line(&mut r).await?;
    let (proto, rest) = line.split_once(' ').unwrap_or((&line, ""));
    if !(proto.starts_with("HTTP/") || proto == "ICY") {
        return Err(err("server did not answer with HTTP"));
    }
    let status_text = rest.trim().to_string();
    let status: u16 = status_text
        .split(' ')
        .next()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| err("bad status line"))?;
    let mut headers = Vec::new();
    loop {
        let h = read_line(&mut r).await?;
        if h.is_empty() {
            break;
        }
        if headers.len() > 200 {
            return Err(err("too many response headers"));
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    let mut resp = Response { status, status_text, headers, body: Body { r, mode: Mode::Eof } };
    resp.body.mode = if resp.header("transfer-encoding").is_some_and(|v| v.to_ascii_lowercase().contains("chunked")) {
        Mode::Chunked { left: 0, started: false, done: false }
    } else if let Some(n) = resp.header("content-length").and_then(|v| v.parse().ok()) {
        Mode::Len(n)
    } else if status == 204 || status == 304 {
        Mode::Len(0)
    } else {
        Mode::Eof
    };
    Ok(resp)
}

/// GET with up to 10 redirects.
pub async fn get(url: &str, proxy: Option<&Proxy>, extra: &[(&str, &str)]) -> io::Result<Response> {
    let mut u = Url::parse(url)
        .filter(|u| u.scheme == "http" || u.scheme == "https")
        .ok_or_else(|| err("URL must start with http:// or https://"))?;
    for _ in 0..10 {
        let resp = get_once(&u, proxy, extra).await?;
        if matches!(resp.status, 301 | 302 | 303 | 307 | 308) {
            if let Some(next) = resp.header("location").and_then(|l| u.join(l)) {
                if next.scheme != "http" && next.scheme != "https" {
                    return Err(err("redirect to an unsupported URL"));
                }
                u = next;
                continue;
            }
        }
        return Ok(resp);
    }
    Err(err("too many redirects"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        let u = Url::parse("http://s39.myradiostream.com:11590/listen/;.mp3").unwrap();
        assert_eq!((u.host.as_str(), u.port, u.target.as_str()), ("s39.myradiostream.com", 11590, "/listen/;.mp3"));
        let u = Url::parse("https://a.b?x=1").unwrap();
        assert_eq!((u.port, u.target.as_str(), u.authority(false).as_str()), (443, "/?x=1", "a.b"));
        let u = Url::parse("socks5h://us%40r:p%3Ass@192.168.1.10").unwrap();
        assert_eq!((u.user.as_deref(), u.pass.as_deref(), u.port), (Some("us@r"), Some("p:ss"), 1080));
        let u = Url::parse("http://[::1]:8000/x").unwrap();
        assert_eq!(u.authority(true), "[::1]:8000");
        assert!(Url::parse("http://").is_none());
        assert!(Url::parse("nonsense").is_none());
        let j = u.join("y/z").unwrap();
        assert_eq!(j.target, "/y/z");
        assert_eq!(Url::parse("http://h/a/b").unwrap().join("c").unwrap().target, "/a/c");
        assert_eq!(Url::parse("http://h/a").unwrap().join("//o:1/p").unwrap().authority(false), "o:1");
    }

    #[test]
    fn proxies() {
        assert!(Proxy::parse("socks5h://guest:pw@192.168.1.10").is_ok());
        assert_eq!(Proxy::parse("socks5h://guest:pw@").unwrap_err(), "proxy needs a host, e.g. socks5://192.168.1.10:1080");
        assert!(Proxy::parse("ftp://x").unwrap_err().starts_with("proxy must start"));
        assert!(Proxy::parse("192.168.1.10:1080").unwrap_err().starts_with("proxy must start"));
    }

    #[test]
    fn b64() {
        assert_eq!(base64(b"admin:pw"), "YWRtaW46cHc=");
        assert_eq!(base64(b"ab"), "YWI=");
        assert_eq!(base64(b"abc"), "YWJj");
    }
}
