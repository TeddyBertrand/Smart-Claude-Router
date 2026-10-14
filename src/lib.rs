//! Smart-Claude-Router: a local proxy between Claude Code and the Anthropic API that will pick
//! the Claude model and effort per request. Today it is a transparent passthrough.

pub mod config;
pub mod conversation;
pub mod proxy;
pub mod rewrite;
pub mod router;
pub mod stream;
