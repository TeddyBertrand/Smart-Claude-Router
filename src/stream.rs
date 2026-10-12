//! Read-only access to response bodies while they stream through the proxy.
//!
//! The proxy never buffers responses. A [`Tap`] (usage stats, #24) gets an [`Observer`] per
//! response that sees every chunk exactly as forwarded, still compressed if upstream compressed
//! it, and learns how the stream ended. Observing never changes or delays what the client gets.

use std::{
    pin::Pin,
    task::{ready, Context, Poll},
};

use axum::{
    body::Bytes,
    http::{HeaderMap, Method, StatusCode},
};
use futures_core::Stream;

pub trait Tap: Send + Sync {
    /// Called once per response, before its body streams. Return `None` to skip it.
    fn observe(&self, response: &ResponseInfo<'_>) -> Option<Box<dyn Observer>>;
}

pub trait Observer: Send {
    /// One body chunk, byte for byte what the client receives.
    fn chunk(&mut self, chunk: &[u8]);
    /// Called exactly once, when the body stops for any reason.
    fn end(&mut self, end: StreamEnd);
}

pub struct ResponseInfo<'a> {
    pub method: &'a Method,
    pub path: &'a str,
    pub status: StatusCode,
    pub headers: &'a HeaderMap,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamEnd {
    /// Upstream sent the whole body and the client received it.
    Complete,
    /// Upstream failed mid-body; the client gets a truncated response.
    UpstreamError,
    /// The client went away first (e.g. Esc in Claude Code). Dropping the body also drops the
    /// upstream request, so generation stops.
    ClientGone,
}

type Upstream = Pin<Box<dyn Stream<Item = reqwest::Result<Bytes>> + Send>>;

/// Wraps an upstream body, reporting to an [`Observer`] without altering the items.
pub(crate) struct Observed {
    inner: Upstream,
    observer: Option<Box<dyn Observer>>,
}

impl Observed {
    pub(crate) fn new(
        inner: impl Stream<Item = reqwest::Result<Bytes>> + Send + 'static,
        observer: Box<dyn Observer>,
    ) -> Self {
        Self {
            inner: Box::pin(inner),
            observer: Some(observer),
        }
    }

    fn finish(&mut self, end: StreamEnd) {
        if let Some(mut observer) = self.observer.take() {
            observer.end(end);
        }
    }
}

impl Stream for Observed {
    type Item = reqwest::Result<Bytes>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let item = ready!(self.inner.as_mut().poll_next(cx));
        match &item {
            Some(Ok(chunk)) => {
                if let Some(observer) = self.observer.as_mut() {
                    observer.chunk(chunk);
                }
            }
            Some(Err(_)) => self.finish(StreamEnd::UpstreamError),
            None => self.finish(StreamEnd::Complete),
        }
        Poll::Ready(item)
    }
}

impl Drop for Observed {
    /// The server drops the body without finishing it when the client disconnects.
    fn drop(&mut self) {
        self.finish(StreamEnd::ClientGone);
    }
}
