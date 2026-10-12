# Smart-Claude-Router

A small proxy that sits between Claude Code and Anthropic. The goal is to let it choose a cheaper
Claude model or a lower effort level when a request doesn't need more, so your subscription quota
lasts longer.

**Status: v0.1.0.** The proxy forwards everything unchanged and shows one log line per request.
It does not choose models yet, so it doesn't save quota yet. Routing comes in v0.2.0.

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

## Documentation

- [Setup guide](docs/setup.md): installing, starting automatically at login, troubleshooting,
  and removing everything.
- [The proxy](docs/proxy.md): what it does and what it never does.
- [Running it](docs/run.md): options, logs, stopping.
- [The config file](docs/config.md): every setting.

## Privacy

The proxy runs on your machine and only talks to Anthropic. It never stores or prints your login,
your prompts or Claude's answers. Its log only has the request method, path, status, timing and
the model name.

## License

MIT or Apache-2.0, at your option.
