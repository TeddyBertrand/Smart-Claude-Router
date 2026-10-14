# The rules router

## What it is

A checklist you write in the config file. For each new message you send in Claude Code, the
rules router goes through the checklist and picks one of three tiers: `low`, `medium` or `high`.
Each tier is a model with an effort level (see [the config file](config.md)). The proxy then
changes the request to use that model and effort.

It saves quota by sending easy messages to a cheaper model. It uses no AI model to decide, so it
costs nothing and answers instantly. The log always says which rule made the choice.

## It is off by default

Nothing changes until you turn it on in the config file:

```toml
[rules]
enabled = true
```

With `enabled = false`, or no config file, every request is forwarded exactly as Claude Code
sent it.

## How it decides

For each new message you send, three steps in this order. The first one that applies decides.

1. **An override marker in your message.** Write `!low`, `!medium` or `!high` as a word of its
   own anywhere in your message to force that tier. Letter case doesn't matter, and a full stop
   or comma right after it is fine (`!low.`). If you write several, the last one counts. Markers
   win over every rule.
2. **Your rules**, from top to bottom. The first rule that matches decides.
3. **The default tier** (`default_tier` in the config file), if no rule matched.

The marker stays in your message. Claude sees it. Only the model and effort of the request are
changed.

The choice is made once per message you send. The requests Claude Code makes while answering it,
such as reading a file and coming back, keep the same model. See [the router](router.md).

## Writing rules

Each rule is a block in the config file. Here are three:

```toml
[[rules.rule]]
name = "small-fix"
tier = "low"
keywords = ["typo", "rename", "format"]
max_chars = 300

[[rules.rule]]
name = "big-design"
tier = "high"
keywords = ["architecture", "redesign", "refactor"]

[[rules.rule]]
name = "screenshot"
tier = "medium"
images = true
```

`name` and `tier` are required. A rule matches only when **every** condition it lists is true.
A condition you leave out is not checked. A rule with no conditions matches every message, which
is useful as a last rule.

| Condition | Matches when |
|---|---|
| `keywords` | your message contains at least one of these words or phrases, in any letter case |
| `min_chars`, `max_chars` | your message has at least, or at most, that many characters |
| `min_messages`, `max_messages` | the conversation has at least, or at most, that many messages so far |
| `images` | `true`: your message includes an image. `false`: it includes none |
| `model_contains` | the model Claude Code asked for contains this text, such as `haiku` |

Keywords match anywhere in the text, so `fix` also matches `prefix`. Choose words that are
specific enough.

Put specific rules first. A rule above another one wins when both match.

## What shows in the log

The `routed` and `reason` fields of each request line (see [running it](run.md)):

- `reason=override !high`: a marker decided
- `reason=rule small-fix`: that rule decided
- `reason=no rule matched, default tier`: nothing matched
- `reason=same turn: rule small-fix`: a follow-up request of a turn that was already decided

The log never contains any of your message, only the rule name from your config file.

## What it never does

- It never stores or prints your messages.
- It never changes anything in a request except the model and effort.
- It never blocks a request. If something is wrong with a request, it goes through unchanged.
- It never lowers the model in the middle of an answer.

## When something is wrong

A mistake in a rule, such as a misspelled condition, a missing `tier`, or a `min_chars` larger
than `max_chars`, stops the proxy at startup with a message naming the rule. Nothing is ignored
silently.

Choosing a cheaper model is a guess. If an answer is worse than you wanted, send the message
again with `!high`, or lower the rule's strictness in the config file. Restart the proxy after
changing the file.

## Turning it off

Set `enabled = false` or delete the `[rules]` block, then restart the proxy.

## For developers

The code lives in `src/rules.rs`. `RulesRouter::new(&Config)` implements `Router`. Its
decisions use `DecisionSource::Rules` and always carry the tier's effort. `override_marker`
finds the markers. The rule and condition types are in `src/config.rs` (`Rules`, `Rule`), where
`validate` rejects empty names and keyword lists and impossible ranges. `main.rs` installs the
router only when `rules.enabled` is true. Tiers are looked up with `Tiers::get`.

Rules see only new turns. Continuations are handled before the router is asked (see
`src/conversation.rs`).
