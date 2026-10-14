# Smart-Claude-Router

A small proxy that sits between Claude Code and Anthropic. The goal is to let it choose a cheaper
Claude model or a lower effort level when a request doesn't need more, so your subscription quota
lasts longer.

**Status: v0.2.0.** The proxy can choose a cheaper model and effort for easy messages, using
rules you write in the config file. This is off by default: until you turn on the
[rules router](docs/rules.md), everything is forwarded unchanged. You can try it first with
[dry run](docs/dry-run.md), which only logs what it would choose.

## Quick start

You need [Claude Code](https://claude.com/claude-code) and either a Rust toolchain (`cargo`) or
Nix.

```bash
git clone https://github.com/TeddyBertrand/Smart-Claude-Router.git
cd Smart-Claude-Router
cargo install --path .                 # with Nix: nix develop -c cargo install --path .
smart-claude-router run
```

`cargo install` puts the program in `~/.cargo/bin`, which must be on your `PATH`.

Leave that terminal open. In another one:

```bash
ANTHROPIC_BASE_URL=http://127.0.0.1:8899 claude
```

Send a message. The proxy prints a line for each request, for example:

```
INFO smart_claude_router::proxy: request method=POST path=/v1/messages status=200 ms=638 model=claude-opus-5-5
```

To stop using it, close the proxy with Ctrl+C and start `claude` without `ANTHROPIC_BASE_URL`.

## Prebuilt binaries

Each tagged release has archives for Linux (x86_64, aarch64) and macOS (Intel, Apple Silicon) on
the [Releases page](https://github.com/TeddyBertrand/Smart-Claude-Router/releases). Download the
archive for your machine, unpack it, and put `smart-claude-router` on your `PATH`. You don't need
Rust for this.

## Documentation

- [Setup guide](docs/setup.md): installing, starting automatically at login, troubleshooting,
  and removing everything.
- [The proxy](docs/proxy.md): what it does and what it never does.
- [The router](docs/router.md): how the model and effort for a request get decided.
- [The rules router](docs/rules.md): choose a cheaper model for easy messages, with `!low` and
  `!high` overrides.
- [Dry run](docs/dry-run.md): see what the router would choose before it changes anything.
- [Running it](docs/run.md): options, logs, stopping.
- [The config file](docs/config.md): every setting.

## Privacy

The proxy runs on your machine and only talks to Anthropic. It never stores or prints your login,
your prompts or Claude's answers. Its log only has the request method, path, status, timing and
the model name.

## License

MIT or Apache-2.0, at your option.
