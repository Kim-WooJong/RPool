//! Minimal blocking HTTP/1.1 client for the local `rclone rcd` daemon: one
//! request per connection (`Connection: close`), Content-Length, chunked and
//! read-to-EOF bodies, bounded heads, cancellation/deadline checks while
//! waiting. Never logs requests or responses.
use super::process;
use crate::storage::error::StorageError;
use crate::storage::traits::OperationContext;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
#[cfg(unix)]
use std::path::PathBuf;
use std::time::Duration;

/// Maximum size of a response head (status line plus headers).
const HEAD_LIMIT: usize = 64 * 1024;
/// Read timeout slice; reads wake this often to check cancel/deadline.
const WAIT: Duration = Duration::from_millis(100);
/// TCP connect timeout.
const CONNECT: Duration = Duration::from_secs(2);
/// Write timeout for sending a request.
const SEND: Duration = Duration::from_secs(10);

/// Where the rclone rc daemon listens.
#[derive(Clone)]
pub(super) enum Endpoint {
    /// Unix socket path (unix only).
    #[cfg(unix)]
    Unix(PathBuf),
    /// Loopback TCP address (Windows), optionally with Basic auth.
    #[cfg_attr(unix, allow(dead_code))]
    Tcp {
        /// Socket address, normally 127.0.0.1 with the daemon's port.
        address: SocketAddr,
        /// Full `Authorization` header value, or empty for none.
        authorization: String,
    },
}

/// Why a request produced no usable response; mapped to `daemon::Failure` by the daemon.
#[derive(Debug)]
pub(super) enum HttpError {
    /// No usable answer (connect/I/O/protocol failure). Retrying a read
    /// through another transport is safe.
    Transport,
    /// The caller's sink refused bytes.
    Sink(io::Error),
    /// Cancellation or deadline.
    Stopped(StorageError),
}

/// Connected socket of either endpoint kind.
enum Stream {
    /// Unix socket connection.
    #[cfg(unix)]
    Unix(UnixStream),
    /// TCP connection.
    Tcp(TcpStream),
}
impl Read for Stream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self {
            #[cfg(unix)]
            Self::Unix(stream) => stream.read(buffer),
            Self::Tcp(stream) => stream.read(buffer),
        }
    }
}
impl Write for Stream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        match self {
            #[cfg(unix)]
            Self::Unix(stream) => stream.write(bytes),
            Self::Tcp(stream) => stream.write(bytes),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            #[cfg(unix)]
            Self::Unix(stream) => stream.flush(),
            Self::Tcp(stream) => stream.flush(),
        }
    }
}

/// Opens a connection to `endpoint` with the read/write timeouts set.
fn connect(endpoint: &Endpoint) -> io::Result<Stream> {
    match endpoint {
        #[cfg(unix)]
        Endpoint::Unix(path) => {
            let stream = UnixStream::connect(path)?;
            stream.set_read_timeout(Some(WAIT))?;
            stream.set_write_timeout(Some(SEND))?;
            Ok(Stream::Unix(stream))
        }
        Endpoint::Tcp { address, .. } => {
            let stream = TcpStream::connect_timeout(address, CONNECT)?;
            stream.set_read_timeout(Some(WAIT))?;
            stream.set_write_timeout(Some(SEND))?;
            stream.set_nodelay(true)?;
            Ok(Stream::Tcp(stream))
        }
    }
}

/// `Stopped` if the operation was cancelled or passed its deadline.
fn stopped(ctx: &OperationContext) -> Result<(), HttpError> {
    process::check(ctx).map_err(HttpError::Stopped)
}

