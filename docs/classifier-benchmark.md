# Classifier model benchmark

## What it is

A benchmark that measures how fast and how accurate a model is at grading messages for
[the classifier router](classifier.md). It sends 80 example messages with a known right answer to
a model, through the same code the proxy uses, and reports the time each answer took and how
often the answer was right.

This page also keeps the results of every run, so the choice of default model can be traced back
to numbers. Each run is added to the log below with its date, machine and prompt version.

## Why it exists

Choosing the model and the prompt by feel is a guess. A wrong grade costs something either way:

- A grade that is too high sends an easy message to an expensive model and wastes quota.
- A grade that is too low sends a hard message to a cheap model and can give a worse answer.

The classifier also runs before every new message, so its time is added to every message you
send.

## What is measured

For each model:

- **Latency**: how long one grade takes, as p50 (half of the calls are faster), p95 (95% are
  faster), the mean and the slowest. It also shows the share of calls within 500 ms, which is
  the time the proxy waits by default.
- **Accuracy**: how many messages got the expected tier.
- **As deployed**: the same, but the way the proxy would use the answers. An answer with a
  confidence below `min_confidence`, or no answer at all, goes to the default tier (`high`).
- **Over-routed and under-routed**: as deployed, how many messages went to a higher or a lower
  tier than expected. Over-routing wastes quota. Under-routing risks a worse answer.
- **Sent below high**: the share of messages that would go to a cheaper model. This is where
  the quota saving comes from.
- **Confusion matrix**: for each expected tier, which tier the model answered.
- **Mean confidence** when the answer was right and when it was wrong. If the two are close,
  the confidence says nothing about whether to trust the answer.

## The messages

The 80 messages are in `bench/classifier-prompts.jsonl`: 26 expected `low`, 28 `medium`, 26
`high`. They are written for this benchmark and contain nothing private. Always answering `high`
would be right 32% of the time.

The expected tier follows the same rule as the classifier prompt:

- **low**: mechanical or tiny work. Typos, renames, formatting, one-line edits, short factual
  questions, "ok", "thanks", "commit this".
- **medium**: ordinary work in a known place. A small feature, a clear bug fix, tests,
  explaining a file, a documentation or CI change.
- **high**: work that needs deep reasoning. Design, architecture, bugs that are hard to
  explain (deadlocks, races, memory growth), large refactors or migrations, security, vague
  large goals.

The set includes the hard cases: short messages that are hard, long pasted logs followed by a
trivial request, long logs followed by a hard one, a message with an image, bare confirmations
("yes", "ok") in long conversations, and six messages in French.

The labels are one person's judgement. Some messages sit near a boundary, so a model that
disagrees on those is not necessarily wrong. Compare models on the same set, and treat small
differences in accuracy as noise.

## How to run it

Ollama must be running with the model pulled. One command:

```
cargo run --release --example classifier_bench -- --model llama3.2
```

(On Nix, put `nix develop -c` in front.) Repeat `--model` to compare several. Useful options:

| Option | What it does |
|---|---|
| `--repeat N` | Run N passes over the messages. Only the first is scored for accuracy; all give latencies. |
| `--show-wrong` | List the messages the model got wrong. |
| `--prompt-file PATH` | Try a different classifier prompt. |
| `--max-chars N` | Change how much of a message the model sees. |
| `--url URL` | Use an Ollama on another address. |

The report is Markdown. Paste it into the log below with the date. Run nothing else heavy on the
machine during a run, because it changes the latencies.

If the connection to Ollama drops during a run, the run for that model stops with an error
instead of reporting the rest as failures.

## Results log

### 2026-10-14: llama3.2 (3B)

The only model that completed.

- Machine: Intel Core i7-13650HX, 31 GB memory, **no GPU**. The model ran on the CPU.
- Ollama 0.33.3. Classifier prompt `v1`, `max_chars` 600. Two passes over 80 messages.

| | |
|---|---|
| Latency (160 calls) | p50 1539 ms, p95 1928 ms, mean 1604 ms, max 4392 ms |
| Within 500 ms | 0% |
| Accuracy | 68 of 80, 85% |
| As deployed | 67 of 80, 84%. Over-routed 3, under-routed 10. 71% sent below high. |
| Failures | 0. 8 answers used the default tier because of low confidence. |
| Mean confidence | 0.80 when right, 0.81 when wrong |

| Expected \ answered | low | medium | high |
|---|---|---|---|
| low | 24 | 2 | 0 |
| medium | 5 | 23 | 0 |
| high | 0 | 5 | 21 |

The mistakes lean towards too low: 5 `medium` messages went to `low`, and 5 `high` messages went
to `medium`. Among them were "refactor the monolith into services", "the tokenizer gives
different results on ARM and x86, why?" and "can you review this function for bugs?".

### 2026-10-14 and 2026-10-15: qwen3.5:4b and llama3.1:8b (not completed)

No usable numbers. Three runs were started and none finished:

- The first run stalled on `qwen3.5:4b` and was stopped by hand.
- The second lost its connection to Ollama after 12 calls, because Ollama had stopped. Its
  results were discarded. This is why the benchmark now stops a run when the connection drops.
- The third was cut short when the session ended.

What was seen while they ran: `qwen3.5:4b` took more than 4 seconds per call on this machine,
against about 1.5 seconds for `llama3.2`. One separate test call took 4.2 s the first time and
1.4 s once the prompt was cached. `llama3.1:8b` (8B) was not measured.

## What the results say

- **On a CPU-only machine a local model is too slow for the default wait.** The fastest
  model measured needs about 1.5 s per message and none of its calls fit in 500 ms. With the
  default `timeout_ms = 500` every call would time out, and after 3 failures in a row the
  classifier would pause for 30 seconds. On this machine, a local classifier adds seconds to
  every message or does nothing.
- **Most of the time is spent writing the answer.** The answer is about 18 tokens and each
  takes about 70 ms on this CPU. A shorter answer would help more than a shorter prompt, but it
  was not tried.
- **The prompt is longer than estimated.** Ollama counted 949 tokens for the prompt with
  `qwen3.5:4b`. The 4-characters-per-token estimate gave 778, so it undercounts by about a
  fifth, and the real size is above the 900 budget. Other models count differently.
- **The confidence is not useful for this model.** It was 0.80 when right and 0.81 when wrong,
  so the `min_confidence` threshold does not separate good answers from bad ones.
- **Accuracy of 85% is decent, but 10 of 80 went too low.** That is the costly kind of
  mistake.
- **The default model is not chosen.** `qwen2.5:3b` in the config is a placeholder that was
  never measured. The default will be set after the Haiku backend (#50) has been measured on
  the same messages, so that the choice between a local model and Haiku rests on numbers.

## What it never does

- It never reads your conversations. The messages are the ones in the repository.
- It never sends anything except to the Ollama address you give it.
- It never changes the proxy or its config.

## For developers

- `examples/classifier_bench.rs`: the benchmark. It builds each message into a `RequestView`,
  condenses it with `classifier::condense` and grades it with `OllamaClient::classify`, the
  same path as `ClassifierRouter`. The first two calls after loading the model are not
  measured. Percentiles are nearest-rank. Progress goes to stderr and the report to stdout.
- `bench/classifier-prompts.jsonl`: one JSON object per line. `text`, `expected` are required.
  `messages`, `tool_use`, `images` describe the conversation. `prefix` repeated `prefix_repeat`
  times is put in front of `text` to make long messages. `note` is free text.
- `tests/bench_dataset.rs` checks that every line has a known shape, that every tier has at
  least 10 messages, that there are no duplicates and that the tricky cases are still present.
