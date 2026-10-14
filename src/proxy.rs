//! Transparent HTTP passthrough to the upstream Anthropic API.
//!
//! Every method and path is forwarded unchanged (Claude Code also calls `HEAD /api/hello`, and
//! `/v1/messages` always carries `?beta=true`, see `docs/spike-request-shape.md`). Request bodies
//! are buffered so `content-length` stays exact and later routing can rewrite the JSON. Response
//! bodies stream back chunk by chunk, still compressed: reqwest is built without its
//! decompression features, so gzip-encoded SSE passes through byte for byte. When the client
//! disconnects, the response body is dropped, which drops the upstream request with it.
//!
//! Each request is logged once at `info`: method, path (never the query), status, time until the
//! upstream answered, the requested model, and for routed requests the model it was routed to and
//! why. Headers and bodies are never logged.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    body::{to_bytes, Body},
    extract::{Request, State},
    http::{header, HeaderMap, HeaderName, Method, StatusCode},
    response::{IntoResponse, Response},
    Router,
};

use crate::{
    conversation::{Conversations, Plan},
    rewrite,
    router::{background_kind, PassthroughRouter, RequestView, Router as RouteDecider},
    stream::{Observed, ResponseInfo, Tap},
};

/// Largest request body accepted. The Messages API itself rejects requests above 32 MB.
const MAX_REQUEST_BODY: usize = 64 * 1024 * 1024;

#[derive(Clone)]
pub struct Proxy {
    client: reqwest::Client,
    upstream: Arc<str>,
    tap: Option<Arc<dyn Tap>>,
    router: Arc<dyn RouteDecider>,
    conversations: Arc<Conversations>,
    dry_run: bool,
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
            tap: None,
            router: Arc::new(PassthroughRouter),
            conversations: Arc::default(),
            dry_run: false,
        })
    }

    /// In dry-run mode the whole routing pipeline runs and each decision is logged, but every
    /// request is forwarded exactly as received.
    pub fn with_dry_run(mut self, dry_run: bool) -> Self {
        self.dry_run = dry_run;
        self
    }

    /// Lets `tap` observe every response body as it streams (see [`crate::stream`]).
    pub fn with_tap(mut self, tap: Arc<dyn Tap>) -> Self {
        self.tap = Some(tap);
        self
    }

    /// Replaces the default [`PassthroughRouter`]. Its decision is applied to `/v1/messages`
    /// requests (see [`crate::rewrite`]).
    pub fn with_router(mut self, router: Arc<dyn RouteDecider>) -> Self {
        self.router = router;
        self
    }

    pub fn router(self) -> Router {
        Router::new().fallback(forward).with_state(self)
    }

    /// Asks the router about a `/v1/messages` request and applies its decision to `body`. Returns
    /// `None` when the request is not routed (other path, unreadable body, background request).
    /// Never fails: whatever goes wrong, the original body is forwarded.
    async fn route(&self, headers: &HeaderMap, body: &[u8]) -> Option<Routed> {
        let view = RequestView::parse(headers, body)?;
        if let Some(kind) = background_kind(&view) {
            tracing::debug!(kind = kind.as_str(), "background request, not routed");
            return None;
        }
        let decision = match self.conversations.plan(&view) {
            Plan::Decide => {
                let decision = self.router.decide(&view).await;
                self.conversations.remember(&view, &decision);
                decision
            }
            Plan::Reuse(mut decision) => {
                decision.reason = format!("same turn: {}", decision.reason);
                decision
            }
            Plan::Skip => {
                tracing::debug!("continuation without a remembered decision, not routed");
                return None;
            }
        };
        tracing::debug!(
            model = %decision.model,
            effort = ?decision.effort,
            source = ?decision.source,
            reason = %decision.reason,
            "routing decision"
        );
        if self.dry_run {
            tracing::info!(
                requested = %loggable_model(&view.model),
                would_route = %loggable_model(&decision.model),
                effort = %decision.effort.map_or("-", |e| e.as_str()),
                reason = %decision.reason,
                turn = %view.turn.as_str(),
                "dry run: would route"
            );
            return None;
        }
        let body = match rewrite::apply(body, &decision) {
            Ok(body) => body,
            Err(err) => {
                tracing::warn!(error = %err, "cannot apply routing decision, forwarding unchanged");
                None
            }
        };
        Some(Routed {
            model: decision.model,
            reason: decision.reason,
            body,
        })
    }

    /// `log` is filled in for the request log: the requested model, if the body names one, and
    /// the routing outcome.
    async fn forward(&self, req: Request, log: &mut RequestLog) -> Result<Response, ForwardError> {
        let (parts, body) = req.into_parts();
        let path_and_query = parts.uri.path_and_query().map_or("/", |pq| pq.as_str());
        let url = format!("{}{}", self.upstream, path_and_query);
        let mut body = to_bytes(body, MAX_REQUEST_BODY)
            .await
            .map_err(|_| ForwardError::Body)?;
        log.model = requested_model(&body);
        let mut headers = strip_hop_by_hop(&parts.headers);
        if parts.method == Method::POST && parts.uri.path() == "/v1/messages" {
            if let Some(routed) = self.route(&parts.headers, &body).await {
                if let Some(new_body) = routed.body {
                    headers.insert(header::CONTENT_LENGTH, new_body.len().into());
                    body = new_body.into();
                }
                log.routed = Some((routed.model, routed.reason));
            }
        }

        let mut request = self
            .client
            .request(parts.method.clone(), url)
            .headers(headers);
        if !body.is_empty() {
            request = request.body(body);
        }
        let upstream = request.send().await.map_err(ForwardError::Upstream)?;

        let status = upstream.status();
        let headers = strip_hop_by_hop(upstream.headers());
        let observer = self
            .tap
            .as_ref()
            // A HEAD body is never polled, so it would always look like `ClientGone`.
            .filter(|_| parts.method != Method::HEAD)
            .and_then(|tap| {
                tap.observe(&ResponseInfo {
                    method: &parts.method,
                    path: parts.uri.path(),
                    status,
                    headers: &headers,
                })
            });
        let body = match observer {
            Some(observer) => Body::from_stream(Observed::new(upstream.bytes_stream(), observer)),
            None => Body::from_stream(upstream.bytes_stream()),
        };
        let mut response = Response::new(body);
        *response.status_mut() = status;
        *response.headers_mut() = headers;
        Ok(response)
    }
}

