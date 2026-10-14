# The classifier prompt

## What it is

The classifier is a small AI model that will read each new message you send and say how hard the
work is. This page describes what that model is told, what it is shown of your message, and what
it must answer. The classifier itself (the Ollama client) is not part of the proxy yet. Nothing
described here runs today, and no request is changed by it.

## What the model is shown

For each new message you send, the model gets:

- Your message text, cut to `max_chars` characters if it is longer (600 by default). A cut
  message keeps its first two thirds and its last third, with the middle removed. The end is
  kept because a long pasted log is often followed by the actual request.
- How many characters your message had before the cut, and whether it was cut.
- How many messages the conversation has so far.
- Whether the assistant already used tools in this conversation.
- How many images are attached.

It is never shown headers, your login, the system prompt, earlier messages, file contents or
tool output. The same message always produces the same text.

Your message is passed as a quoted string. Quotes or line breaks inside it cannot be mistaken
for the other fields. The prompt also tells the model to treat the message only as text to
grade, not as instructions.

## What the model must answer

One line of JSON:

```json
{"tier": "medium", "effort": "low", "confidence": 0.8}
```

- `tier`: `low`, `medium` or `high`. The model the work needs.
- `effort`: `low`, `medium` or `high`. How long that model should think.
- `confidence`: a number from 0 to 1.

The answer is checked strictly. A missing field, an extra field, an unknown value, an effort
of `xhigh` or `max`, or a confidence outside 0 to 1 makes the whole answer invalid. An invalid
answer is never used to change a request.

## The prompt

The built-in prompt is the file `prompts/classifier-v1.md` in the repository. It defines the
three tiers, says to grade the difficulty of the work and not the length of the message, and
gives eight examples. Two of them are the tricky cases: a short question that is hard, and a
long pasted log that ends in "that fixed it, thanks", which is easy.

The prompt has a version (`v1`). Every change to the prompt or to the answer format gets a new
version, so benchmark results can be matched to the prompt that produced them.

The prompt is kept under about 900 tokens, because the classifier runs before every new
message and a long prompt makes it slower. The current prompt is about 780.

## Settings

In the `[classifier]` block of [the config file](config.md):

| Setting | Default | What it does |
|---|---|---|
| `max_chars` | `600` | Most characters of your message the model sees. At least 20. |
| `prompt_file` | not set | Path to your own prompt file, replacing the built-in one. |

If you use your own prompt, it must tell the model to answer in the format above. Examples
inside it are your responsibility; the built-in test that checks them only covers the built-in
prompt. Restart the proxy after changing a setting or the file.

## What it never does

- It never sends anything anywhere by itself. This part only builds text and reads answers.
- It never logs or stores your message.
- It never accepts a partly valid answer.

## When something is wrong

A `prompt_file` that is missing or empty is reported with its path. A `max_chars` below 20 or a
misspelled setting stops the proxy at startup with a message naming the setting.

## Turning it off

Delete the `[classifier]` block. The defaults are used, and until the classifier router exists
they have no effect.

## For developers

The code lives in `src/classifier.rs`:

- `Prompt::builtin()` / `Prompt::load(&Classifier)` give the prompt text and its version.
  `PROMPT_TOKEN_BUDGET` is the limit and `estimated_tokens()` counts 4 characters per token; a
  test keeps the built-in prompt under it.
- `condense(&RequestView, max_chars)` builds the model's input. It uses `last_user_text`,
  `message_count`, `has_tool_use` and `image_count` of the view, and cuts by characters, not
  bytes.
- `output_schema()` is the JSON schema for backends that constrain the output, such as the
  `format` field of Ollama's chat API.
- `Classification::parse(&str)` is the strict reader. `ClassificationError` separates invalid
  JSON, a disallowed effort and an out-of-range confidence, so the fallback code can record the
  cause.

The settings are `Classifier` in `src/config.rs`. The prompt's example answers are checked by a
test against `Classification::parse`, so the prompt and the schema cannot drift apart.
