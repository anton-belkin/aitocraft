//! A small HTTP/1.1 client for the apps' MCP servers on this machine: kept-alive connections for
//! MCP calls ([`Pool`]), one connection per stream ([`request`]), bounded heads and bodies, chunked
//! bodies, server-sent events.
//! It refuses anything that isn't loopback, so a registry file can't send calls elsewhere.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::time::Duration;

/// Largest response head read.
pub const MAX_HEAD: usize = 64 * 1024;
/// Largest response body read (a stream ends there and is reconnected).
pub const MAX_BODY: usize = 32 * 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_millis(1500);

/// An `http://` URL on this machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Url {
    pub host: String,
    pub port: u16,
    /// Path and query, starting with `/`.
    pub path: String,
}

impl Url {
    pub fn parse(s: &str) -> Result<Url, String> {
        let rest = s.trim().strip_prefix("http://").ok_or_else(|| format!("{s}: only http:// URLs on this machine are supported"))?;
        let (auth, path) = match rest.find(['/', '?']) {
            Some(i) if rest[i..].starts_with('/') => (&rest[..i], rest[i..].to_string()),
            Some(i) => (&rest[..i], format!("/{}", &rest[i..])),
            None => (rest, "/".to_string()),
        };
        let (host, port) = if let Some(h) = auth.strip_prefix('[') {
            let (h, p) = h.split_once(']').ok_or_else(|| format!("{s}: bad host"))?;
            (format!("[{h}]"), p.strip_prefix(':'))
        } else {
            match auth.rsplit_once(':') {
                Some((h, p)) => (h.to_string(), Some(p)),
                None => (auth.to_string(), None),
            }
        };
        let port = match port {
            Some(p) => p.parse::<u16>().map_err(|_| format!("{s}: bad port"))?,
            None => 80,
        };
        let lower = host.to_ascii_lowercase();
        if !matches!(lower.as_str(), "127.0.0.1" | "localhost" | "[::1]") {
            return Err(format!("{s}: not on this machine; AItoCraft only talks to apps on 127.0.0.1"));
        }
        Ok(Url { host: lower, port, path })
    }

    /// The same server with another path (and query).
    pub fn with_path(&self, path: &str) -> Url {
        Url { host: self.host.clone(), port: self.port, path: path.to_string() }
    }

    pub fn authority(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

impl std::fmt::Display for Url {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "http://{}{}", self.authority(), self.path)
    }
}

/// Status and headers of a response.
#[derive(Debug)]
pub struct Head {
    pub status: u16,
    pub headers: Vec<(String, String)>,
}

