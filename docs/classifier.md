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

Your request is never failed or held back for long. If Ollama is not running, answers with an
error, takes longer than `timeout_ms`, or gives an answer that breaks the format, the message
goes to the `default_tier` instead. The log says why:

- `reason=classifier low, confidence 0.90`: the classifier decided
- `reason=classifier failed (timeout), default tier`
- `reason=classifier failed (ollama unreachable), default tier`
- `reason=classifier failed (ollama status 404), default tier`: usually a model that was not pulled
- `reason=classifier failed (invalid answer), default tier`

The proxy also logs a warning with the same cause each time.

The model needs to be loaded in memory to answer within the timeout. The proxy asks Ollama to
load it at startup and to keep it loaded (`keep_alive`). If Ollama is not ready at startup, the
proxy says so and the first messages use the default tier.

## Settings

In the `[classifier]` block of [the config file](config.md):

| Setting | Default | What it does |
|---|---|---|
| `url` | `http://127.0.0.1:11434` | Where Ollama listens. |
| `model` | `qwen2.5:3b` | The Ollama model that grades messages. |
| `timeout_ms` | `500` | Longest wait for an answer, in milliseconds. |
| `keep_alive` | `30m` | How long Ollama keeps the model loaded after a call. `-1` keeps it loaded. |
| `max_chars` | `600` | Most characters of your message the model sees. |
| `prompt_file` | not set | Your own prompt, replacing the built-in one. |

The default model is a starting point, not a measured choice.

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
  `Router`. Its decisions use `DecisionSource::Classifier`, for failures too: a failure returns
  the default tier's decision with a reason naming the cause. `warm_up()` loads the model.
- `src/ollama.rs`: `OllamaClient` shares one `reqwest` client, posts to `/api/chat` with
  `stream: false`, `format` set to `classifier::output_schema()`, `temperature: 0`,
  `think: false` and a small `num_predict`. `ClassifyError` separates timeout, unreachable,
  status, unreadable reply and invalid answer. Its `label()` is the only text that reaches logs.
- `main.rs` installs the router when `router = "classifier"` and starts the warm-up in a
  background task.
- Not done yet: a fallback to the rules router, a confidence threshold and a circuit breaker
  after repeated failures (#20).

Tests use a mock Ollama (`tests/classifier.rs`): valid grade, effort capping, no credentials
forwarded, continuation reuse, invalid answers, error status, Ollama down, slow Ollama.
