# The proxy

## What it is

Smart-Claude-Router runs a small program on your computer called a proxy. Normally Claude Code
sends every request straight to Anthropic. When you start Claude Code with
`ANTHROPIC_BASE_URL=http://127.0.0.1:8899`, it sends its requests to the proxy instead, and the
proxy sends them on to Anthropic. Answers travel back the same way.

## Why it exists

Every request Claude Code makes says which Claude model to use and how much effort to spend.
By default that is often the most expensive combination, even for simple questions. The goal of
the project is to let the proxy pick a cheaper model or a lower effort level when a request
doesn't need more, so your subscription quota lasts longer.

## What it does today

Nothing changes. The proxy passes every request to Anthropic exactly as Claude Code wrote it,
and passes every answer back exactly as Anthropic wrote it. Answers still appear word by word
as they are generated.

This first version exists to prove that the proxy can sit between Claude Code and Anthropic
without you noticing. Model and effort choices come in later versions.

## What it never does

- It does not save your prompts, Claude's answers, or your login token anywhere.
- It does not send anything anywhere except to Anthropic.
- It does not block or slow down a request because of a routing decision. If the routing logic
  fails, the request goes through unchanged.

## How a request goes through it

1. You type a message in Claude Code.
2. Claude Code sends the request to the proxy on your machine.
3. The proxy forwards it to Anthropic, including your login.
4. Anthropic's answer comes back to the proxy and is passed to Claude Code as it arrives.

One message from you can produce several requests. For example, Claude reads a file, gets the
result, then answers. Each of those goes through the proxy the same way.

## Live answers and Esc

The proxy never waits for a full answer before passing it on. Each piece Anthropic sends goes to
Claude Code as soon as it reaches the proxy, so answers appear as fast as they do without the
proxy.

When you press Esc, Claude Code closes its connection to the proxy. The proxy then closes its
connection to Anthropic right away, the same way Claude Code does when it talks to Anthropic
directly. Anthropic stops working on an answer nobody is reading anymore.

## When something goes wrong

- If the proxy can't reach Anthropic, Claude Code shows an error saying the upstream request
  failed.
- If a request is larger than 64 MiB, the proxy refuses it. Anthropic itself rejects requests
  above 32 MB, so this doesn't happen in normal use.
- Any error Anthropic returns, such as a rate limit, reaches Claude Code unchanged.
- If Anthropic's connection breaks in the middle of an answer, Claude Code sees the answer cut
  off, the same as it would without the proxy.

## How to use it

Start the proxy:

```bash
nix develop -c cargo run
```

Then, in another terminal:

```bash
ANTHROPIC_BASE_URL=http://127.0.0.1:8899 claude
```

To stop using it, start Claude Code without `ANTHROPIC_BASE_URL`. Nothing else needs to be
undone.

The address (`127.0.0.1:8899`) is fixed for now and can only be reached from your own machine.
A config file to change it is planned (#9).

## What comes next

- **v0.1.0:** passthrough, answers streamed live, config file, a `run` command, setup guide.
- **v0.2.0:** the proxy picks the model and effort using simple rules you can read and edit.
  The model only changes when you send a new message, never in the middle of Claude working on
  one.
- **v0.3.0:** a small AI model running on your machine makes that choice instead of fixed rules.
  If it is slow or fails, the rules decide.
- **v0.4.0:** the proxy avoids switching models when that would cost more than it saves, moves
  to a stronger model when the work is going badly, and shows how much quota it saved.
