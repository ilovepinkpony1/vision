//! HTTP: the base address of a service, one call with a time limit, and a JSON client.
//!
//! A redirect is never followed by itself, because it can carry a token to a different host. No
//! proxy of the environment takes part. Each call has a time limit for the whole call.

use std::io::Read;
use std::sync::OnceLock;
use std::time::Duration;

use serde_json::Value;

use super::core::{clean, fail, mask, Error, Res, DEFAULT_HTTP_TIMEOUT};

/// The parts of an address: the scheme, the host, and the rest, as far as the checks need them.
#[derive(Debug, Default, PartialEq)]
pub struct Parts {
    pub scheme: String,
    /// The user part before `@`, or "".
    pub user: String,
    /// True when the user part has a password.
    pub password: bool,
    /// The host name, lowercase, with no port and no brackets.
    pub host: String,
    /// The host and the port, as written.
    pub netloc: String,
    pub path: String,
    pub query: String,
    pub fragment: String,
}

/// Splits an address into its parts, as `urlsplit` does.
pub fn split(url: &str) -> Parts {
    let mut parts = Parts::default();
    let mut rest = url;
    if let Some(colon) = url.find(':') {
        let scheme = &url[..colon];
        let valid = scheme.starts_with(|c: char| c.is_ascii_alphabetic())
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
        if valid {
            parts.scheme = scheme.to_ascii_lowercase();
            rest = &url[colon + 1..];
        }
    }
    if let Some((before, fragment)) = rest.split_once('#') {
        parts.fragment = fragment.to_string();
        rest = before;
    }
    if let Some((before, query)) = rest.split_once('?') {
        parts.query = query.to_string();
        rest = before;
    }
    if let Some(after) = rest.strip_prefix("//") {
        let end = after.find('/').unwrap_or(after.len());
        parts.netloc = after[..end].to_string();
        parts.path = after[end..].to_string();
        let mut host = parts.netloc.as_str();
        if let Some((user, h)) = host.rsplit_once('@') {
            parts.password = user.split_once(':').is_some_and(|(_, p)| !p.is_empty());
            parts.user = user.split(':').next().unwrap_or("").to_string();
            host = h;
        }
        let host = if let Some(inner) = host.strip_prefix('[') {
            inner.split(']').next().unwrap_or("")
        } else {
            host.split(':').next().unwrap_or("")
        };
        parts.host = host.to_ascii_lowercase();
    } else {
        parts.path = rest.to_string();
    }
    parts
}

/// True for a host of this machine.
pub fn is_local(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

/// A base address of the configuration. A token goes only over https, or to this machine.
pub fn check_base(url: &str, key: &str) -> Res<String> {
    if url.trim().is_empty() {
        return fail(format!(
            "`{key}` in the [adapter] table must be an address, such as https://example.atlassian.net"
        ));
    }
    let parts = split(url.trim());
    if parts.scheme == "https" || (parts.scheme == "http" && is_local(&parts.host)) {
        if !parts.query.is_empty() || !parts.fragment.is_empty() || !parts.user.is_empty() {
            return fail(format!(
                "`{key}` must be a plain address with no query, fragment, or user"
            ));
        }
        return Ok(url.trim().trim_end_matches('/').to_string());
    }
    fail(format!(
        "`{key}` must start with https:// (a token never travels in clear text)"
    ))
}

/// The percent form of a text: each byte that is not a letter, a digit, or one of `_.-~` (and of
/// `safe`) becomes `%XX`.
fn quote(text: &str, safe: &[u8], plus: bool) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || b"_.-~".contains(&b) || safe.contains(&b) {
            out.push(char::from(b));
        } else if plus && b == b' ' {
            out.push('+');
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// One path segment from untrusted text.
pub fn segment(text: &str) -> String {
    quote(text, b"", false)
}

/// A branch name as a path, each part quoted.
pub fn ref_path(branch: &str) -> String {
    branch.split('/').map(segment).collect::<Vec<_>>().join("/")
}

/// The query part of an address from pairs, with `+` for a space.
pub fn urlencode(query: &[(String, String)]) -> String {
    query
        .iter()
        .map(|(k, v)| format!("{}={}", quote(k, b"", true), quote(v, b"", true)))
        .collect::<Vec<_>>()
        .join("&")
}

/// The `Authorization` value of basic authentication.
pub fn basic(user: &str, token: &str) -> String {
    format!(
        "Basic {}",
        crate::util::base64::encode(format!("{user}:{token}").as_bytes())
    )
}

/// The answer of one call.
#[derive(Clone, Debug)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    /// The body as JSON. An empty body is null.
    pub fn json(&self) -> Res<Value> {
        if self.body.is_empty() {
            return Ok(Value::Null);
        }
        let text = std::str::from_utf8(&self.body)
            .map_err(|_| Error::Failure("the answer is not JSON".into()))?;
        serde_json::from_str(text).map_err(|_| Error::Failure("the answer is not JSON".into()))
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// The error message of a Jira or GitHub answer, cut and masked, or "".
pub fn message(body: &[u8]) -> String {
    let Ok(data) = serde_json::from_slice::<Value>(body) else {
        return String::new();
    };
    let mut parts: Vec<String> = Vec::new();
    if let Value::Object(map) = &data {
        if let Some(Value::String(m)) = map.get("message") {
            parts.push(m.clone());
        }
        if let Some(Value::Array(items)) = map.get("errorMessages") {
            parts.extend(items.iter().filter_map(Value::as_str).map(str::to_string));
        }
        match map.get("errors") {
            Some(Value::Object(errors)) => parts.extend(
                errors
                    .iter()
                    .filter_map(|(k, v)| v.as_str().map(|v| format!("{k}: {v}"))),
            ),
            Some(Value::Array(errors)) => parts.extend(
                errors
                    .iter()
                    .filter_map(|e| e.get("message").and_then(Value::as_str))
                    .map(str::to_string),
            ),
            _ => {}
        }
    }
    mask(&clean(&parts.join("; "), 300))
}

fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .proxy(None)
            .build()
            .new_agent()
    })
}

