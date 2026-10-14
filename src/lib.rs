//! Smart-Claude-Router: a local proxy between Claude Code and the Anthropic API that will pick
//! the Claude model and effort per request. Today it is a transparent passthrough.

pub mod classifier;
pub mod classifier_router;
pub mod config;
pub mod conversation;
pub mod ollama;
pub mod proxy;
pub mod rewrite;
pub mod router;
pub mod rules;
pub mod stream;
