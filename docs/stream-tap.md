# The stream tap

## What it is

Every answer from Anthropic passes through the proxy in small pieces while Claude is still
writing it. The stream tap lets another part of Smart-Claude-Router look at those pieces as they
go by. It can read them, but it cannot change them, hold them back or slow them down.

The tap is not used yet. It exists so the usage stats (#24) can be added later without
touching how answers are forwarded.

## Why it exists

The end of each answer from Anthropic contains its token counts: how much text was read, how
much was written, and how much came from the cache. The response headers also include how much
of your 5-hour and 7-day quota is used. To show how much quota the router saves, the proxy has
to read that information. It can't wait for the full answer to do so, because that would delay
every answer.

The tap solves this: the answer goes to Claude Code piece by piece as before, and the stats code
reads a copy of each piece at the same time.

## What it guarantees

- Claude Code receives exactly the same bytes, at the same moment, whether a tap is attached or
  not.
- The tap sees every piece exactly as it was forwarded. Anthropic compresses its answers (gzip),
  so the tap sees compressed bytes. Code that reads token counts has to decompress its own copy.
- The tap is told exactly once how the answer ended. It is never left without an ending.

## How an answer can end

| Ending | What happened |
|---|---|
| Complete | Anthropic sent the whole answer and Claude Code received it. |
| Upstream error | Anthropic's connection broke partway. Claude Code got a cut-off answer. |
| Client gone | Claude Code stopped listening first, usually because you pressed Esc. The proxy has already cancelled the request to Anthropic. |

For a "client gone" answer, the final token counts never arrive, because they come at the end of
the answer. The stats code will have to record these answers as incomplete.

## What it doesn't look at

- `HEAD` requests, such as the connectivity check Claude Code sends at startup. They never
  have a body to read.
- Requests the proxy refuses or can't forward. Those get an error from the proxy itself and
  never reach Anthropic.

## For developers

The code lives in `src/stream.rs`:

- `Tap` is attached once with `Proxy::with_tap`. For each response it receives the method, path,
  status and headers, and decides whether to watch it by returning an `Observer` or `None`.
- `Observer` gets `chunk(&[u8])` for every piece and `end(StreamEnd)` exactly once.
- `StreamEnd` is `Complete`, `UpstreamError` or `ClientGone`.

Both `chunk()` and `end()` run inside the request's streaming path, so they must be quick and
must not block. Heavier work, such as writing stats to disk, should be handed to a background
task (for example by sending the collected numbers over a channel from `end()`).
