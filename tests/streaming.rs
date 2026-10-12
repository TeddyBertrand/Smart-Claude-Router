//! Streaming through the proxy: chunks reach the client as upstream sends them, a client
//! disconnect cancels the upstream request, and a tap sees the body without changing it.

mod common;

use std::{
    convert::Infallible,
    io,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::{
    body::{Body, Bytes},
    extract::State,
    response::Response,
    Router,
};
use common::serve;
use futures_util::stream;
use smart_claude_router::{
    proxy::Proxy,
    stream::{Observer, ResponseInfo, StreamEnd, Tap},
};
use tokio::sync::{mpsc, oneshot};

const WAIT: Duration = Duration::from_secs(5);

/// Tap that records everything it is shown.
#[derive(Clone, Default)]
struct Recorder(Arc<Mutex<Recorded>>);

#[derive(Default)]
struct Recorded {
    observed: usize,
    bytes: Vec<u8>,
    end: Option<StreamEnd>,
}

impl Tap for Recorder {
    fn observe(&self, _: &ResponseInfo<'_>) -> Option<Box<dyn Observer>> {
        self.0.lock().unwrap().observed += 1;
        Some(Box::new(self.clone()))
    }
}

impl Observer for Recorder {
    fn chunk(&mut self, chunk: &[u8]) {
        self.0.lock().unwrap().bytes.extend_from_slice(chunk);
    }

    fn end(&mut self, end: StreamEnd) {
        self.0.lock().unwrap().end = Some(end);
    }
}

impl Recorder {
    async fn wait_end(&self) -> StreamEnd {
        tokio::time::timeout(WAIT, async {
            loop {
                if let Some(end) = self.0.lock().unwrap().end {
                    return end;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("observer never saw the stream end")
    }
}

/// Starts `upstream` and a tapped proxy in front of it; returns the proxy base URL.
async fn proxy_to(upstream: Router, tap: &Recorder) -> String {
    let upstream = serve(upstream).await;
    let proxy = Proxy::new(&format!("http://{upstream}"))
        .unwrap()
        .with_tap(Arc::new(tap.clone()));
    format!("http://{}", serve(proxy.router()).await)
}

type Slot<T> = Arc<Mutex<Option<T>>>;

/// Sends `true` when dropped: tells the test the upstream body was abandoned.
struct DropSignal(Option<oneshot::Sender<()>>);

impl Drop for DropSignal {
    fn drop(&mut self) {
        if let Some(tx) = self.0.take() {
            let _ = tx.send(());
        }
    }
}

#[tokio::test]
async fn chunks_reach_client_before_upstream_finishes() {
    // Upstream body = whatever the test pushes into the channel, ending when it is dropped.
    async fn upstream(State(rx): State<Slot<mpsc::Receiver<Bytes>>>) -> Response {
        let rx = rx.lock().unwrap().take().unwrap();
        Response::new(Body::from_stream(stream::unfold(rx, |mut rx| async move {
            rx.recv()
                .await
                .map(|chunk| (Ok::<_, Infallible>(chunk), rx))
        })))
    }
    let (tx, rx) = mpsc::channel(4);
    let tap = Recorder::default();
    let proxy = proxy_to(
        Router::new()
            .fallback(upstream)
            .with_state(Arc::new(Mutex::new(Some(rx)))),
        &tap,
    )
    .await;

    let mut resp = reqwest::Client::new()
        .post(format!("{proxy}/v1/messages"))
        .send()
        .await
        .unwrap();

    // The second chunk is only sent after the first one has arrived: a buffering proxy would
    // never deliver the first one and the timeout would fire.
    tx.send(Bytes::from_static(b"event: one\n\n"))
        .await
        .unwrap();
    let sent = Instant::now();
    let first = tokio::time::timeout(WAIT, resp.chunk())
        .await
        .expect("first chunk was held back")
        .unwrap()
        .unwrap();
    let latency = sent.elapsed();
    assert_eq!(first, "event: one\n\n");
    assert!(
        latency < Duration::from_secs(1),
        "first chunk took {latency:?}"
    );

    tx.send(Bytes::from_static(b"event: two\n\n"))
        .await
        .unwrap();
    drop(tx);
    let mut rest = Vec::new();
    while let Some(chunk) = resp.chunk().await.unwrap() {
        rest.extend_from_slice(&chunk);
    }
    assert_eq!(rest, b"event: two\n\n");

    assert_eq!(tap.wait_end().await, StreamEnd::Complete);
    assert_eq!(tap.0.lock().unwrap().bytes, b"event: one\n\nevent: two\n\n");
}

#[tokio::test]
async fn client_disconnect_cancels_upstream() {
    // Upstream streams forever and signals when its body gets dropped.
    async fn upstream(State(signal): State<Slot<oneshot::Sender<()>>>) -> Response {
        let signal = DropSignal(signal.lock().unwrap().take());
        Response::new(Body::from_stream(stream::unfold(
            signal,
            |signal| async move {
                tokio::time::sleep(Duration::from_millis(20)).await;
                Some((
                    Ok::<_, Infallible>(Bytes::from_static(b"data: tick\n\n")),
                    signal,
                ))
            },
        )))
    }
    let (dropped_tx, dropped_rx) = oneshot::channel();
    let tap = Recorder::default();
    let proxy = proxy_to(
        Router::new()
            .fallback(upstream)
            .with_state(Arc::new(Mutex::new(Some(dropped_tx)))),
        &tap,
    )
    .await;

    let mut resp = reqwest::Client::new()
        .post(format!("{proxy}/v1/messages"))
        .send()
        .await
        .unwrap();
    resp.chunk().await.unwrap().expect("stream started");
    drop(resp); // what Esc in Claude Code does

    tokio::time::timeout(WAIT, dropped_rx)
        .await
        .expect("upstream kept streaming after the client left")
        .unwrap();
    assert_eq!(tap.wait_end().await, StreamEnd::ClientGone);
}

#[tokio::test]
async fn upstream_failure_mid_body_is_reported() {
    // First chunk, then a failure once headers and that chunk are on the wire. Failing
    // immediately would abort before the headers and the proxy would answer 502 instead.
    async fn upstream() -> Response {
        Response::new(Body::from_stream(stream::unfold(0, |step| async move {
            match step {
                0 => Some((Ok(Bytes::from_static(b"event: one\n\n")), 1)),
                1 => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    Some((Err(io::Error::other("upstream died")), 2))
                }
                _ => None,
            }
        })))
    }
    let tap = Recorder::default();
    let proxy = proxy_to(Router::new().fallback(upstream), &tap).await;

    let resp = reqwest::Client::new()
        .post(format!("{proxy}/v1/messages"))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    assert!(
        resp.bytes().await.is_err(),
        "client must see a truncated body"
    );

    assert_eq!(tap.wait_end().await, StreamEnd::UpstreamError);
}

#[tokio::test]
async fn head_requests_are_not_observed() {
    async fn upstream() -> &'static str {
        ""
    }
    let tap = Recorder::default();
    let proxy = proxy_to(Router::new().fallback(upstream), &tap).await;

    let resp = reqwest::Client::new()
        .head(format!("{proxy}/api/hello"))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    assert_eq!(tap.0.lock().unwrap().observed, 0);
}
