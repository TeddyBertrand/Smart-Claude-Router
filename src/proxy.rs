//! Transparent HTTP passthrough to the upstream Anthropic API.
//!
//! Every method and path is forwarded unchanged (Claude Code also calls `HEAD /api/hello`, and
//! `/v1/messages` always carries `?beta=true`, see `docs/spike-request-shape.md`). Request bodies
//! are buffered so `content-length` stays exact and later routing can rewrite the JSON. Response
//! bodies stream back chunk by chunk, still compressed: reqwest is built without its
//! decompression features, so gzip-encoded SSE passes through byte for byte.

use std::{sync::Arc, time::Duration};

use axum::{
    body::{to_bytes, Body},
    extract::{Request, State},
    http::{header, HeaderMap, HeaderName, StatusCode},
    response::{IntoResponse, Response},
    Router,
};

/// Largest request body accepted. The Messages API itself rejects requests above 32 MB.
const MAX_REQUEST_BODY: usize = 64 * 1024 * 1024;

#[derive(Clone)]
pub struct Proxy {
    client: reqwest::Client,
    upstream: Arc<str>,
}

impl Proxy {
    /// `upstream` is a base URL such as `https://api.anthropic.com`; the incoming path and query
    /// are appended to it as-is.
    pub fn new(upstream: &str) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            // Redirects belong to the client, not to a transparent proxy.
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            client,
            upstream: upstream.trim_end_matches('/').into(),
        })
    }

    pub fn router(self) -> Router {
        Router::new().fallback(forward).with_state(self)
    }

    async fn forward(&self, req: Request) -> Result<Response, ForwardError> {
        let (parts, body) = req.into_parts();
        let path_and_query = parts.uri.path_and_query().map_or("/", |pq| pq.as_str());
        let url = format!("{}{}", self.upstream, path_and_query);
        let body = to_bytes(body, MAX_REQUEST_BODY)
            .await
            .map_err(|_| ForwardError::Body)?;

        let mut request = self
            .client
            .request(parts.method, url)
            .headers(strip_hop_by_hop(&parts.headers));
        if !body.is_empty() {
            request = request.body(body);
        }
        let upstream = request.send().await.map_err(ForwardError::Upstream)?;

        let status = upstream.status();
        let headers = strip_hop_by_hop(upstream.headers());
        let mut response = Response::new(Body::from_stream(upstream.bytes_stream()));
        *response.status_mut() = status;
        *response.headers_mut() = headers;
        Ok(response)
    }
}

async fn forward(State(proxy): State<Proxy>, req: Request) -> Response {
    proxy
        .forward(req)
        .await
        .unwrap_or_else(IntoResponse::into_response)
}

enum ForwardError {
    Body,
    Upstream(reqwest::Error),
}

impl IntoResponse for ForwardError {
    fn into_response(self) -> Response {
        match self {
            Self::Body => (
                StatusCode::PAYLOAD_TOO_LARGE,
                format!(
                    "smart-claude-router: request body unreadable or over {MAX_REQUEST_BODY} bytes"
                ),
            )
                .into_response(),
            Self::Upstream(err) => {
                tracing::warn!(error = %err, "upstream request failed");
                (
                    StatusCode::BAD_GATEWAY,
                    "smart-claude-router: upstream request failed",
                )
                    .into_response()
            }
        }
    }
}

/// Copies `headers` minus hop-by-hop ones (RFC 9110 §7.6.1) and `host`, which reqwest sets
/// from the upstream URL. Everything else, auth included, is forwarded untouched.
fn strip_hop_by_hop(headers: &HeaderMap) -> HeaderMap {
    let mut out = HeaderMap::with_capacity(headers.len());
    for (name, value) in headers {
        if !is_hop_by_hop(name) {
            out.append(name.clone(), value.clone());
        }
    }
    out
}

fn is_hop_by_hop(name: &HeaderName) -> bool {
    *name == header::HOST
        || matches!(
            name.as_str(),
            "connection"
                | "keep-alive"
                | "proxy-connection"
                | "proxy-authenticate"
                | "proxy-authorization"
                | "te"
                | "trailer"
                | "transfer-encoding"
                | "upgrade"
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn strips_hop_by_hop_and_host_but_keeps_auth() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("127.0.0.1:8899"));
        headers.insert(header::CONNECTION, HeaderValue::from_static("keep-alive"));
        headers.insert("keep-alive", HeaderValue::from_static("timeout=5"));
        headers.insert(
            header::TRANSFER_ENCODING,
            HeaderValue::from_static("chunked"),
        );
        headers.insert(header::AUTHORIZATION, HeaderValue::from_static("Bearer t"));
        headers.insert("anthropic-beta", HeaderValue::from_static("a,b"));
        headers.append("x-multi", HeaderValue::from_static("1"));
        headers.append("x-multi", HeaderValue::from_static("2"));

        let out = strip_hop_by_hop(&headers);

        let names: Vec<_> = out.keys().map(HeaderName::as_str).collect();
        assert_eq!(names, ["authorization", "anthropic-beta", "x-multi"]);
        assert_eq!(out.get_all("x-multi").iter().count(), 2);
    }
}