impl Head {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

/// A response body, read as it comes.
pub type Body = Box<dyn BufRead + Send>;

fn connect(url: &Url) -> Result<TcpStream, String> {
    let host = url.host.trim_start_matches('[').trim_end_matches(']');
    let addrs = (host, url.port).to_socket_addrs().map_err(|e| e.to_string())?;
    let mut last = "no address".to_string();
    for a in addrs {
        match TcpStream::connect_timeout(&a, CONNECT_TIMEOUT) {
            Ok(s) => return Ok(s),
            Err(e) => last = e.to_string(),
        }
    }
    Err(last)
}

/// Sends one request and reads the response head. Returns the head, the body as a reader, and the
/// socket (shut it down to stop a stream from another thread). `timeout` bounds each read.
pub fn request(
    method: &str,
    url: &Url,
    headers: &[(&str, String)],
    body: Option<&[u8]>,
    timeout: Duration,
) -> Result<(Head, Body, TcpStream), String> {
    let stream = connect(url)?;
    let _ = stream.set_nodelay(true);
    stream.set_read_timeout(Some(timeout)).map_err(|e| e.to_string())?;
    stream.set_write_timeout(Some(Duration::from_secs(10))).map_err(|e| e.to_string())?;
    let mut req = format!(
        "{method} {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nUser-Agent: aitocraft/{}\r\n",
        url.path,
        url.authority(),
        env!("CARGO_PKG_VERSION")
    );
    for (k, v) in headers {
        if v.contains(['\r', '\n']) || k.contains(['\r', '\n', ':']) {
            continue;
        }
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    if let Some(b) = body {
        req.push_str(&format!("Content-Length: {}\r\n", b.len()));
    }
    req.push_str("\r\n");
    let mut w = stream.try_clone().map_err(|e| e.to_string())?;
    w.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
    if let Some(b) = body {
        w.write_all(b).map_err(|e| e.to_string())?;
    }
    w.flush().map_err(|e| e.to_string())?;
    let mut r = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
    let head = loop {
        let h = read_head(&mut r)?;
        // Informational responses (100 Continue) come before the real one.
        if !(100..200).contains(&h.status) {
            break h;
        }
    };
    let chunked = head.header("transfer-encoding").is_some_and(|t| t.to_ascii_lowercase().contains("chunked"));
    let body: Body = if method == "HEAD" || head.status == 204 || head.status == 304 {
        Box::new(std::io::empty())
    } else if chunked {
        Box::new(BufReader::new(Chunked { inner: r, left: 0, after_chunk: false, done: false, total: 0 }))
    } else if let Some(n) = head.header("content-length").and_then(|v| v.trim().parse::<u64>().ok()) {
        if n > MAX_BODY as u64 {
            return Err(format!("response of {n} bytes is too large"));
        }
        Box::new(r.take(n))
    } else {
        Box::new(r.take(MAX_BODY as u64))
    };
    Ok((head, body, stream))
}

/// Idle keep-alive connections to one server, reused by later requests.
#[derive(Default)]
pub struct Pool {
    idle: std::sync::Mutex<Vec<(BufReader<TcpStream>, TcpStream)>>,
}

const MAX_IDLE: usize = 8;

enum Sent {
    Done(Head, Body),
    /// The server closed a reused connection before answering: safe to send again.
    Stale,
}

impl Pool {
    /// POSTs `body` over a kept-alive connection (a fresh one if none is idle or the idle one went
    /// stale). A complete response is read into memory and the connection goes back to the pool;
    /// a stream (server-sent events, or no length) keeps the connection to itself.
    pub fn send(&self, url: &Url, headers: &[(&str, String)], body: &[u8], timeout: Duration) -> Result<(Head, Body), String> {
        let reused = crate::util::lock(&self.idle).pop();
        if let Some(c) = reused {
            match self.exchange(c, url, headers, body, timeout, true)? {
                Sent::Done(h, b) => return Ok((h, b)),
                Sent::Stale => {}
            }
        }
        let s = connect(url)?;
        let _ = s.set_nodelay(true);
        let r = BufReader::new(s.try_clone().map_err(|e| e.to_string())?);
        match self.exchange((r, s), url, headers, body, timeout, false)? {
            Sent::Done(h, b) => Ok((h, b)),
            Sent::Stale => Err("the connection closed without a response".into()),
        }
    }

    fn exchange(
        &self,
        (mut r, mut w): (BufReader<TcpStream>, TcpStream),
        url: &Url,
        headers: &[(&str, String)],
        body: &[u8],
        timeout: Duration,
        reused: bool,
    ) -> Result<Sent, String> {
        w.set_read_timeout(Some(timeout)).map_err(|e| e.to_string())?;
        w.set_write_timeout(Some(Duration::from_secs(10))).map_err(|e| e.to_string())?;
        let mut req = format!("POST {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: aitocraft/{}\r\n", url.path, url.authority(), env!("CARGO_PKG_VERSION"));
        for (k, v) in headers {
            if v.contains(['\r', '\n']) || k.contains(['\r', '\n', ':']) {
                continue;
            }
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        req.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
        let mut msg = req.into_bytes();
        msg.extend_from_slice(body);
        if let Err(e) = w.write_all(&msg).and_then(|()| w.flush()) {
            return if reused { Ok(Sent::Stale) } else { Err(e.to_string()) };
        }
        let head = loop {
            match read_head(&mut r) {
                Ok(h) if (100..200).contains(&h.status) => continue,
                Ok(h) => break h,
                Err(_) if reused => return Ok(Sent::Stale),
                Err(e) => return Err(e),
            }
        };
        let ctype = head.header("content-type").unwrap_or("").to_ascii_lowercase();
        let chunked = head.header("transfer-encoding").is_some_and(|t| t.to_ascii_lowercase().contains("chunked"));
        let length = head.header("content-length").and_then(|v| v.trim().parse::<u64>().ok());
        let closes = head.header("connection").is_some_and(|c| c.eq_ignore_ascii_case("close"));
        if ctype.starts_with("text/event-stream") || (!chunked && length.is_none() && !matches!(head.status, 202 | 204 | 304)) {
            let body: Body = if chunked {
                Box::new(BufReader::new(Chunked { inner: r, left: 0, after_chunk: false, done: false, total: 0 }))
            } else {
                Box::new(r.take(MAX_BODY as u64))
            };
            return Ok(Sent::Done(head, body));
        }
        let bytes = if chunked {
            let mut c = BufReader::new(Chunked { inner: &mut r, left: 0, after_chunk: false, done: false, total: 0 });
            read_all(&mut c, MAX_BODY)?
        } else {
            let n = length.unwrap_or(0);
            if n > MAX_BODY as u64 {
                return Err(format!("response of {n} bytes is too large"));
            }
            let mut v = vec![0; usize::try_from(n).map_err(|e| e.to_string())?];
            r.read_exact(&mut v).map_err(|e| e.to_string())?;
            v
        };
        if !closes {
            let mut idle = crate::util::lock(&self.idle);
            if idle.len() < MAX_IDLE {
                idle.push((r, w));
            }
        }
        Ok(Sent::Done(head, Box::new(std::io::Cursor::new(bytes))))
    }
}

/// Stops a stream that another thread reads.
pub fn shut(stream: &TcpStream) {
    let _ = stream.shutdown(Shutdown::Both);
}

fn read_line_bounded(r: &mut dyn BufRead, max: usize) -> Result<Vec<u8>, String> {
    let mut line = Vec::new();
    r.take(max as u64 + 1).read_until(b'\n', &mut line).map_err(|e| e.to_string())?;
    if line.len() > max {
        return Err("line too long".into());
    }
    Ok(line)
}

fn read_head(r: &mut dyn BufRead) -> Result<Head, String> {
    let first = read_line_bounded(r, MAX_HEAD)?;
    if first.is_empty() {
        return Err("the connection closed without a response".into());
    }
    let first = String::from_utf8_lossy(&first);
    let mut parts = first.split_whitespace();
    let version = parts.next().unwrap_or("");
    if !version.starts_with("HTTP/") {
        return Err("not an HTTP response".into());
    }
    let status = parts.next().and_then(|s| s.parse::<u16>().ok()).ok_or("bad status line")?;
    let mut headers = Vec::new();
    let mut size = first.len();
    loop {
        let line = read_line_bounded(r, MAX_HEAD)?;
        size += line.len();
        if size > MAX_HEAD {
            return Err("response head too large".into());
        }
        if line.is_empty() {
            return Err("the connection closed in the response head".into());
        }
        let line = String::from_utf8_lossy(&line);
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    Ok(Head { status, headers })
}

/// Decodes `Transfer-Encoding: chunked`.
struct Chunked<R: BufRead> {
    inner: R,
    left: u64,
    after_chunk: bool,
    done: bool,
    total: usize,
}

impl<R: BufRead> Read for Chunked<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let bad = |m: &str| std::io::Error::new(std::io::ErrorKind::InvalidData, m.to_string());
        if self.done || buf.is_empty() {
            return Ok(0);
        }
        if self.left == 0 {
            if self.after_chunk {
                read_line_bounded(&mut self.inner, 1024).map_err(|e| bad(&e))?;
                self.after_chunk = false;
            }
            let line = read_line_bounded(&mut self.inner, 1024).map_err(|e| bad(&e))?;
            if line.is_empty() {
                return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "chunked body cut short"));
            }
            let line = String::from_utf8_lossy(&line);
            let size = line.trim().split(';').next().unwrap_or("").trim();
            let size = u64::from_str_radix(size, 16).map_err(|_| bad("bad chunk size"))?;
            if size == 0 {
                // Trailers, up to the empty line.
                for _ in 0..64 {
                    let l = read_line_bounded(&mut self.inner, 4096).map_err(|e| bad(&e))?;
                    if l.is_empty() || l == b"\r\n" || l == b"\n" {
                        break;
                    }
                }
                self.done = true;
                return Ok(0);
            }
            self.left = size;
        }
        let want = buf.len().min(usize::try_from(self.left).unwrap_or(usize::MAX));
        let n = self.inner.read(&mut buf[..want])?;
        if n == 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "chunked body cut short"));
        }
        self.left -= n as u64;
        if self.left == 0 {
            self.after_chunk = true;
        }
        self.total += n;
        if self.total > MAX_BODY {
            return Err(bad("body too large"));
        }
        Ok(n)
    }
}

