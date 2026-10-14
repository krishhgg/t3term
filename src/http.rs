//! Authenticated HTTP calls to the T3 server.

use std::time::Duration;

use anyhow::Result;
use serde_json::Value;

use crate::discovery::{PROTOCOL_HEADER, PROTOCOL_VERSION, Runtime};
use crate::error::{err, err_exit, exit};

/// The client of every HTTP request t3term sends a T3 server. It follows no redirect, so a
/// request reaches the origin t3term chose or fails there, and it uses no proxy, so a request
/// for a server on this machine stays on this machine. The WebSocket works the same way:
/// tokio-tungstenite opens it straight to the origin and fails on any answer but 101.
pub(crate) fn direct_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()
        // `reqwest::Client::new` panics on the same failure: a TLS backend or the system's
        // resolver configuration that can't load.
        .expect("an HTTP client")
}

#[derive(Clone)]
pub struct Api {
    pub origin: String,
    token: String,
    http: reqwest::Client,
}

impl Api {
    pub fn new(runtime: &Runtime, token: &str) -> Self {
        Self {
            origin: runtime.origin.clone(),
            token: token.to_string(),
            http: direct_client(),
        }
    }

    /// An `Api` with no token for tests that talk to a fake T3 on 127.0.0.1. Like every `Api`,
    /// it ignores proxy settings, so a proxy in the test's environment can't take a request.
    #[cfg(test)]
    pub(crate) fn without_proxy(origin: &str) -> Self {
        Self {
            origin: origin.to_string(),
            token: String::new(),
            http: direct_client(),
        }
    }

    pub async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value> {
        let mut request = self
            .http
            .request(method.clone(), format!("{}{}", self.origin, path))
            .bearer_auth(&self.token)
            // Orchestration routes reject requests that do not name the protocol; others ignore it.
            .header(PROTOCOL_HEADER, PROTOCOL_VERSION.to_string())
            .timeout(Duration::from_secs(30));
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request.send().await.map_err(|e| {
            err_exit(
                "T3_REQUEST_FAILED",
                exit::UNAVAILABLE,
                format!("{method} {path} failed: {e}"),
            )
        })?;
        let status = response.status();
        if status.is_redirection() {
            // No route t3term calls redirects. At T3 f570bd21 only the web app's pages and the
            // MCP sign-in page do. Following one would send this request, and its body, to a
            // server t3term didn't choose.
            return Err(err_exit(
                "T3_REDIRECTED",
                exit::REJECTED,
                format!(
                    "T3 at {} answered {method} {path} with a redirect. t3term sends a request only to the server it chose, so it stopped.",
                    self.origin
                ),
            ));
        }
        let text = response.text().await.unwrap_or_default();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(err_exit(
                "T3_NOT_FOUND",
                exit::NOT_FOUND,
                format!("T3 has nothing at {path}."),
            ));
        }
        if !status.is_success() {
            let detail = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| {
                    v.get("message")
                        .or_else(|| v.get("error"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .unwrap_or_else(|| text.chars().take(200).collect());
            return Err(err(
                "T3_API_ERROR",
                format!("T3 returned HTTP {status} for {method} {path}: {detail}"),
            ));
        }
        if text.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text).map_err(|_| {
            err(
                "T3_API_ERROR",
                format!("T3 sent unreadable JSON for {path}."),
            )
        })
    }

    pub async fn get(&self, path: &str) -> Result<Value> {
        self.request(reqwest::Method::GET, path, None).await
    }

    pub async fn post(&self, path: &str, body: Option<&Value>) -> Result<Value> {
        self.request(reqwest::Method::POST, path, body).await
    }

    /// A single-use ticket that authenticates one WebSocket connection.
    pub async fn websocket_ticket(&self) -> Result<String> {
        let issued = self.post("/api/auth/websocket-ticket", None).await?;
        issued
            .get("ticket")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .ok_or_else(|| err("T3_AUTH_FAILED", "T3 returned an invalid WebSocket ticket."))
    }
}

/// A fake T3 for unit tests, on 127.0.0.1. It answers each request with the raw HTTP response
/// `respond` gives for the request's path, and records the paths. An empty response closes the
/// connection without an answer.
#[cfg(test)]
pub(crate) mod fake {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    pub(crate) struct Server {
        pub(crate) origin: String,
        paths: Arc<Mutex<Vec<String>>>,
    }

    impl Server {
        /// Starts a server on a free port. Its thread answers until the test process exits.
        pub(crate) fn start(respond: impl Fn(&str) -> String + Send + 'static) -> Server {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let origin = format!("http://{}", listener.local_addr().unwrap());
            let paths = Arc::new(Mutex::new(Vec::new()));
            let seen = paths.clone();
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(mut stream) = stream else { continue };
                    let Ok(reading) = stream.try_clone() else {
                        continue;
                    };
                    let mut reader = BufReader::new(reading);
                    let mut request_line = String::new();
                    if reader.read_line(&mut request_line).is_err() {
                        continue;
                    }
                    // Read up to the blank line that ends the headers. No request a test sends
                    // has a body.
                    let mut header = String::new();
                    while reader.read_line(&mut header).is_ok_and(|read| read > 2) {
                        header.clear();
                    }
                    let path = request_line.split_whitespace().nth(1).unwrap_or_default();
                    let reply = respond(path);
                    seen.lock().unwrap().push(path.to_string());
                    let _ = stream.write_all(reply.as_bytes());
                }
            });
            Server { origin, paths }
        }

        /// The path of each request so far, in order.
        pub(crate) fn paths(&self) -> Vec<String> {
            self.paths.lock().unwrap().clone()
        }
    }

    /// An HTTP/1.1 response with `status`, such as `200 OK`, the `headers` given and `body`.
    /// It closes the connection.
    pub(crate) fn response(status: &str, headers: &[&str], body: &str) -> String {
        let mut text = format!(
            "HTTP/1.1 {status}\r\nconnection: close\r\ncontent-length: {}\r\n",
            body.len()
        );
        for header in headers {
            text.push_str(header);
            text.push_str("\r\n");
        }
        text.push_str("\r\n");
        text.push_str(body);
        text
    }
}
