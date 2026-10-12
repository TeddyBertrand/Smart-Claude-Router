# Spike: Claude Code request shape through a proxy (#6)

Captured on 2026-10-12 with Claude Code **2.1.296**, subscription (OAuth) login, through a
logging reverse proxy (`mitmdump` in reverse mode → `https://api.anthropic.com`). The
logging proxy redacted everything in memory before writing it to disk. No prompt text, token or
id was stored. Redacted request fixtures are in `tests/fixtures/requests/`.

## In plain words

The post office accepts letters that pass through our sorting room. Claude Code works normally
with a regular Claude subscription when it is pointed at a local proxy, so the project's core
idea is viable.

We also opened the envelopes (without reading the letters) and learned:

- **The delivery-speed sticker.** The model is one field (`model`). The effort level, how hard
  Claude thinks, is another field (`output_config.effort`). It also appears inside the
  conversation itself. To change effort the router must edit both places.
- **Each conversation has a name tag.** Every request carries a session id header. Requests made
  by a subagent carry an extra "agent" tag, so they can be told apart.
- **One question, many trips.** A single question you ask can produce several requests: the
  model asks to read a file, gets the result, then answers. We can tell a fresh question from a
  continuation trip by looking at the last real message. The conversation often ends with a
  hidden "system note" that has to be skipped first.
- **Switching models has a real price.** Switching to Haiku mid-conversation re-read the whole
  71k-token conversation from scratch instead of using the cached copy. This confirms the
  "don't switch cashiers mid-checkout" concern in #22.
