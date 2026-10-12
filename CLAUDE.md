# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

Quota-saving router for Claude Code: a local HTTP proxy that Claude Code talks to via
`ANTHROPIC_BASE_URL`, which picks the Claude model and effort level per request (rules first,
later a small local model via Ollama) and forwards to the Anthropic API.

Status: bootstrapped from the `dev-templates` Rust template; `src/main.rs` is still the
template placeholder. The roadmap lives in GitHub milestones v0.1.0–v0.4.0 (issues #6–#25),
each issue with a plain-language and a technical description. #6 (request-shape / subscription
auth spike) gates everything else.

## Feature docs

Every user-facing feature gets a plain-language page in `docs/` (see `docs/proxy.md`), added in
the same PR as the feature and updated whenever its behavior changes. Write it for someone who
doesn't read the code: what it does, why it exists, what it never does, how to use it and turn
it off, and what happens when something fails. Describe the real behavior in short, direct
sentences. No analogies and no code internals.

## Planned architecture (from the roadmap issues)

Request path: Claude Code → proxy (`axum`, catch-all passthrough to upstream via `reqwest`) →
on `POST /v1/messages` only: parse into a `RequestView` → `Router` trait returns a
`RoutingDecision { model, effort, reason, source }` → rewrite only `model` + effort/thinking
fields → forward. Implementations of `Router`: `PassthroughRouter` (default), `RulesRouter`
(TOML-configured heuristics), `ClassifierRouter` (Ollama, with timeout → fallback to rules /
`default_tier`).

Constraints that shape the design — don't regress these:
- **Switch models only at user-turn boundaries.** One user turn = many requests (tool-use loop
  with `tool_result` continuations). Thinking blocks are signed by the producing model, so a
  mid-loop switch can 400. Continuations reuse the conversation's stored decision.
- **Prompt cache is per model.** Switching on a long conversation forces a full cache miss that
  can cost more than it saves; v0.4.0 adds a sticky-model policy.
- **Background requests** (title generation, compaction, token counting) pass through untouched.
- **Streaming:** SSE must be piped chunk by chunk, never buffered; client disconnect cancels
  the upstream request.
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
nix develop -c cargo fmt --all -- --check
nix develop -c cargo clippy --workspace -- -D warnings
```

CI (`.github/workflows/ci.yml`, on PRs into `dev`/`main`) runs fmt check, build, test, clippy
— all through `nix develop -c`. `Cargo.toml` sets `warnings = "deny"` and `clippy::all = "deny"`,
so any warning fails the build.

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
