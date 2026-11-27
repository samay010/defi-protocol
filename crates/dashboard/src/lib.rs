//! A read-only dashboard server for the local machine.
//!
//! Security model (the whole point of this crate):
//! - It binds `127.0.0.1` only, never a public interface, and refuses any peer that is not a
//!   loopback address.
//! - Every request needs the random per-run token (`?t=...`), compared in constant time. Without it
//!   a web page you happen to open in a browser cannot read your data, even if it guesses the port.
//! - The `Host` header must be exactly `127.0.0.1:<port>` or `localhost:<port>`, which defeats DNS
//!   rebinding.
//! - Only `GET`. Nothing here can change anything: the [`Data`] provider only has read methods.
//! - No CORS headers are ever sent, so other origins cannot read responses; `Cache-Control:
//!   no-store`, `Referrer-Policy: no-referrer`, a strict `Content-Security-Policy`, `nosniff` and
//!   `X-Frame-Options: DENY` are set on every response.
//! - Requests are limited in size (8 KiB header) and time (a read timeout), one at a time.
//!
//! It is plain HTTP on loopback: anyone who can already run code as you on this machine (and read
//! the terminal where the link was printed) can use the link. That is the intended trust boundary.

pub mod html;
pub mod json;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

/// Where the data comes from. Read-only by construction: there is no method that changes state.
pub trait Data {
    /// JSON for `/api/overview`.
    fn overview(&self) -> String;
    /// JSON for `/api/instance/<name>`; `None` when there is no such instance.
    fn instance(&self, name: &str) -> Option<String>;
}

pub struct Config {
    /// The secret every request must carry.
    pub token: String,
    /// The port the server is bound to (needed to check the `Host` header).
    pub port: u16,
    /// How long to wait for a client to send its request.
    pub read_timeout: Duration,
}

pub struct Response {
    pub status: u16,
    pub content_type: &'static str,
    pub body: Vec<u8>,
    pub allow: Option<&'static str>,
}

fn text(status: u16, msg: &str) -> Response {
    Response {
        status,
        content_type: "text/plain; charset=utf-8",
        body: format!("{msg}\n").into_bytes(),
        allow: None,
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        421 => "Misdirected Request",
        431 => "Request Header Fields Too Large",
        _ => "Error",
    }
}

/// Constant-time equality for secrets of equal or different length.
pub fn tokens_equal(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut diff = a.len() ^ b.len();
    for i in 0..a.len().max(b.len()) {
        diff |= usize::from(a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0));
    }
    diff == 0
}

/// A request head split into what the server uses.
pub struct Head<'a> {
    pub method: &'a str,
    pub target: &'a str,
    pub host: Option<&'a str>,
}

/// Parse the request line and the `Host` header. Errors are HTTP status codes.
pub fn parse_head(head: &str) -> Result<Head<'_>, u16> {
    let mut lines = head.split("\r\n");
    let first = lines.next().ok_or(400u16)?;
    let mut parts = first.split(' ');
    let (method, target, version) = (
        parts.next().ok_or(400u16)?,
        parts.next().ok_or(400u16)?,
        parts.next().ok_or(400u16)?,
    );
    if parts.next().is_some() || !version.starts_with("HTTP/1.") || method.is_empty() {
        return Err(400);
    }
    if !target.starts_with('/') || target.chars().any(|c| c.is_control() || c == ' ') {
        return Err(400);
    }
    let mut host = None;
    for l in lines {
        if l.is_empty() {
            break;
        }
        let Some((k, v)) = l.split_once(':') else {
            return Err(400);
        };
        if k.eq_ignore_ascii_case("host") {
            if host.is_some() {
                return Err(400); // two Host headers are a smuggling signal
            }
            host = Some(v.trim());
        }
    }
    Ok(Head {
        method,
        target,
        host,
    })
}

fn host_ok(host: Option<&str>, port: u16) -> bool {
    let Some(h) = host else { return false };
    let h = h.to_ascii_lowercase();
    h == format!("127.0.0.1:{port}") || h == format!("localhost:{port}")
}

fn query_token(query: &str) -> Option<&str> {
    query.split('&').find_map(|kv| kv.strip_prefix("t="))
}

