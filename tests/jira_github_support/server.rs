//! A fake HTTP/1.1 server on a free localhost port, with one thread: canned answers by method
//! and path, a fallback for each other request, and a record of each request. Each answer
//! closes its connection.

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::Value;

/// The body of an answer.
#[derive(Clone, Debug)]
pub enum Body {
    Json(Value),
    Bytes(Vec<u8>),
    None,
}

/// One answer: a status, a body, more headers, and a wait before it goes out.
#[derive(Clone, Debug)]
pub struct Reply {
    pub status: u16,
    pub body: Body,
    pub headers: Vec<(String, String)>,
    pub delay: Option<Duration>,
}

impl Reply {
    pub fn json(status: u16, value: Value) -> Reply {
        Reply {
            status,
            body: Body::Json(value),
            headers: Vec::new(),
            delay: None,
        }
    }

    pub fn text(status: u16, text: &str) -> Reply {
        Reply {
            status,
            body: Body::Bytes(text.as_bytes().to_vec()),
            headers: vec![("Content-Type".into(), "text/plain".into())],
            delay: None,
        }
    }

    pub fn empty(status: u16) -> Reply {
        Reply {
            status,
            body: Body::None,
            headers: Vec::new(),
            delay: None,
        }
    }

    pub fn header(mut self, name: &str, value: &str) -> Reply {
        self.headers.push((name.into(), value.into()));
        self
    }

    pub fn delayed(mut self, delay: Duration) -> Reply {
        self.delay = Some(delay);
        self
    }
}

/// One request as the server read it.
#[derive(Clone, Debug)]
pub struct Record {
    pub method: String,
    /// The path with the percent forms decoded, and no query.
    pub path: String,
    pub raw_path: String,
    /// The first value of each key of the query.
    pub query: BTreeMap<String, String>,
    /// The headers, with lowercase names.
    pub headers: BTreeMap<String, String>,
    /// The body as JSON, as a string when it is not JSON, or null when it is empty.
    pub body: Value,
}

type Handler = Arc<Mutex<dyn FnMut(&Record) -> Reply + Send>>;
type Fallback = Arc<Mutex<dyn FnMut(&Record) -> Option<Reply> + Send>>;

enum Route {
    Handler(Handler),
    /// Served in order; the last one repeats.
    Queue(Vec<Reply>),
}

#[derive(Default)]
struct Inner {
    routes: HashMap<(String, String), Route>,
    fallback: Option<Fallback>,
    requests: Vec<Record>,
}

pub struct FakeServer {
    pub port: u16,
    pub url: String,
    inner: Arc<Mutex<Inner>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl FakeServer {
    pub fn start() -> FakeServer {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind a free port");
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let inner = Arc::new(Mutex::new(Inner::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let inner = Arc::clone(&inner);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => serve(&inner, stream),
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(2));
                        }
                        Err(_) => std::thread::sleep(Duration::from_millis(2)),
                    }
                }
            })
        };
        FakeServer {
            port,
            url: format!("http://127.0.0.1:{port}"),
            inner,
            stop,
            thread: Some(thread),
        }
    }

    /// Answers `method path` with the handler.
    pub fn route(
        &self,
        method: &str,
        path: &str,
        handler: impl FnMut(&Record) -> Reply + Send + 'static,
    ) {
        let handler: Handler = Arc::new(Mutex::new(handler));
        self.inner
            .lock()
            .unwrap()
            .routes
            .insert((method.into(), path.into()), Route::Handler(handler));
    }

    /// Answers `method path` with a JSON value.
    pub fn json_route(&self, method: &str, path: &str, value: Value, status: u16) {
        self.route(method, path, move |_| Reply::json(status, value.clone()));
    }

    /// Answers `method path` with the replies in order; the last one repeats.
    pub fn queue(&self, method: &str, path: &str, replies: Vec<Reply>) {
        self.inner
            .lock()
            .unwrap()
            .routes
            .insert((method.into(), path.into()), Route::Queue(replies));
    }

    /// Answers each request that no route answers, or gives `None` for a 404.
    pub fn fallback(&self, handler: impl FnMut(&Record) -> Option<Reply> + Send + 'static) {
        self.inner.lock().unwrap().fallback = Some(Arc::new(Mutex::new(handler)));
    }

    /// The requests, oldest first, with this method and path when they are given.
    pub fn calls(&self, method: Option<&str>, path: Option<&str>) -> Vec<Record> {
        self.inner
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|r| method.is_none_or(|m| r.method == m) && path.is_none_or(|p| r.path == p))
            .cloned()
            .collect()
    }

    /// The count of requests with this method and path.
    pub fn count(&self, method: &str, path: &str) -> usize {
        self.calls(Some(method), Some(path)).len()
    }
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The percent forms of a text decoded, and `+` as a space when `plus` is true.
pub fn unquote(text: &str, plus: bool) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(b) => {
                        out.push(b);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b'+' if plus => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The query as `parse_qs` reads it: the first value of each key, with no empty value.
fn parse_query(query: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let value = unquote(value, true);
        if value.is_empty() {
            continue;
        }
        out.entry(unquote(key, true)).or_insert(value);
    }
    out
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        302 => "Found",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        422 => "Unprocessable Entity",
        _ => "Status",
    }
}

