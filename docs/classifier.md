# The classifier router

## What it is

A router that asks a small AI model on your own computer how hard each new message is, and
picks the Claude model for it. Easy messages go to a cheaper model, hard ones to a stronger
one. The small model runs through [Ollama](https://ollama.com), so it costs no Claude quota and
your messages are not sent to any online service.

Compared to [the rules router](rules.md), it judges the meaning of a message instead of matching
words and lengths. It is slower: every new message waits for the small model, up to the timeout
below.

## It is off by default

Turn it on in the config file:

```toml
router = "classifier"
```

Then restart the proxy. With the default `router = "rules"`, no Ollama call is ever made.

You also need Ollama running with the model pulled:

```
ollama pull qwen2.5:3b
```

## What it does

For each new message you send:

1. It builds a short description of the message. See [the classifier prompt](classifier-prompt.md)
   for exactly what the small model is shown.
2. It sends that to Ollama and gets back a tier (`low`, `medium` or `high`), an effort and a
   confidence.
3. The tier gives the model, through the `[tiers]` table in [the config file](config.md).
4. The effort is the tier's configured effort, lowered if the classifier asked for less. It is
   never raised above what the tier is set to.

The requests Claude Code makes while answering that message keep the same model. See
[the router](router.md).

## When the classifier cannot answer

Your request is never failed, and never held back for longer than `timeout_ms`. When the
classifier gives no usable answer, a fallback decides instead:

- **The rules router**, if you turned it on (`[rules] enabled = true`). Your rules and `!low` /
  `!high` markers then decide, as described in [the rules router](rules.md).
- **The default tier** (`default_tier`), if the rules are off.

The classifier gives no usable answer when:

- Ollama is not running or the connection drops
- Ollama answers with an error, usually a model that was not pulled (status 404)
- the answer takes longer than `timeout_ms`
- the answer breaks the format
- the model says it is less sure than `min_confidence`. A message it is unsure about is not
  lowered by a guess.
- the classifier is paused (see below)

The log says which one, in the `reason` field:

- `classifier low, confidence 0.90`: the classifier decided
- `classifier fallback (timeout): default tier`
- `classifier fallback (ollama unreachable): rule small-fix`: the rules decided
- `classifier fallback (ollama status 404): default tier`
- `classifier fallback (invalid answer): default tier`
- `classifier fallback (confidence 0.30 below 0.50): no rule matched, default tier`
- `classifier fallback (paused after repeated failures): default tier`

The proxy also logs a warning with the cause each time a call fails.

## Pausing after repeated failures

If Ollama is stopped, every message would otherwise wait for the full timeout before the
fallback decides. To avoid that, after `failure_limit` failures in a row (3 by default) the
classifier is not called for `cooldown_secs` seconds (30 by default). During that time messages
go straight to the fallback with no wait, and the proxy logs one warning when it pauses.

When the pause ends, the next message checks whether Ollama is back. If it answers, the
classifier works normally again and the proxy logs `classifier answering again`. If it fails,
the pause starts over. Only one message at a time makes that check. A low-confidence answer
counts as an answer: it does not count as a failure.

The pause lasts until the cooldown ends, even if you start Ollama sooner. Restarting the proxy
clears it.

## Loading the model

The model needs to be loaded in memory to answer within the timeout. The proxy asks Ollama to
load it at startup and to keep it loaded (`keep_alive`). If Ollama is not ready at startup, the
proxy says so and the first messages use the fallback until a call succeeds.

## Settings

In the `[classifier]` block of [the config file](config.md):

| Setting | Default | What it does |
|---|---|---|
| `url` | `http://127.0.0.1:11434` | Where Ollama listens. |
| `model` | `qwen2.5:3b` | The Ollama model that grades messages. |
| `timeout_ms` | `500` | Longest wait for an answer, in milliseconds. |
| `keep_alive` | `30m` | How long Ollama keeps the model loaded after a call. `-1` keeps it loaded. |
| `min_confidence` | `0.5` | Answers with a lower confidence (0 to 1) are not used. |
| `failure_limit` | `3` | Failures in a row before the classifier is paused. At least 1. |
| `cooldown_secs` | `30` | How long the classifier is paused. At least 1. |
| `max_chars` | `600` | Most characters of your message the model sees. |
| `prompt_file` | not set | Your own prompt, replacing the built-in one. |

The default model is a starting point, not a measured choice. On a computer without a GPU,
the local model measured in [the benchmark](classifier-benchmark.md) needed about 1.5 seconds
per message, so `timeout_ms = 500` would not be enough there. Raise it, or expect the fallback
to decide.

## What it never does

- It never sends your login, headers or earlier messages to Ollama.
- It never logs your message or the model's answer. Logs only name the cause of a failure.
- It never runs for requests inside an answer that already started, or for the requests Claude
  Code makes on its own (quota checks, session titles).
- It never changes anything in a request except the model and effort.

## Keep Ollama on your machine

The text of your message is sent to `url`. If `url` is not on this machine, the proxy prints a
warning at startup. Point it somewhere else only if you trust that place with your messages.

## Turning it off

Set `router = "rules"` (or remove the line) and restart the proxy.

## For developers

- `src/classifier_router.rs`: `ClassifierRouter::new(&Config, OllamaClient, Prompt)` implements
  `Router`. A usable answer gives `DecisionSource::Classifier`. Every other outcome goes through
  `fallback()`: it asks a `RulesRouter` when `rules.enabled`, else uses `default_tier`, then sets
  the source to `DecisionSource::Fallback` and the reason to
  `classifier fallback (<cause>): <inner reason>`. `warm_up()` loads the model.
- `src/breaker.rs`: `Breaker` counts failures in a row and refuses calls until the cooldown ends.
  Time is passed in (`allow(now)`, `record_failure(now)`), so its tests do not sleep. After the
  cooldown one call is let through and the next only after `probe_window` (the call timeout), so
  there is no "probe in progress" flag that a dropped request could leave set. `record_*` return
  a `Change` that the router uses to log once when it opens and once when it closes. Timeouts,
  unreachable, status, unreadable replies and invalid answers count as failures; a low-confidence
  answer does not.
- `src/ollama.rs`: `OllamaClient` shares one `reqwest` client, posts to `/api/chat` with
  `stream: false`, `format` set to `classifier::output_schema()`, `temperature: 0`,
  `think: false` and a small `num_predict`. `ClassifyError` separates timeout, unreachable,
  status, unreadable reply and invalid answer. Its `label()` is the only text that reaches logs.
- `main.rs` installs the router when `router = "classifier"` and starts the warm-up in a
  background task.

Tests use a mock Ollama (`tests/classifier.rs`): valid grade, effort capping, no credentials
forwarded, continuation reuse, invalid answers, error status, Ollama down, slow Ollama, rules
and default-tier fallback with their reasons, the confidence threshold, pausing, recovery and a
failed recovery check.