/// Reads the whole body, refusing more than `max` bytes.
pub fn read_all(body: &mut dyn BufRead, max: usize) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    body.take(max as u64 + 1).read_to_end(&mut out).map_err(|e| e.to_string())?;
    if out.len() > max {
        return Err("response too large".into());
    }
    Ok(out)
}

/// Reads one line of at most `max` bytes (`None` at the end of the body).
pub fn next_line(body: &mut dyn BufRead, max: usize) -> Result<Option<String>, String> {
    let line = read_line_bounded(body, max)?;
    if line.is_empty() {
        return Ok(None);
    }
    Ok(Some(String::from_utf8_lossy(&line).trim_end_matches(['\r', '\n']).to_string()))
}

/// The data of the next server-sent event (`None` at the end of the stream).
pub fn next_sse_data(body: &mut dyn BufRead, max: usize) -> Result<Option<String>, String> {
    let mut data = String::new();
    let mut any = false;
    loop {
        let Some(line) = next_line(body, max)? else {
            return Ok(any.then_some(data));
        };
        if line.is_empty() {
            if any {
                return Ok(Some(data));
            }
            continue;
        }
        if let Some(d) = line.strip_prefix("data:") {
            if any {
                data.push('\n');
            }
            data.push_str(d.strip_prefix(' ').unwrap_or(d));
            any = true;
            if data.len() > max {
                return Err("event too large".into());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_on_this_machine_only() {
        let u = Url::parse("http://127.0.0.1:7980/mcp").unwrap();
        assert_eq!((u.host.as_str(), u.port, u.path.as_str()), ("127.0.0.1", 7980, "/mcp"));
        assert_eq!(Url::parse("http://[::1]:7982/mcp").unwrap().host, "[::1]");
        assert_eq!(Url::parse("http://localhost:7984").unwrap().path, "/");
        assert!(Url::parse("http://10.0.0.5:7980/mcp").is_err());
        assert!(Url::parse("https://127.0.0.1:7980/mcp").is_err());
        assert!(Url::parse("http://127.0.0.1:99999/mcp").is_err());
        assert_eq!(u.with_path("/events?document=a").to_string(), "http://127.0.0.1:7980/events?document=a");
    }

    #[test]
    fn chunked_bodies_and_events() {
        let raw = b"5\r\nhello\r\n7; x=1\r\n, world\r\n0\r\n\r\n";
        let mut c = BufReader::new(Chunked { inner: &raw[..], left: 0, after_chunk: false, done: false, total: 0 });
        assert_eq!(read_all(&mut c, 100).unwrap(), b"hello, world");
        let sse = b": hi\n\nevent: message\ndata: {\"a\":\ndata: 1}\n\ndata: 2\n";
        let mut r: &[u8] = sse;
        assert_eq!(next_sse_data(&mut r, 1000).unwrap().unwrap(), "{\"a\":\n1}");
        assert_eq!(next_sse_data(&mut r, 1000).unwrap().unwrap(), "2");
        assert_eq!(next_sse_data(&mut r, 1000).unwrap(), None);
    }

    /// A keep-alive server answering `n` requests per connection; counts connections.
    fn server(per_conn: usize) -> (Url, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = Url::parse(&format!("http://127.0.0.1:{}/mcp", l.local_addr().unwrap().port())).unwrap();
        let conns = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let c = conns.clone();
        std::thread::spawn(move || {
            for s in l.incoming().flatten() {
                c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                std::thread::spawn(move || {
                    let mut w = s.try_clone().unwrap();
                    let mut r = BufReader::new(s);
                    for i in 0..per_conn {
                        let Ok(h) = read_head_req(&mut r) else { return };
                        let mut b = vec![0; h];
                        if r.read_exact(&mut b).is_err() {
                            return;
                        }
                        let body = format!("{{\"n\":{i}}}");
                        if i % 2 == 0 {
                            write!(w, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
                        } else {
                            write!(w, "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{body}\r\n0\r\n\r\n", body.len()).unwrap();
                        }
                    }
                });
            }
        });
        (url, conns)
    }

    fn read_head_req(r: &mut BufReader<TcpStream>) -> Result<usize, ()> {
        let mut len = 0;
        loop {
            let mut line = String::new();
            if r.read_line(&mut line).map_err(|_| ())? == 0 {
                return Err(());
            }
            if line == "\r\n" {
                return Ok(len);
            }
            if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                len = v.trim().parse().unwrap();
            }
        }
    }

    #[test]
    fn calls_reuse_one_connection_and_survive_a_closed_one() {
        let (url, conns) = server(100);
        let pool = Pool::default();
        for i in 0..6 {
            let (h, mut b) = pool.send(&url, &[], b"{}", Duration::from_secs(5)).unwrap();
            assert_eq!(h.status, 200);
            assert_eq!(read_all(&mut b, 100).unwrap(), format!("{{\"n\":{i}}}").as_bytes(), "plain and chunked bodies");
        }
        assert_eq!(conns.load(std::sync::atomic::Ordering::SeqCst), 1, "one connection for all");
        // A server that closes after each answer: the stale connection is replaced, the call succeeds.
        let (url, conns) = server(1);
        let pool = Pool::default();
        for _ in 0..3 {
            let (_, mut b) = pool.send(&url, &[], b"{}", Duration::from_secs(5)).unwrap();
            assert_eq!(read_all(&mut b, 100).unwrap(), b"{\"n\":0}");
        }
        assert_eq!(conns.load(std::sync::atomic::Ordering::SeqCst), 3);
    }
}
