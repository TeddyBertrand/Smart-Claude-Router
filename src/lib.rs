//! Smart-Claude-Router: a local proxy between Claude Code and the Anthropic API that will pick
//! the Claude model and effort per request. Today it is a transparent passthrough.

pub mod config;
pub mod proxy;
pub mod router;
pub mod stream;
