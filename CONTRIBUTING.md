# Contributing

## Setup

If this project has a `flake.nix` (you opted into Nix when bootstrapping):

```
nix develop
pre-commit install --hook-type commit-msg --hook-type pre-commit
```

Otherwise, install `rustc`/`cargo`/`rustfmt`/`clippy` (e.g. via `rustup`) and `pre-commit`
yourself, then:

```
pre-commit install --hook-type commit-msg --hook-type pre-commit
```

## Issues

- Title: `[Scope] Imperative summary` — scope is a functional module/domain, not `[BUG]`/`[FEAT]`.
  Category (bug/feature/chore) goes in labels, never the title.
- Labels: reuse existing ones first (`gh label list`). Type + scope labels only —
  scope labels are applied automatically by the `auto-scope-label` workflow from the
  title's `[Scope]` prefix, no manual creation needed.
- Priority is a **Project field**, not a label. Check `gh project field-list <number> --owner <owner>`
  for a `Priority` single-select field and set it there. Only fall back to a `priority:*` label
  if no project exists.

## License

Chosen at bootstrap time (or fetched fresh via `gh api licenses/:key` and dropped into
`LICENSE`, or `LICENSE-MIT`/`LICENSE-APACHE` for the MIT OR Apache-2.0 dual-license default).
To change it later, either edit the file directly or re-fetch a different one the same way.

## Branches & commits

- Branch naming: `type/slug` (`feat`, `fix`, `refacto`, `docs`, `chore`, `test`, `setup`).
- Commit format: `type(scope): description`.
- **No `Co-authored-by` trailers, ever** — enforced locally by the `reject-co-authored-by`
  pre-commit hook (bypassable with `--no-verify`) and by the `reject-co-author` CI check on
  every PR (not bypassable). If a commit already has one, rewrite history before opening the PR.

## Pull requests

- Fill the PR template's `Closes #` field — pairs with the `project-status` workflow, which
  parses `closes|fixes|resolves #N` from the PR body to move the linked issue's board status.

## Verifying the DevOps wiring itself

If you change anything under `.github/workflows/project-status.yml`, `auto-scope-label.yml`,
or the board-setup logic in `dev-templates`, see `dev-templates/docs/SMOKE_TEST.md` to verify
end-to-end against a disposable repo — this repo's own CI can't exercise those workflows in place.