- **Your quota meter is on every reply.** Each response says what fraction of your 5-hour and
  7-day limits is used. The stats feature (#24) can report real numbers instead of estimates.
- **Small internal errands exist:** a startup ping, a quota probe and title generation. Title
  generation runs on the expensive main model.
- **Replies arrive compressed (gzip), even while streaming.** The proxy has to handle that.

## Method

| Scenario | How | What it exercised |
|---|---|---|
| S1 chat | `claude -p "Reply with exactly the word ok."` | auth through proxy, base request |
| S2 tool loop | `-p` + Read and `Bash(wc:*)` | `tool_use` → `tool_result` continuation |
| S3 subagent | `-p` + Agent tool | subagent requests, trailing system message |
| S4 model/effort | `--model claude-haiku-5-5`, `--effort low`, `--effort max` | where model and effort live |
| Interactive | real TUI session: question, Esc mid-answer, `/model` → Haiku, `/compact` | title, quota probe, interrupt, model switch, compaction |

All headless runs exited 0 with `stop_reason: end_turn`. The interactive session behaved normally.

## Findings per acceptance criterion

### Works end to end with subscription auth: yes

- `ANTHROPIC_BASE_URL=http://127.0.0.1:<port>` is honored and plain `http://` is accepted.
- The OAuth bearer token is sent to the proxy and forwarding it unchanged works. The
  `anthropic-beta` header includes `oauth-2025-04-20`.
- `user-agent: claude-cli/2.1.296 (external, cli)` for the TUI, `(external, sdk-cli)` for `-p`.

### Endpoints

| Method + path | When | Notes |
|---|---|---|
| `HEAD /api/hello` | session start | connectivity check, empty body, no `authorization` header |
| `POST /v1/messages?beta=true` | everything else | always the `?beta=true` query |

`/v1/messages/count_tokens` was **not** observed. The proxy must still forward every path
unchanged (#7).

### Fields controlling model, effort and thinking

Top-level keys of a normal turn: `model`, `messages`, `system`, `tools`, `metadata`,
`max_tokens` (128000), `thinking`, `context_management`, `output_config`, `stream` (true), plus
`safeguards` in interactive sessions.

- **Model:** `model`, e.g. `claude-opus-5-5`, `claude-haiku-5-5`. The SSE `message_start` echoes
  the model that actually served the request.
- **Effort:** `output_config.effort`. Values seen: `low`, `medium`, `xhigh`, `max`. The CLI also
  accepts `high`. Defaults: `xhigh` on Opus, `medium` on Haiku and for title generation.
  - Effort is **also** repeated on mid-conversation `role: "system"` messages
    (`{"role": "system", "output_config": {"effort": ...}, "content": ...}`). These come from the
    `mid-conversation-system-2026-04-07` / `per-turn-control-2026-07-01` betas. After the
    `/model` switch, a new system message carried the new effort.
  - **Rewriting effort (#13) must update the top-level value and the latest per-message
    `output_config`.**
- **Thinking:** `{"type": "adaptive", "display": "omitted" | "updates"}`. There is no token
  budget; effort is the knob. It is absent on title generation and the quota probe.
  `context_management.edits` contains `clear_thinking_20251015`.
- **Cache:** the main thread marks its system blocks `cache_control: {type: ephemeral, ttl: 1h}`.
  Subagents use the default 5-minute cache. Compaction also dropped to the 5-minute cache.

### Background / side requests

| Kind | Signature | Model | Fixture |
|---|---|---|---|
| Startup ping | `HEAD /api/hello` | – | – |
| Quota probe | `max_tokens: 1`, one user message with string content, no `system`/`tools`/`stream` | main model | `background-quota-probe.json` |
| Title generation | `tools: []`, no `thinking`, `output_config.format` = JSON schema with a `title` property, beta `structured-outputs-2025-12-15` | **main model** (Opus), effort `medium` | `background-title.json` |
| Compaction (`/compact`) | Looks like a normal turn: same model and session, an extra text block on the last user message, trailing system message with string content, system cache TTL drops from 1h to 5m | current main model | `compaction.json` |

- In this session the quota probe returned **429 `rate_limit_error`** with `x-should-retry: true`
  and no rate-limit headers. Claude Code carried on normally. Pass it through unchanged.
- Title generation runs on the expensive main model for about 1.2k input / 16 output tokens. It
  is a possible later optimization (route it to Haiku), but #16 keeps it untouched for now.
- Compaction has **no reliable structural marker**. Treating it as a continuation (keep the
  current model and its cache) is the safe default. Compaction read 71.6k tokens from cache on
  the current model.

### Per-conversation identifier: yes

- Header `x-claude-code-session-id: <uuid>` on every request, including side requests.
- `metadata.user_id` is a JSON string `{"device_id", "account_uuid", "session_id"}`.
- Subagent requests add `x-claude-code-agent-id: <17 hex chars>` and keep the same session id.
- Not verified: whether the header and `metadata.user_id.session_id` are equal (both values
  were masked). Check this in code during #7/#14.
- **Conversation key for #14/#22:** `x-claude-code-session-id` + `x-claude-code-agent-id`
  (if present). No hashing of prompts is needed.

### Turn structure (for #14)

- The first user message has several text blocks (context reminders plus the prompt), followed
  by a `role: "system"` message.
- Continuations end with a user message made only of `tool_result` blocks. A trailing
  `role: "system"` message can follow it (S3).
- **Rule:** skip trailing `system` messages. If the last remaining message is a user message
  with any `text` block (or string content), it is a **new turn**. If it has only
  `tool_result` blocks, it is a **continuation**.
- Assistant `thinking` blocks carry a `signature`, which confirms why a model switch mid-loop
  is unsafe.

### Streaming and usage (for #8, #24)

- The response is `text/event-stream` with **`content-encoding: gzip`**. The proxy must
  forward bytes and headers as-is. The usage tap has to gunzip a copy, or the proxy could
  request identity encoding upstream.
- Events: `message_start` → (`content_block_start` / `ping` / `content_block_delta` /
  `content_block_stop`)* → `message_delta` → `message_stop`.
- `message_start.message.usage`: `input_tokens`, `cache_creation_input_tokens`,
  `cache_read_input_tokens`, `cache_creation.{ephemeral_5m,ephemeral_1h}_input_tokens`,
  `service_tier`.
- `message_delta.usage`: final cumulative counts including `output_tokens`,
  `output_tokens_details.thinking_tokens` and `iterations[]`.
- **Quota headers on every response:** `anthropic-ratelimit-unified-5h-utilization`,
  `-7d-utilization` (0–1), `-5h-status` / `-7d-status`, `-5h-reset` / `-7d-reset` (unix time),
  `-representative-claim`, `-fallback-percentage`, `-overage-status`.

### Interrupt (Esc)

- The client closes the connection mid-stream and the logging proxy closed the upstream
  connection. The partial stream has `message_start` but **no `message_delta`**, so final
  output tokens are unknown. #24 must record these as partial.
- Observed quirk: right before the interrupted request, an identical request was sent and
  dropped by the client after about 500 ms, before any response. The cause is unknown and it
  hasn't been reproduced yet. The proxy just needs to handle client-side aborts cleanly.

## Implications per issue

- **#7 passthrough:** forward all paths (`HEAD /api/hello` included). Keep `?beta=true`.
  Forward `content-encoding` untouched.
- **#8 streaming:** gzip SSE. The tap needs its own decompression. Client abort must cancel
  upstream.
- **#12 RequestView:** include the session id and agent id headers, `output_config`, trailing
  system messages, and `output_config.format` presence.
- **#13 rewrite:** `model` plus `output_config.effort` at top level and on the latest
  per-message `output_config`. Leave `thinking` alone (it is adaptive).
- **#14 turns:** use the rule above. Key = session id + agent id.
- **#16 background:** detect the quota probe and title generation by the signatures above.
  Treat compaction as a continuation.
- **#22 cache:** a model switch at 71k context forced a 72k-token re-cache
  (`cache_creation_input_tokens: 72142`, `cache_read_input_tokens: 0`). The main thread uses
  the 1-hour cache, which makes "idle beyond TTL" rarer than assumed.
- **#24 stats:** read real quota utilization from the rate-limit headers, and token counts
  from `message_start` + `message_delta`.

## Open questions

- Is `x-claude-code-session-id` equal to `metadata.user_id.session_id`?
- Does a resumed session (`claude --resume`) keep the same session id?
- Is there a reliable compaction marker in the system message text? It was masked in the
  capture, so check it in code by looking for a known instruction prefix without storing it.
- Why was a request sent and dropped right before the interrupted one?
- Request shapes can change with any Claude Code release. Re-run the capture when bumping the
  supported version.
