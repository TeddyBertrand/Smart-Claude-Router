# Running the proxy

## Starting it

```bash
smart-claude-router run
```

From a clone of the repository, without installing it:

```bash
nix develop -c cargo run -- run
```

The proxy runs in the terminal you started it in and prints what it is doing. Then start Claude
Code in another terminal with:

```bash
ANTHROPIC_BASE_URL=http://127.0.0.1:8899 claude
```

## Options

| Option | What it does |
|---|---|
| `--config <PATH>` | Use this config file instead of `~/.config/smart-claude-router/config.toml`. The file must exist. |
| `--listen <IP:PORT>` | Listen on this address instead of the one in the config file. |
| `--log-level <LEVEL>` | How much to print: `error`, `warn`, `info` (default), `debug` or `trace`. |

Options win over the config file, and the config file wins over the built-in defaults.

## What it prints

At start, it says which config file it used and where it listens. Then one line per request:

```
INFO smart_claude_router::proxy: request method=POST path=/v1/messages status=200 ms=638 model=claude-opus-5-5
```

- `method` and `path`: what Claude Code asked for.
- `status`: Anthropic's answer code (200 = fine, 429 = rate limited, 502 = the proxy could not
  reach Anthropic).
- `ms`: how long Anthropic took to start answering. The answer itself can keep streaming after
  that.
- `model`: the model Claude Code asked for, or `-` when the request names none.

With `--log-level debug` it also prints the full settings it is using.

## What it never prints

Your login token, your API key, your prompts, Claude's answers, or anything else from inside a
request. The request log only contains the fields listed above. A test checks this on every
change.

## Stopping it

Press Ctrl+C. The proxy stops accepting new requests, gives answers that are still arriving up
to 5 seconds to finish, then exits. A system service manager stopping it has the same effect.

## When it can't start

If another program already uses the address, usually a proxy you started earlier, it exits
right away:

```
Error: cannot listen on 127.0.0.1:8899 (is another smart-claude-router already running?)
```

Stop the other one, or pick another address with `--listen` and use the same one in
`ANTHROPIC_BASE_URL`.

A broken config file also stops it at start, with the file, line and problem (see
`docs/config.md`).

## Starting it automatically at login

The repository has an example systemd user service in `contrib/smart-claude-router.service`.
It starts the proxy when you log in and restarts it if it crashes. The install and removal
commands are at the top of that file. Logs then go to the journal:

```bash
journalctl --user -u smart-claude-router -f
```

Making Claude Code use the proxy automatically, and what happens when the proxy is down, is
covered in the [setup guide](setup.md).
