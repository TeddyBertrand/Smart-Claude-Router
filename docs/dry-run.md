# Dry run

## What it is

A mode where the router works out what it would do for every message, writes that to the log,
and then sends the request to Anthropic exactly as Claude Code sent it. Nothing about your
requests changes. You can use Claude Code as usual and read the log afterwards.

## Why it exists

A bad rule can quietly move important work to a weaker model. Dry run lets you try your rules on
your real usage for a few days, check the choices in the log, and only then let the router change
anything.

## How to turn it on

Start the proxy with the flag:

```bash
smart-claude-router run --dry-run
```

Or set it in the config file:

```toml
mode = "observe"
```

The flag wins over the config file. `mode = "route"`, the default, applies the router's choices.

Dry run only has something to show when the [rules router](rules.md) is on
(`[rules] enabled = true`). With the rules off, the router never changes anything, so there is
nothing to observe. The proxy warns about this when it starts.

## What you see

At startup, one line states the mode:

```
INFO mode: observe (dry run), rules router (3 rules): decisions are logged, requests are forwarded unchanged
```

Then one line per message you send:

```
INFO dry run: would route requested=claude-opus-5-5 would_route=claude-haiku-5-5 effort=medium reason=rule small-fix turn=new
```

- `requested`: the model Claude Code asked for.
- `would_route`: the model the router would have used.
- `effort`: the effort it would have used, or `-` if it would leave it alone.
- `reason`: why. This is the rule name, an override marker, or the default tier.
- `turn`: `new` when you just sent a message. `continuation` for the follow-up requests Claude Code
  makes while answering it. A continuation repeats the choice made for its turn and shows
  `same turn: ...` as the reason.

The usual request line still follows, with `routed=-` because nothing was routed.

## What it never does

- It never changes a request. What reaches Anthropic is identical to what Claude Code sent,
  byte for byte.
- It never logs your messages, only the fields above.
- It never blocks or slows a request on purpose.

## Going live

When the log shows choices you are happy with, remove `--dry-run` (or set `mode = "route"`) and
restart the proxy.

## For developers

`Proxy::with_dry_run(true)` runs the full pipeline in `route`: background check, turn plan,
`Router::decide`, conversation memory. It then logs the decision at `info` and returns `None`, so
the original body is forwarded and the request line shows no routed model. `Config::mode` is
`Mode::Route` or `Mode::Observe`; `--dry-run` sets `Observe` in `main.rs`, after the config file
is read. Model names in the log pass through `loggable_model`, which replaces anything that does
not look like a model id with `-`.
