# The router

## What it is

The router is the part of Smart-Claude-Router that answers one question for each message Claude
Code sends: which Claude model should handle this, and how hard should it think?

It does not decide anything smart yet. The only router today is the passthrough router. It
always answers "the model Claude Code asked for, with the effort Claude Code asked for". The
rules (#15) and the local model (v0.3.0) will give different answers to the same question.

## Why it exists

Rules and a local model both have to answer the same question in the same format. Defining the
question and the answer first means the proxy has one place to ask, whichever one is plugged in.

## What the router is told

For each request to `/v1/messages`, the proxy gives the router a short summary of it:

- the requested model and effort, and the `max_tokens` limit
- the session and subagent ids Claude Code sends in its headers
- how many messages the conversation has
- the text of the last message you wrote (empty when the last message only carries tool results)
- whether the conversation contains tool calls, how many tools are available, and how many
  images it contains
- whether the request asks for a structured answer (Claude Code does this to write session titles)
- the size of the request in bytes

The summary is only held in memory while the router decides. It is never logged and never
stored. Printing it for debugging shows the length of your last message, not the text.

## What the router answers

- the model to use
- the effort to use, or none, which means "leave it as Claude Code set it"
- a short reason, for the logs
- who decided: passthrough, rules or classifier

## Requests the router skips

Claude Code also sends small requests of its own that no user asked for. The router recognises
two of them and does not look at them at all:

- the quota check, a request that may only produce one token
- the session title, a request that asks for a structured answer

They are forwarded exactly as Claude Code sent them, with the model and effort Claude Code chose.
With `--log-level debug`, each one logs `background request, not routed` and its kind.

Compaction (`/compact`) is not on this list. It looks the same as a normal message, so it is
treated as one: it stays on the model the conversation already uses.

## What it never does

- It never changes your request. For now the answer is only written to the debug log. Applying
  it comes with #13.
- It never blocks or fails a request. A request whose body can't be read as a Claude message,
  such as invalid JSON or no `model`, skips the router and is forwarded exactly as received.
- It is only asked about `POST /v1/messages`. Every other path goes straight through.
- It never sees your login, headers other than the two id headers above, or Claude's answers.

## How to use it and turn it off

Nothing to configure. The passthrough router is always on and changes nothing. To see its
answers, start the proxy with `--log-level debug`. Each `/v1/messages` request then logs one
extra `routing decision` line with the model, effort, source and reason.

## When something fails

If a request can't be summarised, nothing is logged for it and Claude Code gets its answer as
if the router did not exist.

## For developers

The code lives in `src/router.rs`:

- `Router` has one method, `decide(&RequestView) -> BoxFuture<RoutingDecision>`. It returns a
  boxed future so that the proxy can hold an `Arc<dyn Router>`; implementations write
  `Box::pin(async move { ... })`.
- `RequestView::parse(&HeaderMap, &[u8]) -> Option<RequestView>` reads only the fields it needs
  and borrows strings from the body. `None` means "don't route this request". An `effort` value
  it doesn't know becomes `None` instead of making the whole request unroutable.
- `RoutingDecision { model, effort, reason, source }`. `effort: None` means "keep the request's
  own effort". `reason` must never contain prompt text.
- `PassthroughRouter` is the default. `Proxy::with_router` swaps it.

`background_kind(&RequestView) -> Option<BackgroundKind>` recognises the quota probe
(`max_tokens == 1`) and title generation (`output_config.format` present). The proxy calls it
before `decide` and skips the router when it returns `Some`. Add new background kinds there.

Turn detection (new turn or continuation, #14) is not done here. The view carries what it needs:
`last_user_text`, `has_tool_use`, `session_id` and `agent_id`.

`tests/router.rs` runs `RequestView::parse` against every file in `tests/fixtures/requests/`.
A new fixture is checked by the "every fixture parses" test without further changes.