/// Socket plus read buffer; `buffer[start..end]` holds unconsumed bytes.
struct Connection {
    /// Underlying socket.
    stream: Stream,
    /// Read buffer (`process::CHUNK` bytes).
    buffer: Box<[u8]>,
    /// Offset of the first unconsumed byte.
    start: usize,
    /// Offset just past the last buffered byte.
    end: usize,
}
impl Connection {
    /// Reads more bytes; `false` at EOF. Waits in short slices so a cancel or
    /// deadline is noticed while the daemon is busy.
    fn fill(&mut self, ctx: &OperationContext) -> Result<bool, HttpError> {
        if self.start == self.end {
            (self.start, self.end) = (0, 0);
        } else if self.end == self.buffer.len() {
            self.buffer.copy_within(self.start..self.end, 0);
            (self.start, self.end) = (0, self.end - self.start);
        }
        if self.end == self.buffer.len() {
            return Err(HttpError::Transport);
        }
        loop {
            stopped(ctx)?;
            match self.stream.read(&mut self.buffer[self.end..]) {
                Ok(0) => return Ok(false),
                Ok(n) => {
                    self.end += n;
                    return Ok(true);
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock
                            | io::ErrorKind::TimedOut
                            | io::ErrorKind::Interrupted
                    ) => {}
                Err(_) => return Err(HttpError::Transport),
            }
        }
    }
    /// Buffered bytes not yet consumed.
    fn available(&self) -> &[u8] {
        &self.buffer[self.start..self.end]
    }
    /// One CRLF/LF-terminated line without its terminator.
    fn line(&mut self, ctx: &OperationContext) -> Result<Vec<u8>, HttpError> {
        loop {
            if let Some(at) = self.available().iter().position(|b| *b == b'\n') {
                let mut line = self.available()[..at].to_vec();
                self.start += at + 1;
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                return Ok(line);
            }
            if !self.fill(ctx)? {
                return Err(HttpError::Transport);
            }
        }
    }
}

/// How the response body is delimited.
enum Framing {
    /// Exactly this many bytes remain.
    Length(u64),
    /// Chunked encoding: `remaining` bytes left in the current chunk; `done` after the last chunk.
    Chunked {
        /// Bytes left in the current chunk.
        remaining: u64,
        /// The terminating zero-size chunk was read.
        done: bool,
    },
    /// Body runs until the connection closes.
    Eof,
}

/// Parsed response head plus the connection to stream the body from.
pub(super) struct Response {
    /// HTTP status code.
    pub(super) status: u16,
    /// Header (lowercased name, trimmed value) pairs.
    headers: Vec<(String, String)>,
    /// Connection the body is read from.
    connection: Connection,
    /// Body framing derived from the head.
    framing: Framing,
}

/// Sends one request and parses the response head. `headers` must not
/// contain CR/LF; the body is sent with a Content-Length.
pub(super) fn send(
    endpoint: &Endpoint,
    ctx: &OperationContext,
    method: &str,
    target: &str,
    headers: &[(&str, &str)],
    body: Option<&[u8]>,
) -> Result<Response, HttpError> {
    match body {
        Some(body) => send_parts(endpoint, ctx, method, target, headers, Some(&[body])),
        None => send_parts(endpoint, ctx, method, target, headers, None),
    }
}

/// [`send`] with a body written from several slices (no joined copy of a
/// large upload body).
pub(super) fn send_parts(
    endpoint: &Endpoint,
    ctx: &OperationContext,
    method: &str,
    target: &str,
    headers: &[(&str, &str)],
    body: Option<&[&[u8]]>,
) -> Result<Response, HttpError> {
    stopped(ctx)?;
    let mut stream = connect(endpoint).map_err(|_| HttpError::Transport)?;
    let mut head =
        format!("{method} {target} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n");
    #[allow(irrefutable_let_patterns)] // Windows has only the TCP endpoint.
    if let Endpoint::Tcp { authorization, .. } = endpoint {
        if !authorization.is_empty() {
            head.push_str(&format!("Authorization: {authorization}\r\n"));
        }
    }
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    let parts = body.unwrap_or_default();
    if body.is_some() {
        let length: usize = parts.iter().map(|part| part.len()).sum();
        head.push_str(&format!("Content-Length: {length}\r\n"));
    }
    head.push_str("\r\n");
    stream
        .write_all(head.as_bytes())
        .and_then(|()| parts.iter().try_for_each(|part| stream.write_all(part)))
        .and_then(|()| stream.flush())
        .map_err(|_| HttpError::Transport)?;
    let mut connection = Connection {
        stream,
        buffer: vec![0; process::CHUNK].into_boxed_slice(),
        start: 0,
        end: 0,
    };
    loop {
        let (status, headers) = read_head(&mut connection, ctx)?;
        if (100..200).contains(&status) {
            continue; // Interim response; the final one follows.
        }
        let framing = framing(method, status, &headers)?;
        return Ok(Response {
            status,
            headers,
            connection,
            framing,
        });
    }
}

