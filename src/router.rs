//! The routing contract: what a router is told about a request, and what it answers.
//!
//! Every way of choosing a model (pass the request through, rules, a local classifier) implements
//! [`Router`]: it gets a [`RequestView`] of a `/v1/messages` request and returns a
//! [`RoutingDecision`]. The proxy builds the view once per request and calls the router. Today
//! the only router is [`PassthroughRouter`], and the decision is only logged, never applied.
//!
//! A view is only a summary. The proxy keeps the original body and forwards it as received.
//! A body that cannot be summarised gives no view, and the request is forwarded as it came.

use std::{borrow::Cow, fmt, future::Future, pin::Pin};

use axum::http::HeaderMap;
use serde::{
    de::{value::Error as ValueError, IgnoredAny, IntoDeserializer},
    Deserialize, Deserializer,
};

use crate::config::Effort;

/// Session header sent by Claude Code on every request (`docs/spike-request-shape.md`).
const SESSION_HEADER: &str = "x-claude-code-session-id";
/// Added on requests made by a subagent, which share the parent's session id.
const AGENT_HEADER: &str = "x-claude-code-agent-id";

/// Boxed so that `dyn Router` works. Implementations can write `Box::pin(async move { ... })`.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait Router: Send + Sync {
    /// Decides the model and effort for one request. Must not fail: when it cannot tell, it
    /// answers with the requested model (see [`PassthroughRouter`]).
    fn decide<'a>(&'a self, req: &'a RequestView) -> BoxFuture<'a, RoutingDecision>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutingDecision {
    pub model: String,
    /// `None` leaves the request's effort as it is.
    pub effort: Option<Effort>,
    /// Short explanation for logs. Never prompt content.
    pub reason: String,
    pub source: DecisionSource,
}

/// Which router made a decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionSource {
    Passthrough,
    Rules,
    Classifier,
}

/// Internal requests Claude Code makes on its own, not on behalf of a user turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundKind {
    /// `max_tokens: 1`, sent to read the quota headers.
    QuotaProbe,
    /// Structured output (`output_config.format`) that asks for a session title.
    TitleGeneration,
}

impl BackgroundKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::QuotaProbe => "quota_probe",
            Self::TitleGeneration => "title_generation",
        }
    }
}

/// Recognises the background requests found in `docs/spike-request-shape.md`. These must reach
/// the upstream untouched, so the proxy does not route them.
///
/// Compaction is not listed: it has no reliable marker and looks like a continuation, which
/// keeps the current model.
pub fn background_kind(req: &RequestView) -> Option<BackgroundKind> {
    if req.max_tokens == Some(1) {
        Some(BackgroundKind::QuotaProbe)
    } else if req.has_output_format {
        Some(BackgroundKind::TitleGeneration)
    } else {
        None
    }
}

/// Returns the requested model and effort unchanged. The default router.
#[derive(Debug, Default, Clone, Copy)]
pub struct PassthroughRouter;

impl Router for PassthroughRouter {
    fn decide<'a>(&'a self, req: &'a RequestView) -> BoxFuture<'a, RoutingDecision> {
        Box::pin(async move {
            RoutingDecision {
                model: req.model.clone(),
                effort: req.effort,
                reason: "passthrough".into(),
                source: DecisionSource::Passthrough,
            }
        })
    }
}

/// Where a request sits in a user turn. One turn is one message from the user and every request
/// Claude Code makes to answer it: after each tool call it sends the result back as a new request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Turn {
    /// The user just wrote something (text or images).
    New,
    /// Claude Code is sending tool results back inside a turn that already started.
    Continuation,
}

/// Trailing `role: "system"` messages are skipped. If the last message left is a user message
/// made only of `tool_result` blocks, the request continues a turn. Anything else, including
/// text mixed with tool results, an image, or no messages at all, starts a new one.
fn turn_of(messages: &[Message]) -> Turn {
    let last = messages.iter().rev().find(|m| m.role != "system");
    match last {
        Some(Message {
            role,
            content: Content::Blocks(blocks),
        }) if role == "user"
            && !blocks.is_empty()
            && blocks.iter().all(|b| b.kind == "tool_result") =>
        {
            Turn::Continuation
        }
        _ => Turn::New,
    }
}

/// Read-only summary of one `POST /v1/messages` request.
#[derive(Clone, PartialEq, Eq)]
pub struct RequestView {
    /// The model Claude Code asked for.
    pub model: String,
    /// Top-level `output_config.effort`. `None` if absent or not a value we know.
    pub effort: Option<Effort>,
    pub max_tokens: Option<u64>,
    /// `x-claude-code-session-id`.
    pub session_id: Option<String>,
    /// `x-claude-code-agent-id`, set on subagent requests only.
    pub agent_id: Option<String>,
    /// Number of entries in `messages`, `role: "system"` ones included.
    pub message_count: usize,
    /// Text blocks of the last `role: "user"` message, joined by newlines. Empty when that
    /// message only holds tool results, which is how a continuation looks. Kept in memory for
    /// the routing decision only; `Debug` hides it.
    pub last_user_text: String,
    /// Whether this request starts a user turn or continues one (see [`Turn`]).
    pub turn: Turn,
    /// Any `tool_use` block in `messages`.
    pub has_tool_use: bool,
    /// Number of tools declared in the request's `tools`.
    pub tool_count: usize,
    /// Number of top-level `image` blocks across `messages`.
    pub image_count: usize,
    /// The request carries `output_config.format` (structured output, used by title generation).
    pub has_output_format: bool,
    /// Size of the request body in bytes.
    pub body_bytes: usize,
}