async fn forward(State(proxy): State<Proxy>, req: Request) -> Response {
    let started = Instant::now();
    let method = req.method().clone();
    let path = req.uri().path().to_owned();
    let mut log = RequestLog::default();
    let response = proxy
        .forward(req, &mut log)
        .await
        .unwrap_or_else(IntoResponse::into_response);
    let (routed, reason) = match &log.routed {
        Some((model, reason)) => (
            plausible_model(model).then_some(model.as_str()),
            Some(reason.as_str()),
        ),
        None => (None, None),
    };
    tracing::info!(
        %method,
        %path,
        status = response.status().as_u16(),
        ms = started.elapsed().as_millis() as u64,
        model = %log.model.as_deref().unwrap_or("-"),
        routed = %routed.unwrap_or("-"),
        reason = %reason.unwrap_or("-"),
        "request"
    );
    response
}

/// What the request log says about one request.
#[derive(Default)]
struct RequestLog {
    /// The model Claude Code asked for.
    model: Option<String>,
    /// Model and reason of the routing decision. `None` if the request was not routed.
    routed: Option<(String, String)>,
}

/// The outcome of routing one request.
struct Routed {
    model: String,
    reason: String,
    /// The rewritten body, if the decision changed anything.
    body: Option<Vec<u8>>,
}

/// The top-level `model` of a JSON body, if it looks like a model id. Anything else (no JSON,
/// no `model`, odd characters) gives `None`, so the log never carries arbitrary body text.
fn requested_model(body: &[u8]) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct Peek {
        model: Option<String>,
    }
    let model = serde_json::from_slice::<Peek>(body).ok()?.model?;
    plausible_model(&model).then_some(model)
}

fn plausible_model(model: &str) -> bool {
    !model.is_empty()
        && model.len() <= 100
        && model
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':' | '@'))
}

/// `model` if it looks like a model id, else `-`, so logs never carry arbitrary text.
fn loggable_model(model: &str) -> &str {
    if plausible_model(model) {
        model
    } else {
        "-"
    }
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

    #[test]
    fn requested_model_only_accepts_model_ids() {
        let model = |body: &[u8]| requested_model(body);
        assert_eq!(
            model(br#"{"model":"claude-opus-5-5","messages":[]}"#).as_deref(),
            Some("claude-opus-5-5")
        );
        assert_eq!(model(b""), None);
        assert_eq!(model(b"not json"), None);
        assert_eq!(model(br#"{"messages":[]}"#), None);
        assert_eq!(model(br#"{"model":"x\nFAKE LOG LINE"}"#), None);
    }
}