fn seconds(timeout: f64) -> Duration {
    if timeout.is_finite() && timeout > 0.0 {
        Duration::from_secs_f64(timeout)
    } else {
        Duration::from_secs_f64(DEFAULT_HTTP_TIMEOUT)
    }
}

fn unreachable(e: &ureq::Error) -> Error {
    let reason = match e {
        ureq::Error::Timeout(_) => "timed out".to_string(),
        other => other.to_string(),
    };
    Error::Unreachable(format!("no answer: {}", mask(&clean(&reason, 200))))
}

/// One HTTP call with a time limit. Gives a [`Response`] for any status, a redirect included.
/// `limit` keeps only the last `limit` bytes of the body.
pub fn send(
    method: &str,
    url: &str,
    headers: &[(String, String)],
    body: Option<&[u8]>,
    timeout: f64,
    limit: Option<usize>,
) -> Res<Response> {
    let limit_of = Some(seconds(timeout));
    macro_rules! headed {
        ($builder:expr) => {{
            let mut builder = $builder.config().timeout_global(limit_of).build();
            for (key, value) in headers {
                builder = builder.header(key.as_str(), value.as_str());
            }
            builder
        }};
    }
    let agent = agent();
    let result = match method {
        "GET" => headed!(agent.get(url)).call(),
        "DELETE" => headed!(agent.delete(url)).call(),
        "HEAD" => headed!(agent.head(url)).call(),
        "POST" | "PUT" | "PATCH" => {
            let builder = match method {
                "POST" => headed!(agent.post(url)),
                "PUT" => headed!(agent.put(url)),
                _ => headed!(agent.patch(url)),
            };
            match body {
                Some(bytes) => builder.send(bytes),
                None => builder.send_empty(),
            }
        }
        other => return fail(format!("the method {other} is not known")),
    };
    let mut response = result.map_err(|e| unreachable(&e))?;
    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().to_string(),
                String::from_utf8_lossy(v.as_bytes()).into_owned(),
            )
        })
        .collect();
    let mut reader = response.body_mut().as_reader();
    let mut data = Vec::new();
    let mut chunk = vec![0u8; 65536];
    loop {
        let n = reader.read(&mut chunk).map_err(|e| {
            // A fault while the body comes is not a rule of the script: the first version of
            // the scripts reports it as an unexpected error.
            let text =
                if e.kind() == std::io::ErrorKind::TimedOut || e.to_string().contains("timeout") {
                    "TimeoutError: timed out".to_string()
                } else {
                    format!("OSError: {}", clean(&e.to_string(), 200))
                };
            Error::Unexpected(text)
        })?;
        if n == 0 {
            break;
        }
        data.extend_from_slice(&chunk[..n]);
        if let Some(keep) = limit {
            if data.len() > keep {
                let cut = data.len() - keep;
                data.drain(..cut);
            }
        }
    }
    Ok(Response {
        status,
        headers,
        body: data,
    })
}

/// A JSON client of one service: the base address, the auth header, and the time limit.
#[derive(Clone, Debug)]
pub struct Client {
    pub service: String,
    pub base: String,
    pub headers: Vec<(String, String)>,
    pub timeout: f64,
}

/// The options of one [`Client::call`].
#[derive(Clone, Debug, Default)]
pub struct Call<'a> {
    pub query: Vec<(String, String)>,
    pub body: Option<&'a Value>,
    /// The statuses that the call accepts. Empty: 200.
    pub ok: &'a [u16],
    pub limit: Option<usize>,
}