impl RequestView {
    /// `None` when `body` is not a JSON object with a `model`; the request is then not routed.
    pub fn parse(headers: &HeaderMap, body: &[u8]) -> Option<Self> {
        let raw: RawRequest = serde_json::from_slice(body).ok()?;
        let header = |name: &str| {
            headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        };

        let mut has_tool_use = false;
        let mut image_count = 0;
        let mut last_user_text = String::new();
        for message in &raw.messages {
            let blocks = match &message.content {
                Content::Blocks(blocks) => blocks.as_slice(),
                _ => &[],
            };
            has_tool_use |= blocks.iter().any(|b| b.kind == "tool_use");
            image_count += blocks.iter().filter(|b| b.kind == "image").count();
            if message.role == "user" {
                last_user_text = match &message.content {
                    Content::Text(text) => text.to_string(),
                    Content::Blocks(blocks) => blocks
                        .iter()
                        .filter(|b| b.kind == "text")
                        .filter_map(|b| b.text.as_deref())
                        .collect::<Vec<_>>()
                        .join("\n"),
                    Content::Other(_) => String::new(),
                };
            }
        }

        let output_config = raw.output_config.unwrap_or_default();
        Some(Self {
            model: raw.model?.into_owned(),
            effort: output_config.effort,
            max_tokens: raw.max_tokens,
            session_id: header(SESSION_HEADER),
            agent_id: header(AGENT_HEADER),
            message_count: raw.messages.len(),
            last_user_text,
            turn: turn_of(&raw.messages),
            has_tool_use,
            tool_count: raw.tools.len(),
            image_count,
            has_output_format: output_config.has_format,
            body_bytes: body.len(),
        })
    }
}

/// Never prints `last_user_text`: prompts must not end up in logs.
impl fmt::Debug for RequestView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RequestView")
            .field("model", &self.model)
            .field("effort", &self.effort)
            .field("max_tokens", &self.max_tokens)
            .field("session_id", &self.session_id)
            .field("agent_id", &self.agent_id)
            .field("message_count", &self.message_count)
            .field("last_user_text_chars", &self.last_user_text.chars().count())
            .field("turn", &self.turn)
            .field("has_tool_use", &self.has_tool_use)
            .field("tool_count", &self.tool_count)
            .field("image_count", &self.image_count)
            .field("has_output_format", &self.has_output_format)
            .field("body_bytes", &self.body_bytes)
            .finish()
    }
}

/// The parts of a request body the view needs. Everything else is skipped while parsing, and
/// strings are borrowed from the body when possible.
#[derive(Deserialize)]
struct RawRequest<'a> {
    #[serde(borrow, default)]
    model: Option<Cow<'a, str>>,
    #[serde(default)]
    max_tokens: Option<u64>,
    #[serde(borrow, default)]
    messages: Vec<Message<'a>>,
    #[serde(default)]
    tools: Vec<IgnoredAny>,
    #[serde(default)]
    output_config: Option<OutputConfig>,
}

#[derive(Deserialize)]
struct Message<'a> {
    #[serde(borrow, default)]
    role: Cow<'a, str>,
    #[serde(borrow, default = "no_content")]
    content: Content<'a>,
}

fn no_content<'a>() -> Content<'a> {
    Content::Other(IgnoredAny)
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Content<'a> {
    Text(#[serde(borrow)] Cow<'a, str>),
    Blocks(#[serde(borrow)] Vec<Block<'a>>),
    Other(IgnoredAny),
}

#[derive(Deserialize)]
struct Block<'a> {
    #[serde(rename = "type", borrow, default)]
    kind: Cow<'a, str>,
    #[serde(borrow, default)]
    text: Option<Cow<'a, str>>,
}

#[derive(Default)]
struct OutputConfig {
    effort: Option<Effort>,
    has_format: bool,
}

impl<'de> Deserialize<'de> for OutputConfig {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Fields {
            #[serde(default)]
            effort: Option<String>,
            #[serde(default)]
            format: Option<IgnoredAny>,
        }
        let fields = Fields::deserialize(deserializer)?;
        Ok(Self {
            // An effort value we do not know yet must not make the whole request unroutable.
            effort: fields.effort.and_then(|e| {
                Effort::deserialize(IntoDeserializer::<ValueError>::into_deserializer(e)).ok()
            }),
            has_format: fields.format.is_some(),
        })
    }
}