fn read_request(stream: &TcpStream) -> Option<Record> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();
    let mut headers = BTreeMap::new();
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).ok()? == 0 {
            break;
        }
        let header = header.trim_end_matches(['\r', '\n']);
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            headers.insert(name.trim().to_lowercase(), value.trim().to_string());
        }
    }
    let mut raw = Vec::new();
    if let Some(length) = headers
        .get("content-length")
        .and_then(|l| l.parse::<usize>().ok())
    {
        raw.resize(length, 0);
        reader.read_exact(&mut raw).ok()?;
    } else if headers
        .get("transfer-encoding")
        .is_some_and(|t| t.eq_ignore_ascii_case("chunked"))
    {
        loop {
            let mut size = String::new();
            reader.read_line(&mut size).ok()?;
            let size = usize::from_str_radix(size.trim(), 16).ok()?;
            if size == 0 {
                let mut end = String::new();
                let _ = reader.read_line(&mut end);
                break;
            }
            let mut chunk = vec![0; size];
            reader.read_exact(&mut chunk).ok()?;
            raw.extend_from_slice(&chunk);
            let mut end = String::new();
            reader.read_line(&mut end).ok()?;
        }
    }
    let (raw_path, query) = target.split_once('?').unwrap_or((target.as_str(), ""));
    let body = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&raw).into_owned()))
    };
    Some(Record {
        method,
        path: unquote(raw_path, false),
        raw_path: raw_path.to_string(),
        query: parse_query(query),
        headers,
        body,
    })
}

fn serve(inner: &Arc<Mutex<Inner>>, mut stream: TcpStream) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
    let Some(record) = read_request(&stream) else {
        return;
    };
    enum Pick {
        Handler(Handler),
        Reply(Reply),
        Fallback(Option<Fallback>),
    }
    let pick = {
        let mut inner = inner.lock().unwrap();
        inner.requests.push(record.clone());
        let key = (record.method.clone(), record.path.clone());
        match inner.routes.get_mut(&key) {
            Some(Route::Handler(h)) => Pick::Handler(Arc::clone(h)),
            Some(Route::Queue(replies)) => {
                if replies.len() > 1 {
                    Pick::Reply(replies.remove(0))
                } else {
                    Pick::Reply(replies[0].clone())
                }
            }
            None => Pick::Fallback(inner.fallback.clone()),
        }
    };
    let reply = match pick {
        Pick::Handler(h) => (h.lock().unwrap())(&record),
        Pick::Reply(reply) => reply,
        Pick::Fallback(f) => f
            .and_then(|f| (f.lock().unwrap())(&record))
            .unwrap_or_else(|| {
                Reply::json(
                    404,
                    serde_json::json!({"message": "Not Found (fake server)"}),
                )
            }),
    };
    let mut headers = reply.headers.clone();
    let payload = match &reply.body {
        Body::Json(value) => {
            if !headers
                .iter()
                .any(|(k, _)| k.eq_ignore_ascii_case("content-type"))
            {
                headers.insert(0, ("Content-Type".into(), "application/json".into()));
            }
            value.to_string().into_bytes()
        }
        Body::Bytes(bytes) => bytes.clone(),
        Body::None => Vec::new(),
    };
    if let Some(delay) = reply.delay {
        std::thread::sleep(delay);
    }
    let mut head = format!("HTTP/1.1 {} {}\r\n", reply.status, reason(reply.status));
    for (name, value) in &headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str(&format!(
        "Content-Length: {}\r\nConnection: close\r\n\r\n",
        payload.len()
    ));
    let _ = stream.write_all(head.as_bytes());
    if record.method != "HEAD" {
        let _ = stream.write_all(&payload);
    }
    let _ = stream.flush();
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

/// One HTTP call to a local server with no library: the status and the body as JSON (null
/// when it is not JSON).
pub fn call(
    port: u16,
    method: &str,
    path: &str,
    body: Option<&Value>,
    headers: &[(&str, &str)],
) -> (u16, Value) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect to the fake server");
    stream
        .set_read_timeout(Some(Duration::from_secs(120)))
        .unwrap();
    let text = body.map(|b| b.to_string()).unwrap_or_default();
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Length: {}\r\nConnection: close\r\n",
        text.len()
    );
    if body.is_some() {
        request.push_str("Content-Type: application/json\r\n");
    }
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");
    request.push_str(&text);
    stream.write_all(request.as_bytes()).unwrap();
    let mut response = Vec::new();
    let _ = stream.read_to_end(&mut response);
    let response = String::from_utf8_lossy(&response).into_owned();
    let status = response
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, b)| b)
        .unwrap_or("");
    (status, serde_json::from_str(body).unwrap_or(Value::Null))
}