/// Reads the status line and headers (bounded by `HEAD_LIMIT`); malformed heads are `Transport` errors.
fn read_head(
    connection: &mut Connection,
    ctx: &OperationContext,
) -> Result<(u16, Vec<(String, String)>), HttpError> {
    let mut total = 0usize;
    let mut next = |connection: &mut Connection| -> Result<String, HttpError> {
        let line = connection.line(ctx)?;
        total += line.len() + 2;
        if total > HEAD_LIMIT {
            return Err(HttpError::Transport);
        }
        String::from_utf8(line).map_err(|_| HttpError::Transport)
    };
    let status_line = next(connection)?;
    let mut parts = status_line.splitn(3, ' ');
    let (Some(version), Some(code)) = (parts.next(), parts.next()) else {
        return Err(HttpError::Transport);
    };
    if !version.starts_with("HTTP/1.") || code.len() != 3 {
        return Err(HttpError::Transport);
    }
    let status = code.parse().map_err(|_| HttpError::Transport)?;
    let mut headers = Vec::new();
    loop {
        let line = next(connection)?;
        if line.is_empty() {
            return Ok((status, headers));
        }
        let (name, value) = line.split_once(':').ok_or(HttpError::Transport)?;
        headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
    }
}

/// Body framing from method, status and headers; conflicting Content-Length
/// or a non-chunked Transfer-Encoding is rejected.
fn framing(method: &str, status: u16, headers: &[(String, String)]) -> Result<Framing, HttpError> {
    if method == "HEAD" || status == 204 || status == 304 {
        return Ok(Framing::Length(0));
    }
    fn values<'a>(
        headers: &'a [(String, String)],
        name: &'a str,
    ) -> impl Iterator<Item = &'a str> + 'a {
        headers
            .iter()
            .filter(move |(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
    if let Some(coding) = values(headers, "transfer-encoding").last() {
        return if coding
            .rsplit(',')
            .next()
            .is_some_and(|last| last.trim().eq_ignore_ascii_case("chunked"))
        {
            Ok(Framing::Chunked {
                remaining: 0,
                done: false,
            })
        } else {
            Err(HttpError::Transport)
        };
    }
    let mut length = None;
    for value in values(headers, "content-length") {
        let parsed: u64 = value.parse().map_err(|_| HttpError::Transport)?;
        if length.is_some_and(|known| known != parsed) {
            return Err(HttpError::Transport);
        }
        length = Some(parsed);
    }
    Ok(length.map_or(Framing::Eof, Framing::Length))
}

impl Response {
    /// First header value named `name` (case-insensitive).
    pub(super) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// Streams the whole body into `sink`; returns the bytes written. A body
    /// cut short (EOF before its framing says) is a transport error.
    pub(super) fn copy_to(
        &mut self,
        ctx: &OperationContext,
        sink: &mut dyn Write,
    ) -> Result<u64, HttpError> {
        let mut total = 0u64;
        loop {
            let want = match &mut self.framing {
                Framing::Length(0) => return Ok(total),
                Framing::Length(remaining) => *remaining,
                Framing::Eof => u64::MAX,
                Framing::Chunked { done: true, .. } => return Ok(total),
                Framing::Chunked { remaining: 0, .. } => {
                    self.next_chunk(ctx)?;
                    continue;
                }
                Framing::Chunked { remaining, .. } => *remaining,
            };
            if self.connection.available().is_empty() && !self.connection.fill(ctx)? {
                return match self.framing {
                    Framing::Eof => Ok(total),
                    _ => Err(HttpError::Transport),
                };
            }
            stopped(ctx)?;
            let take = (self.connection.available().len() as u64).min(want) as usize;
            sink.write_all(&self.connection.available()[..take])
                .map_err(HttpError::Sink)?;
            self.connection.start += take;
            total = total.checked_add(take as u64).ok_or(HttpError::Transport)?;
            match &mut self.framing {
                Framing::Length(remaining) | Framing::Chunked { remaining, .. } => {
                    *remaining -= take as u64
                }
                Framing::Eof => {}
            }
        }
    }

    /// Parses the next chunk-size line, consuming trailers after the last chunk.
    fn next_chunk(&mut self, ctx: &OperationContext) -> Result<(), HttpError> {
        let mut line = self.connection.line(ctx)?;
        if line.is_empty() {
            // CRLF closing the previous chunk's data.
            line = self.connection.line(ctx)?;
        }
        let text = std::str::from_utf8(&line).map_err(|_| HttpError::Transport)?;
        let size_text = text.split(';').next().unwrap_or("").trim();
        if size_text.is_empty() || size_text.len() > 16 {
            return Err(HttpError::Transport);
        }
        let size = u64::from_str_radix(size_text, 16).map_err(|_| HttpError::Transport)?;
        let Framing::Chunked { remaining, done } = &mut self.framing else {
            return Err(HttpError::Transport);
        };
        if size == 0 {
            *done = true;
            // Trailers until the empty line.
            let mut trailers = 0usize;
            loop {
                let trailer = self.connection.line(ctx)?;
                trailers += trailer.len() + 2;
                if trailer.is_empty() {
                    return Ok(());
                }
                if trailers > HEAD_LIMIT {
                    return Err(HttpError::Transport);
                }
            }
        }
        *remaining = size;
        Ok(())
    }

    /// Whole body into memory, at most `limit` bytes (else a transport error).
    pub(super) fn body(
        &mut self,
        ctx: &OperationContext,
        limit: usize,
    ) -> Result<Vec<u8>, HttpError> {
        let mut sink = super::BoundedVec {
            bytes: Vec::new(),
            limit,
        };
        match self.copy_to(ctx, &mut sink) {
            Ok(_) => Ok(sink.bytes),
            Err(HttpError::Sink(_)) => Err(HttpError::Transport),
            Err(error) => Err(error),
        }
    }
}

/// Percent-encodes everything except RFC 3986 unreserved characters.
pub(super) fn encode_component(text: &str, out: &mut String) {
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    /// Serves `reply` once per connection on a fresh unix socket, returning the
    /// request bytes it read (up to the end of the head).
    fn serve(
        replies: Vec<Vec<u8>>,
    ) -> (
        tempfile::TempDir,
        Endpoint,
        std::thread::JoinHandle<Vec<String>>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let handle = std::thread::spawn(move || {
            let mut heads = Vec::new();
            for reply in replies {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = Vec::new();
                let mut byte = [0u8; 1];
                while !request.ends_with(b"\r\n\r\n") {
                    if stream.read(&mut byte).unwrap() == 0 {
                        break;
                    }
                    request.push(byte[0]);
                }
                heads.push(String::from_utf8_lossy(&request).into_owned());
                let _ = stream.write_all(&reply);
            }
            heads
        });
        (dir, Endpoint::Unix(path), handle)
    }

    fn get(endpoint: &Endpoint) -> Result<(u16, Vec<u8>), HttpError> {
        let ctx = OperationContext::none();
        let mut response = send(
            endpoint,
            &ctx,
            "GET",
            "/x%20y",
            &[("Range", "bytes=1-2")],
            None,
        )?;
        let body = response.body(&ctx, 1 << 20)?;
        Ok((response.status, body))
    }

    #[test]
    fn content_length_chunked_eof_and_interim_bodies() {
        let (_dir, endpoint, handle) = serve(vec![
            b"HTTP/1.1 206 Partial Content\r\nContent-Length: 5\r\nContent-Range: bytes 1-5/9\r\n\r\nhelloEXTRA".to_vec(),
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3;ext=1\r\nabc\r\n0A\r\n0123456789\r\n0\r\nTrailer: x\r\n\r\n".to_vec(),
            b"HTTP/1.0 200 OK\r\n\r\nuntil eof".to_vec(),
            b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 404 Not Found\n\n".to_vec(),
        ]);
        assert_eq!(get(&endpoint).unwrap(), (206, b"hello".to_vec()));
        assert_eq!(get(&endpoint).unwrap(), (200, b"abc0123456789".to_vec()));
        assert_eq!(get(&endpoint).unwrap(), (200, b"until eof".to_vec()));
        assert_eq!(get(&endpoint).unwrap(), (404, Vec::new()));
        let heads = handle.join().unwrap();
        assert!(heads[0].starts_with("GET /x%20y HTTP/1.1\r\n"));
        assert!(heads[0].contains("\r\nRange: bytes=1-2\r\n"));
        assert!(heads[0].contains("\r\nConnection: close\r\n"));
    }

    #[test]
    fn malformed_truncated_and_oversized_responses_are_transport_errors() {
        let mut huge = b"HTTP/1.1 200 OK\r\nX: ".to_vec();
        huge.extend(vec![b'a'; HEAD_LIMIT + 10]);
        huge.extend(b"\r\n\r\n");
        let (_dir, endpoint, handle) = serve(vec![
            b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nshort".to_vec(),
            b"garbage\r\n\r\n".to_vec(),
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n".to_vec(),
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nab".to_vec(),
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nContent-Length: 3\r\n\r\nabc".to_vec(),
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip\r\n\r\nabc".to_vec(),
            huge,
            b"HTTP/1.1 200".to_vec(),
        ]);
        for _ in 0..8 {
            assert!(matches!(get(&endpoint), Err(HttpError::Transport)));
        }
        handle.join().unwrap();
        // Nobody listening.
        assert!(matches!(get(&endpoint), Err(HttpError::Transport)));
    }

    #[test]
    fn body_limit_sink_errors_and_cancellation() {
        let (_dir, endpoint, handle) = serve(vec![
            b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nabcdef".to_vec(),
            b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nabcdef".to_vec(),
        ]);
        let ctx = OperationContext::none();
        let mut response = send(&endpoint, &ctx, "POST", "/rc/noop", &[], Some(b"{}")).unwrap();
        assert!(matches!(response.body(&ctx, 5), Err(HttpError::Transport)));
        struct Refuse;
        impl Write for Refuse {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::other("full"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut response = send(&endpoint, &ctx, "GET", "/", &[], None).unwrap();
        assert!(matches!(
            response.copy_to(&ctx, &mut Refuse),
            Err(HttpError::Sink(_))
        ));
        let heads = handle.join().unwrap();
        assert!(heads[0].contains("Content-Length: 2\r\n"));

        // A server that never answers: the deadline stops the wait.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("silent.sock");
        let _listener = UnixListener::bind(&path).unwrap();
        let started = std::time::Instant::now();
        let ctx = OperationContext::with_deadline(started + Duration::from_millis(300));
        let error = send(&Endpoint::Unix(path), &ctx, "GET", "/", &[], None)
            .err()
            .unwrap();
        assert!(matches!(
            error,
            HttpError::Stopped(StorageError::Timeout { .. })
        ));
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn component_encoding_keeps_only_unreserved_bytes() {
        let mut out = String::new();
        encode_component("a b/[x]%20:한", &mut out);
        assert_eq!(out, "a%20b%2F%5Bx%5D%2520%3A%ED%95%9C");
    }
}
