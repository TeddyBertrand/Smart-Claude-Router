# Setup guide

This guide goes from nothing to using Claude Code through Smart-Claude-Router every day, and
back. Each step can be undone; the last section lists how.

## What you need

- Claude Code, already logged in.
- A Rust toolchain (`cargo`, for example from rustup), or Nix.
- A clone of this repository.

## 1. Install

With a Rust toolchain:

```bash
cargo install --path .
```

With Nix, from the repository:

```bash
nix develop -c cargo install --path .
```

Both put the `smart-claude-router` program in `~/.cargo/bin`. Check that it is found:

```bash
smart-claude-router --version
```

On NixOS, a program built inside `nix develop` uses libraries from the Nix store. After
`nix-collect-garbage`, it can stop starting; running the install command again fixes it.

## 2. Start the proxy

```bash
smart-claude-router run
```

It keeps running in that terminal and prints one line per request. Ctrl+C stops it. Options
and logs are explained in [run.md](run.md).

## 3. Use it with Claude Code

In another terminal:

```bash
ANTHROPIC_BASE_URL=http://127.0.0.1:8899 claude
```

Send a message. If the proxy terminal shows `request` lines, Claude Code is going through it.
Nothing else changes: same login, same answers, same speed.

## 4. Change settings (optional)

Everything works without a config file. To change the address or the model tiers, see
[config.md](config.md).

## Using it every day

Two things have to happen by themselves: the proxy has to be running, and Claude Code has to be
told to use it.

### Start the proxy at login

The repository has an example systemd user service. It starts the proxy when you log in and
restarts it if it crashes:

```bash
mkdir -p ~/.config/systemd/user
cp contrib/smart-claude-router.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now smart-claude-router
```

Check it and read its log:

```bash
systemctl --user status smart-claude-router
journalctl --user -u smart-claude-router -f
```

The service expects the program in `~/.cargo/bin`. If you installed it somewhere else, change the
`ExecStart` line in the copied file.

### Make Claude Code use it

Pick one of these two options. Don't use both: with option B set, option A can no longer fall
back to a direct connection.

**Option A (recommended): a `claude` function for bash.** Add this line to `~/.bashrc`, with the
real path of your clone:

```bash
source /path/to/Smart-Claude-Router/contrib/claude-wrapper.bash
```

From then on, typing `claude` in a new terminal:

- goes through the proxy when it is running;
- connects to Anthropic directly when it isn't, after printing
  `smart-claude-router is not running on 127.0.0.1:8899; connecting to Anthropic directly`.

To skip the proxy once, type `command claude`. If the proxy listens on another address, set
`SMART_CLAUDE_ROUTER_ADDR` (for example `export SMART_CLAUDE_ROUTER_ADDR=127.0.0.1:9000`) before
the `source` line. The function uses a bash feature, so it does not work in zsh or fish yet.

**Option B: a Claude Code setting.** Add this to `~/.claude/settings.json` (merge it with what is
already there):

```json
{
  "env": {
    "ANTHROPIC_BASE_URL": "http://127.0.0.1:8899"
  }
}
```

This covers every Claude Code session, including ones started from an editor. It also means
Claude Code can't work at all while the proxy is down, so only use it together with the service
above.

## If Claude Code seems stuck

When Claude Code is told to use the proxy but the proxy isn't running, it doesn't show an error
right away. It keeps retrying quietly; in a test it showed nothing for more than a minute.

To fix it, either start the proxy:

```bash
systemctl --user start smart-claude-router   # if you installed the service
smart-claude-router run                      # otherwise
```

or start Claude Code without the proxy for now:

```bash
env -u ANTHROPIC_BASE_URL claude   # option B, or a variable set by hand
command claude                     # option A
```

Option A avoids this problem by checking first.

## Troubleshooting

**`cannot listen on 127.0.0.1:8899 (is another smart-claude-router already running?)`**
Another program uses that address, usually a proxy started earlier or the service. Check with
`systemctl --user status smart-claude-router`, stop the extra one, or start this one with
`--listen` on another port and use that port in `ANTHROPIC_BASE_URL`.

**Claude Code says you are logged out, or shows an authentication error.**
The proxy passes your login through unchanged, so first check without it (`command claude` or
`env -u ANTHROPIC_BASE_URL claude`).
- If it fails without the proxy too, the login itself is the problem: run `/login` in Claude
  Code.
- If it only fails through the proxy, look at the proxy's `request` lines. A `status=401` or
  `status=403` means Anthropic refused the request; check that `upstream` in your config file is
  `https://api.anthropic.com` or not set at all.

**Requests show `status=502`, or Claude Code reports "upstream request failed".**
The proxy can't reach Anthropic. The proxy log has a warning with the reason, usually a network
or DNS problem, or a wrong `upstream` in the config file.

**The proxy log stays empty while you use Claude Code.**
Claude Code isn't going through the proxy. Check the spelling of `ANTHROPIC_BASE_URL`, that its
port matches the proxy's, and that you started `claude` from a terminal where the variable or the
function is set.

## Removing everything

Undo only the parts you set up:

- Option A: remove the `source .../claude-wrapper.bash` line from `~/.bashrc`.
- Option B: remove `ANTHROPIC_BASE_URL` from the `env` block in `~/.claude/settings.json`.
- Service: `systemctl --user disable --now smart-claude-router`, then
  `rm ~/.config/systemd/user/smart-claude-router.service`.
- Program: `cargo uninstall smart-claude-router`.
- Config file: `rm -r ~/.config/smart-claude-router`.

## Privacy

The proxy only talks to Anthropic. It never stores anything, and its log never contains your
login, API key, prompts or Claude's answers. What it does log is listed in [run.md](run.md).
