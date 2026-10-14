# The config file

## What it is

A small text file where you can change how Smart-Claude-Router runs. You don't need one. Without
it, the proxy uses the defaults listed below, which is what most people want.

## Where it goes

`~/.config/smart-claude-router/config.toml`

If you have set `XDG_CONFIG_HOME`, the file goes in `$XDG_CONFIG_HOME/smart-claude-router/`
instead.

The repository has a `config.example.toml` with every setting and a comment for each. Copy it to
that location and change what you need.

When the proxy starts, it prints which file it used, or says it is using the built-in defaults.

## What you can set

| Setting | Default | What it does |
|---|---|---|
| `listen` | `127.0.0.1:8899` | The address the proxy listens on. Claude Code must use the same one in `ANTHROPIC_BASE_URL`. |
| `upstream` | `https://api.anthropic.com` | Where requests are sent on to. |
| `default_tier` | `high` | The tier used when routing has no better answer. |
| `mode` | `route` | `route` applies the router's choices. `observe` only logs them and changes nothing. See [dry run](dry-run.md). |
| `router` | `rules` | Which router picks the tier: `rules` (acts only when `[rules] enabled = true`) or `classifier`, a local model that grades each message. See [the classifier router](classifier.md). |
| `[tiers.low]` | Haiku, effort `medium` | The cheapest model and effort routing can choose. |
| `[tiers.medium]` | Sonnet, effort `high` | The middle option. |
| `[tiers.high]` | Opus, effort `xhigh` | The strongest option. These are Claude Code's own defaults. |
| `[rules]` | `enabled = false` | Turns the rules router on, and holds your rules. See [the rules router](rules.md). |
| `[classifier]` | Ollama at `127.0.0.1:11434`, model `qwen2.5:3b` | Which local model grades messages, how long to wait for it, and what it is shown. Used when `router = "classifier"`. See [the classifier router](classifier.md) and [the classifier prompt](classifier-prompt.md). |

Effort can be `low`, `medium`, `high`, `xhigh` or `max`.

## What the tiers are for

When the rules router or the classifier router is on, it picks one of the three tiers for each
message you send, and Claude Code's request is changed to use that tier's model and effort. With
the defaults (`router = "rules"` and `enabled = false`) nothing uses the tiers and the proxy
forwards every request unchanged.

The default tier is `high` because that is what Claude Code already uses. When the rules are on
and none of them matches, or when the classifier cannot answer, the message goes to the default
tier, so a message is only lowered by a rule, an override marker or a classifier answer.

## Rules

- Any setting you leave out keeps its default. A file with a single line is fine.
- If you write a tier, give both its `model` and its `effort`. Tiers you don't write keep their
  defaults.
- Keep `listen` on `127.0.0.1`. Requests passing through the proxy carry your Claude login, so
  the proxy should only be reachable from your own machine.
- Changes take effect the next time the proxy starts.

## When the file has a mistake

The proxy refuses to start. It prints the file, the line, and what is wrong. For example:

```
Error: invalid config file /home/you/.config/smart-claude-router/config.toml

Caused by:
    TOML parse error at line 3, column 10
      |
    3 | effort = "huge"
      |          ^^^^^^
    unknown variant `huge`, expected one of `low`, `medium`, `high`, `xhigh`, `max`
```

Misspelled settings are caught the same way, so a typo never gets silently ignored.

## Turning it off

Delete or rename the file. The proxy goes back to the defaults the next time it starts.

## Using another file

Start the proxy with `--config <PATH>` to use a different file (see `docs/run.md`). That file
must exist.
