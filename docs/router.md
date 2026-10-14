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

## One model per turn

One message from you usually causes several requests. Claude reads a file, gets the content,
runs a command, gets the result, and only then answers. Every one of those requests belongs to
the same turn, and they all have to use the same model. A model can't continue the hidden
reasoning of another model, so switching in the middle of a turn can make Anthropic refuse the
request. It would also throw away the cached conversation.

So the router is asked once per turn:

- **New turn:** your last message has text or an image in it. The router decides, and the proxy
  remembers the answer for this conversation.
- **Same turn:** the last message only carries tool results. The remembered answer is used
  again, and the log shows the reason as `same turn: ...`. A system note after the tool results
  doesn't change this.

A conversation is a Claude Code session. A subagent counts as its own conversation, so its model
is chosen separately from the main one.

If a request continues a turn but nothing is remembered for it, the request is forwarded
exactly as Claude Code sent it. This happens after the proxy restarts in the middle of a turn,
when a conversation was idle for over an hour, and for requests that carry no session id.

The memory only lives in the running proxy. It holds the model, effort and reason for up to
1024 conversations and forgets a conversation after an hour without requests. It never holds
your messages and is never written to disk.

## What changes in a request

When the router chooses a different model or effort than Claude Code asked for, the proxy edits
the request before sending it on:

- the model is replaced
- the effort is replaced where the request already has one. Claude Code writes the effort in two
  places: once for the whole request, and once on the latest system note in the conversation.
  Both are set to the same value. If a request has no effort, none is added.

Everything else is left alone, including how Claude thinks (`thinking`). If the router chooses
what the request already has, nothing is edited and the request goes out as Claude Code sent it.

## What it never does

- It never changes anything in your request except the model and the effort (see below). Every
  other byte of the request is sent to Anthropic exactly as Claude Code wrote it.
- It never blocks or fails a request. A request whose body can't be read as a Claude message,
  such as invalid JSON or no `model`, skips the router and is forwarded exactly as received.
- It is only asked about `POST /v1/messages`. Every other path goes straight through.
- It never sees your login, headers other than the two id headers above, or Claude's answers.

## How to use it and turn it off

Nothing to configure. The passthrough router is always on and changes nothing. Every request
line in the log shows the model Claude Code asked for (`model`), the model the router chose
(`routed`) and why (`reason`). With `--log-level debug`, each `/v1/messages` request also logs a
`routing decision` line with the effort and who decided.

## When something fails

If a request can't be summarised, nothing is logged for it and Claude Code gets its answer as
if the router did not exist.

If the router has decided but the request can't be edited, for example because `output_config`
has an unexpected shape, the proxy logs a warning and forwards the request exactly as received.
Claude Code never sees an error from this.

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

`src/rewrite.rs` has `apply(body, &RoutingDecision) -> Result<Option<Vec<u8>>>`. `Some` is the
new body, `None` means "send the original bytes", `Err` means an unexpected shape (the proxy
warns and sends the original). It splices raw JSON text: each top-level value is kept as its
original text and only the changed values are written again, so numbers, escapes and key order
are never reformatted. The proxy then sets `content-length` to the new size. The per-message
effort is changed on the latest `role: "system"` message that has `output_config.effort`; older
ones are history and stay as they are.

`RequestView::turn` is `Turn::New` or `Turn::Continuation`. Trailing `role: "system"` messages
are skipped; if the last message left is a user message made only of `tool_result` blocks it is a
continuation. Anything else is new, including tool results mixed with text, an image, an
assistant message last, or no messages. Compaction has no marker, so it is a new turn.

`src/conversation.rs` has `Conversations`, which the proxy owns. `plan(&RequestView)` returns
`Plan::Decide` (ask the router, then call `remember`), `Plan::Reuse(decision)` or `Plan::Skip`.
The key is session id plus agent id (`ConversationKey::from_view`, `None` without a session id).
Entries expire after `DEFAULT_TTL` (1 hour, refreshed on use) and the least recently used one
is dropped at `DEFAULT_MAX_ENTRIES` (1024). The lock is a std `Mutex` held only for a map
lookup, never across an `await`. The two limits are constants for now, not settings. The view carries what it needs:
`last_user_text`, `has_tool_use`, `session_id` and `agent_id`.

`tests/router.rs` runs `RequestView::parse` against every file in `tests/fixtures/requests/`.
A new fixture is checked by the "every fixture parses" test without further changes.
