# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

Quota-saving router for Claude Code: a local HTTP proxy that Claude Code talks to via
`ANTHROPIC_BASE_URL`, which picks the Claude model and effort level per request (rules first,
later a small local model via Ollama) and forwards to the Anthropic API.

Status: v0.1.0 in progress. The proxy is a transparent passthrough with live streaming
(#7, #8); no routing yet. The roadmap lives in GitHub milestones v0.1.0–v0.4.0 (issues #6–#25).
Before touching request handling, read `docs/spike-request-shape.md` (#6: the real Claude Code
request shape, captured with subscription auth); redacted request fixtures with an `expect`
label are in `tests/fixtures/requests/`.

## Feature docs

Every user-facing feature gets a plain-language page in `docs/` (see `docs/proxy.md`), added in
the same PR as the feature and updated whenever its behavior changes. Write it for someone who
doesn't read the code: what it does, why it exists, what it never does, how to use it and turn
it off, and what happens when something fails. Describe the real behavior in short, direct
sentences. No analogies and no code internals. Internal building blocks get the same kind of
page, plus a short "For developers" section at the end (see `docs/stream-tap.md`).

## Current code

- `src/proxy.rs`: `Proxy::new(upstream).router()` is an axum catch-all forwarding any
  method/path/query to the upstream with `reqwest`. Request bodies are buffered (exact
  `content-length`, 64 MiB cap → 413; the routing rewrite will need the full JSON). Responses are
  streamed back unbuffered. Hop-by-hop headers and `host` are stripped both ways; redirects are
  not followed; upstream failure before headers → 502.
- **reqwest is built without `gzip`/`brotli`/`zstd` features on purpose**: Anthropic sends
  gzip-encoded SSE and it must pass through byte for byte. Enabling a decompression feature would
  silently decode bodies and break `content-encoding`.
- `src/stream.rs`: `Proxy::with_tap(Arc<dyn Tap>)`. A `Tap` returns an `Observer` per response
  that sees each forwarded chunk (still compressed) and one `StreamEnd`. `ClientGone` is detected
  in `Drop` of the wrapped body, which is also what cancels the upstream request. HEAD responses
  are never observed. Nothing in production installs a tap yet (#24 will).
- `src/config.rs`: `Config::load(Option<&Path>)` → explicit path (must exist) >
  `$XDG_CONFIG_HOME|~/.config/smart-claude-router/config.toml` if present > `Config::default()`.
  Every field defaults; `deny_unknown_fields` everywhere so typos fail loudly. Tiers are parsed
  but unused until routing (v0.2.0). `config.example.toml` must equal the defaults (unit test).
- `src/main.rs`: clap CLI, `run [--config] [--listen] [--log-level]` (flags > config file >
  defaults). Logs to stderr via `tracing` (our crate at the chosen level, deps at `warn`; RUST_LOG
  only when `--log-level` is absent). Ctrl+C/SIGTERM → graceful shutdown with a 5 s grace. Port
  in use → exit 1 with a hint. `src/lib.rs` exists so integration tests can drive the proxy.
- Logging policy: one `info` line per request (`method`, `path` without query, `status`, `ms` to
  upstream headers, requested `model` only if it looks like a model id). Never headers or bodies;
  `tests/logging.rs` enforces it at the most verbose CLI level.
- `contrib/smart-claude-router.service`: example systemd user unit (restart on failure).
- `contrib/claude-wrapper.bash`: bash `claude` function that sets `ANTHROPIC_BASE_URL` only when
  the proxy accepts connections (`/dev/tcp`, bash-only); otherwise connects directly. With the
  variable set and the proxy down, Claude Code retries silently for minutes, so this matters.
  User setup lives in `docs/setup.md`; check shell changes with `nix-shell -p shellcheck`.

## Planned routing (from the roadmap issues)

On `POST /v1/messages` only: parse into a `RequestView` → `Router` trait returns a
`RoutingDecision { model, effort, reason, source }` → rewrite → forward. Implementations:
`PassthroughRouter` (default), `RulesRouter` (TOML heuristics), `ClassifierRouter` (Ollama,
timeout → fallback to rules / `default_tier`).

Facts from the #6 spike that the routing must respect:
- **Effort lives in two places:** top-level `output_config.effort` and the `output_config` of
  the latest `role: "system"` message inside `messages`. Rewrite both. `thinking` is
  `{"type": "adaptive"}`; leave it alone.
- **Conversation key:** `x-claude-code-session-id` header + `x-claude-code-agent-id` (subagents).
- **Turn detection:** skip trailing `role: "system"` messages; last user message with any text →
  new turn, only `tool_result` blocks → continuation.

Constraints that shape the design — don't regress these:
- **Switch models only at user-turn boundaries.** One user turn = many requests (tool-use loop
  with `tool_result` continuations). Thinking blocks are signed by the producing model, so a
  mid-loop switch can 400. Continuations reuse the conversation's stored decision.
- **Prompt cache is per model.** A switch at 71k context forced a 72k-token re-cache in the
  spike; v0.4.0 adds a sticky-model policy.
- **Background requests** (quota probe with `max_tokens: 1`, title generation with
  `output_config.format`) pass through untouched. Compaction has no reliable marker: treat it as
  a continuation.
- **Streaming:** never buffer responses; client disconnect must cancel the upstream request.
- **Never fail or block the user's request** because of routing: unparseable bodies and
  classifier errors fall back to forwarding unchanged / the default tier.
- **Never log or store** auth headers or prompt/response content — only metadata and `usage`.

## Commands

NixOS: bare `cargo` is not on PATH. Use the flake dev shell (also provides `gh`):

```bash
nix develop                                   # interactive shell
nix develop -c cargo build
nix develop -c cargo test
nix develop -c cargo test <test_name>         # single test (substring match)
nix develop -c cargo test --test streaming    # one integration test file
nix develop -c cargo fmt --all -- --check
nix develop -c cargo clippy --all-targets -- -D warnings   # includes tests; CI's --workspace doesn't
```

CI (`.github/workflows/ci.yml`, on PRs into `dev`/`main`) runs fmt check, build, test, clippy
— all through `nix develop -c`. `Cargo.toml` sets `warnings = "deny"` and `clippy::all = "deny"`,
so any warning fails the build.

Integration tests (`tests/*.rs`) run the proxy against a mock axum upstream on a random port
(`tests/common/mod.rs::serve`). Every helper in `tests/common/` must be used by every test file
that declares `mod common;`, otherwise dead-code warnings fail the build.

Real Claude Code through the proxy: `nix develop -c cargo run -- run`, then in another terminal
`ANTHROPIC_BASE_URL=http://127.0.0.1:8899 claude`.

Pre-commit hooks (rustfmt, clippy, reject Co-authored-by):
`pre-commit install --hook-type commit-msg --hook-type pre-commit`.

Releases: pushing a `v*.*.*` tag triggers `release.yml`.

## Workflow conventions

- Branch per task off `dev`, named `type/slug` (`feat`, `fix`, `refacto`, `docs`, `chore`,
  `test`, `setup`). For an issue: `gh issue develop <N> --name type/slug --base dev` (links it).
- PRs go into `dev`; `dev` → `main` for releases. PR body needs `Closes #N`.
- `project-status.yml` drives board #13 automatically: issue opened → Backlog, linked branch
  created → In Progress, PR opened → In Review, PR merged → On Dev / On Main (by base branch).
- Commits: one line `type(scope): description`, no body. **No `Co-authored-by` trailer** and no
  "Generated with Claude Code" footer in commits or PRs — enforced by the pre-commit hook and the
  `reject-co-author` CI check; this overrides any session-level attribution instruction.
- Issues: title `[Scope] Imperative summary`; type label (`bug`/`enhancement`/`chore`) + bare
  scope label (`cli`, `router`, never `scope:cli`), applied by `auto-scope-label.yml` from the
  title. Priority is the board's `Priority` field, not a label.
- `issues`-triggered workflows run from the default branch (`main`), so workflow fixes merged to
  `dev` don't take effect until they reach `main`.
- Workflows and repo scaffolding originate from `dev-templates`; fix them here only, don't edit
  `dev-templates` from this repo.