impl Client {
    /// Gives the response of a call whose status is in `ok`. A different status is an HTTP
    /// error, which names the call and the status, never a header.
    pub fn call(&self, method: &str, path: &str, what: &str, call: Call<'_>) -> Res<Response> {
        let mut url = format!("{}{}", self.base, path);
        if !call.query.is_empty() {
            url.push('?');
            url.push_str(&urlencode(&call.query));
        }
        let mut headers = self.headers.clone();
        let data = call.body.map(|body| {
            headers.push(("Content-Type".into(), "application/json".into()));
            serde_json::to_vec(body).unwrap_or_default()
        });
        let label = format!("{}: {} ({} {})", self.service, what, method, path);
        let response = match send(
            method,
            &url,
            &headers,
            data.as_deref(),
            self.timeout,
            call.limit,
        ) {
            Ok(response) => response,
            Err(Error::Unreachable(e)) => {
                return Err(Error::Unreachable(format!(
                    "{label}: {e} in {} s",
                    super::core::whole(self.timeout)
                )))
            }
            Err(e) => return Err(e),
        };
        let ok: &[u16] = if call.ok.is_empty() { &[200] } else { call.ok };
        if !ok.contains(&response.status) {
            let message = message(&response.body);
            let tail = if message.is_empty() {
                String::new()
            } else {
                format!(": {message}")
            };
            return Err(Error::Http {
                text: format!("{label} answered HTTP {}{tail}", response.status),
                status: response.status,
            });
        }
        Ok(response)
    }

    /// The JSON of a GET call that accepts only 200.
    pub fn get(&self, path: &str, what: &str) -> Res<Value> {
        self.call("GET", path, what, Call::default())?.json()
    }

    /// The JSON of a GET call with a query.
    pub fn get_query(&self, path: &str, what: &str, query: Vec<(String, String)>) -> Res<Value> {
        self.call(
            "GET",
            path,
            what,
            Call {
                query,
                ..Call::default()
            },
        )?
        .json()
    }

    /// Each item of a list that GitHub pages with `page` and `per_page`.
    pub fn pages(
        &self,
        path: &str,
        what: &str,
        query: &[(String, String)],
        per_page: usize,
        max_pages: usize,
    ) -> Res<Vec<Value>> {
        let mut items = Vec::new();
        for page in 1..=max_pages {
            let mut q: Vec<(String, String)> = query.to_vec();
            q.push(("per_page".into(), per_page.to_string()));
            q.push(("page".into(), page.to_string()));
            let batch = self.get_query(path, what, q)?;
            let batch = match batch {
                // The runs and the jobs come inside an object.
                Value::Object(map) => map
                    .into_iter()
                    .map(|(_, v)| v)
                    .find(Value::is_array)
                    .unwrap_or(Value::Array(Vec::new())),
                other => other,
            };
            let Value::Array(batch) = batch else {
                return fail(format!("{}: {} gave no list", self.service, what));
            };
            let count = batch.len();
            items.extend(batch);
            if count < per_page {
                break;
            }
        }
        Ok(items)
    }
}

/// A query from pairs of texts.
pub fn query(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_goes_only_over_https_or_to_this_machine() {
        assert_eq!(
            check_base("https://example.atlassian.net/", "jira_url").unwrap(),
            "https://example.atlassian.net"
        );
        assert!(check_base("http://127.0.0.1:8080", "x").is_ok());
        assert!(check_base("http://localhost", "x").is_ok());
        let err = check_base("http://example.com", "jira_url")
            .unwrap_err()
            .to_string();
        assert!(err.contains("https://"), "{err}");
        assert!(check_base("https://u@example.com", "x").is_err());
        assert!(check_base("https://example.com?a=1", "x").is_err());
        assert!(check_base(" ", "x").is_err());
    }

    #[test]
    fn quoting_of_segments_and_queries() {
        assert_eq!(segment("a b/c"), "a%20b%2Fc");
        assert_eq!(ref_path("vision/DEMO-12"), "vision/DEMO-12");
        assert_eq!(
            urlencode(&query(&[("head", "acme:vision/DEMO-12"), ("q", "a b")])),
            "head=acme%3Avision%2FDEMO-12&q=a+b"
        );
        assert_eq!(basic("x", "y"), "Basic eDp5");
    }

    #[test]
    fn the_parts_of_an_address() {
        let parts = split("https://user:pw@Example.com:8443/a/b?c=d#e");
        assert_eq!(parts.scheme, "https");
        assert_eq!(parts.user, "user");
        assert!(parts.password);
        assert_eq!(parts.host, "example.com");
        assert_eq!(parts.netloc, "user:pw@Example.com:8443");
        assert_eq!(parts.path, "/a/b");
        assert_eq!(
            (parts.query.as_str(), parts.fragment.as_str()),
            ("c=d", "e")
        );
        assert_eq!(split("/srv/repo.git").scheme, "");
        assert_eq!(split("http://[::1]:80/x").host, "::1");
    }
}
