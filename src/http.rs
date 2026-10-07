//! Authenticated HTTP calls to the T3 server.

use std::time::Duration;

use anyhow::Result;
use serde_json::Value;

use crate::discovery::{PROTOCOL_HEADER, PROTOCOL_VERSION, Runtime};
use crate::error::{err, err_exit, exit};

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
            http: reqwest::Client::new(),
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