fn valid_instance_name(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 64
        && n.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Route one request. Pure: no sockets, so every rule is testable.
pub fn handle(head: &Head<'_>, cfg: &Config, data: &dyn Data) -> Response {
    if !host_ok(head.host, cfg.port) {
        return text(421, "unexpected Host header");
    }
    if head.method != "GET" {
        let mut r = text(405, "this dashboard is read-only: GET only");
        r.allow = Some("GET");
        return r;
    }
    let (path, query) = head.target.split_once('?').unwrap_or((head.target, ""));
    match query_token(query) {
        Some(t) if tokens_equal(t, &cfg.token) => {}
        _ => {
            return text(
                403,
                "missing or wrong token: open the link printed by `protocol dashboard`",
            )
        }
    }
    if path.contains('%') || path.contains("..") || path.contains('\\') {
        return text(404, "not found");
    }
    match path {
        "/" => Response {
            status: 200,
            content_type: "text/html; charset=utf-8",
            body: html::PAGE.as_bytes().to_vec(),
            allow: None,
        },
        "/api/overview" => json_response(data.overview()),
        p => {
            let Some(name) = p.strip_prefix("/api/instance/") else {
                return text(404, "not found");
            };
            if !valid_instance_name(name) {
                return text(400, "invalid instance name");
            }
            match data.instance(name) {
                Some(j) => json_response(j),
                None => text(404, "no such instance"),
            }
        }
    }
}

fn json_response(body: String) -> Response {
    Response {
        status: 200,
        content_type: "application/json; charset=utf-8",
        body: body.into_bytes(),
        allow: None,
    }
}

/// The bytes of a full HTTP response, security headers included.
pub fn render(r: &Response) -> Vec<u8> {
    let mut h = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\
         Cache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\n\
         X-Frame-Options: DENY\r\nCross-Origin-Resource-Policy: same-origin\r\n\
         Content-Security-Policy: default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'\r\n",
        r.status,
        reason(r.status),
        r.content_type,
        r.body.len()
    );
    if let Some(a) = r.allow {
        h.push_str(&format!("Allow: {a}\r\n"));
    }
    h.push_str("\r\n");
    let mut out = h.into_bytes();
    out.extend_from_slice(&r.body);
    out
}

/// Bind the loopback interface only. Port `0` picks a free port.
pub fn bind(port: u16) -> std::io::Result<TcpListener> {
    TcpListener::bind(("127.0.0.1", port))
}

const MAX_HEAD: usize = 8 * 1024;

fn read_head(stream: &mut TcpStream) -> Result<String, u16> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if buf.len() > MAX_HEAD {
            return Err(431);
        }
        match stream.read(&mut chunk) {
            Ok(0) => return Err(400),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(_) => return Err(400), // includes the read timeout
        }
    }
    let end = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .unwrap_or(buf.len());
    if end > MAX_HEAD {
        return Err(431);
    }
    String::from_utf8(buf[..end + 2].to_vec()).map_err(|_| 400)
}

fn serve_one(mut stream: TcpStream, cfg: &Config, data: &dyn Data) {
    let _ = stream.set_read_timeout(Some(cfg.read_timeout));
    let _ = stream.set_write_timeout(Some(cfg.read_timeout));
    let allowed = stream
        .peer_addr()
        .map(|a| a.ip().is_loopback())
        .unwrap_or(false);
    let resp = if !allowed {
        text(403, "loopback clients only")
    } else {
        match read_head(&mut stream) {
            Err(code) => text(code, "bad request"),
            Ok(raw) => match parse_head(&raw) {
                Err(code) => text(code, "bad request"),
                Ok(head) => handle(&head, cfg, data),
            },
        }
    };
    let _ = stream.write_all(&render(&resp));
    let _ = stream.flush();
}

/// Serve requests one at a time. Stops after `max_requests` when given (tests); otherwise runs
/// until the process is stopped.
pub fn serve(
    listener: TcpListener,
    cfg: &Config,
    data: &dyn Data,
    max_requests: Option<usize>,
) -> std::io::Result<()> {
    let mut served = 0usize;
    for stream in listener.incoming() {
        match stream {
            Ok(s) => serve_one(s, cfg, data),
            Err(_) => continue,
        }
        served += 1;
        if max_requests.is_some_and(|m| served >= m) {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake;
    impl Data for Fake {
        fn overview(&self) -> String {
            json::Obj::new().bool("kill_switch", false).finish()
        }
        fn instance(&self, name: &str) -> Option<String> {
            (name == "alpha").then(|| json::Obj::new().str("name", name).finish())
        }
    }

    fn cfg() -> Config {
        Config {
            token: "s3cret".into(),
            port: 8787,
            read_timeout: Duration::from_millis(300),
        }
    }

    fn get(target: &str, host: &str) -> Response {
        let raw = format!("GET {target} HTTP/1.1\r\nHost: {host}\r\n\r\n");
        let head = parse_head(&raw).unwrap();
        handle(&head, &cfg(), &Fake)
    }

    const H: &str = "127.0.0.1:8787";

    #[test]
    fn a_valid_request_gets_the_page_and_the_data() {
        let r = get("/?t=s3cret", H);
        assert_eq!(r.status, 200);
        assert!(r.content_type.starts_with("text/html"));
        assert!(String::from_utf8_lossy(&r.body).contains("read-only"));
        let r = get("/api/overview?t=s3cret", "localhost:8787");
        assert_eq!(r.status, 200);
        assert!(json::is_valid(&String::from_utf8_lossy(&r.body)));
        let r = get("/api/instance/alpha?t=s3cret", H);
        assert_eq!(r.status, 200);
        assert!(String::from_utf8_lossy(&r.body).contains("alpha"));
    }

    #[test]
    fn the_token_is_required_everywhere() {
        for t in ["/", "/api/overview", "/api/instance/alpha"] {
            assert_eq!(get(t, H).status, 403, "{t} without token");
            assert_eq!(get(&format!("{t}?t=wrong"), H).status, 403);
            assert_eq!(get(&format!("{t}?t="), H).status, 403);
            assert_eq!(get(&format!("{t}?x=s3cret"), H).status, 403);
        }
        assert!(tokens_equal("abc", "abc"));
        assert!(
            !tokens_equal("abc", "abd") && !tokens_equal("abc", "abcd") && !tokens_equal("", "a")
        );
    }

    #[test]
    fn a_foreign_host_header_is_refused_even_with_the_token() {
        for h in [
            "evil.example:8787",
            "127.0.0.1",
            "127.0.0.1:9999",
            "localhost",
            "evil.example",
            "",
        ] {
            assert_eq!(get("/api/overview?t=s3cret", h).status, 421, "{h}");
        }
        let raw = "GET /api/overview?t=s3cret HTTP/1.1\r\n\r\n";
        assert_eq!(handle(&parse_head(raw).unwrap(), &cfg(), &Fake).status, 421);
        assert_eq!(get("/api/overview?t=s3cret", "LOCALHOST:8787").status, 200);
    }

    #[test]
    fn only_get_is_allowed() {
        for m in ["POST", "PUT", "DELETE", "PATCH", "OPTIONS", "HEAD"] {
            let raw = format!("{m} /api/overview?t=s3cret HTTP/1.1\r\nHost: {H}\r\n\r\n");
            let r = handle(&parse_head(&raw).unwrap(), &cfg(), &Fake);
            assert_eq!(r.status, 405, "{m}");
            assert_eq!(r.allow, Some("GET"));
        }
    }

    #[test]
    fn paths_are_strict() {
        assert_eq!(get("/nope?t=s3cret", H).status, 404);
        assert_eq!(get("/api/instance/?t=s3cret", H).status, 400);
        assert_eq!(get("/api/instance/ghost?t=s3cret", H).status, 404);
        assert_eq!(get("/api/instance/a%2fb?t=s3cret", H).status, 404);
        assert_eq!(get("/api/instance/../overview?t=s3cret", H).status, 404);
        assert_eq!(get("/api/instance/a.b?t=s3cret", H).status, 400);
        assert_eq!(
            get(&format!("/api/instance/{}?t=s3cret", "a".repeat(65)), H).status,
            400
        );
        assert_eq!(get("/api/instance/alpha/extra?t=s3cret", H).status, 400);
    }

    #[test]
    fn malformed_heads_are_rejected() {
        for raw in [
            "",
            "GET\r\n\r\n",
            "GET / HTTP/2.0\r\n\r\n",
            "GET / HTTP/1.1 extra\r\n\r\n",
            "GET nothing HTTP/1.1\r\n\r\n",
            "GET /a b HTTP/1.1\r\n\r\n",
            "GET / HTTP/1.1\r\nbroken header\r\n\r\n",
            "GET / HTTP/1.1\r\nHost: a\r\nHost: b\r\n\r\n",
        ] {
            assert!(parse_head(raw).is_err(), "{raw:?}");
        }
        assert!(parse_head("GET / HTTP/1.1\r\nHost: x\r\n\r\n").is_ok());
        assert!(parse_head("GET / HTTP/1.0\r\n\r\n").is_ok());
    }

    #[test]
    fn every_response_carries_the_security_headers_and_no_cors() {
        let text = String::from_utf8(render(&get("/api/overview?t=s3cret", H))).unwrap();
        for needle in [
            "HTTP/1.1 200 OK",
            "Cache-Control: no-store",
            "Referrer-Policy: no-referrer",
            "X-Content-Type-Options: nosniff",
            "X-Frame-Options: DENY",
            "Content-Security-Policy: default-src 'none'",
            "Cross-Origin-Resource-Policy: same-origin",
        ] {
            assert!(text.contains(needle), "{needle}");
        }
        assert!(!text.to_ascii_lowercase().contains("access-control-allow"));
        let bad = String::from_utf8(render(&get("/", H))).unwrap();
        assert!(bad.starts_with("HTTP/1.1 403") && bad.contains("nosniff"));
    }

    #[test]
    fn the_page_never_uses_innerhtml_or_external_resources() {
        let p = html::PAGE;
        assert!(
            !p.contains("innerHTML") && !p.contains("outerHTML") && !p.contains("document.write")
        );
        assert!(!p.contains("http://") && !p.contains("https://") && !p.contains("src=\""));
        assert!(!p.contains("eval(") && !p.contains("<form"));
    }

    fn exchange(port: u16, request: &[u8]) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        s.write_all(request).unwrap();
        let mut out = Vec::new();
        let _ = s.read_to_end(&mut out);
        String::from_utf8_lossy(&out).to_string()
    }

    #[test]
    fn the_server_answers_over_a_real_loopback_socket() {
        let listener = bind(0).unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(listener.local_addr().unwrap().ip().is_loopback());
        let handle = std::thread::spawn(move || {
            let c = Config {
                token: "tok".into(),
                port,
                read_timeout: Duration::from_millis(300),
            };
            serve(listener, &c, &Fake, Some(6)).unwrap();
        });
        let ok = exchange(
            port,
            format!("GET /api/overview?t=tok HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n")
                .as_bytes(),
        );
        assert!(ok.starts_with("HTTP/1.1 200 OK"), "{ok}");
        assert!(ok.contains("\"kill_switch\":false"));
        let no_token = exchange(
            port,
            format!("GET /api/overview HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n").as_bytes(),
        );
        assert!(no_token.starts_with("HTTP/1.1 403"), "{no_token}");
        let rebind = exchange(
            port,
            b"GET /api/overview?t=tok HTTP/1.1\r\nHost: attacker.example\r\n\r\n",
        );
        assert!(rebind.starts_with("HTTP/1.1 421"), "{rebind}");
        let post = exchange(
            port,
            format!("POST /api/overview?t=tok HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n")
                .as_bytes(),
        );
        assert!(
            post.starts_with("HTTP/1.1 405") && post.contains("Allow: GET"),
            "{post}"
        );
        // an oversized header is cut off
        let mut big = format!("GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nX: ").into_bytes();
        big.extend(std::iter::repeat_n(b'a', 20_000));
        let r = exchange(port, &big);
        assert!(r.starts_with("HTTP/1.1 431"), "{}", &r[..r.len().min(60)]);
        // a client that sends half a request and stalls is dropped after the timeout
        let started = std::time::Instant::now();
        let stalled = exchange(port, b"GET /api/over");
        assert!(stalled.starts_with("HTTP/1.1 400"), "{stalled}");
        assert!(started.elapsed() < Duration::from_secs(4));
        handle.join().unwrap();
    }
}
