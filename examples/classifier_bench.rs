//! Benchmark of local models as the classifier: latency and accuracy on a labeled prompt set.
//!
//! It runs the same code path as the proxy (`condense`, the built-in prompt, `OllamaClient`)
//! against a running Ollama and prints a Markdown report to paste into
//! `docs/classifier-benchmark.md`.
//!
//! ```text
//! cargo run --release --example classifier_bench -- --model llama3.2 --model llama3.1:8b
//! ```
//!
//! The prompts are in `bench/classifier-prompts.jsonl`. They are synthetic and carry the tier a
//! person would expect. Nothing is sent anywhere except to the Ollama at `--url`.

use std::{
    fs,
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::Context;
use axum::http::HeaderMap;
use clap::Parser;
use serde::Deserialize;
use serde_json::Value;
use smart_claude_router::{
    classifier::{condense, Classification, Prompt, PROMPT_VERSION},
    config::{Classifier, Config, Tier},
    ollama::{ClassifyError, OllamaClient},
    router::RequestView,
};

#[derive(Parser)]
struct Args {
    /// Ollama model to measure. Repeat the flag to compare several.
    #[arg(long = "model", required = true)]
    models: Vec<String>,
    #[arg(long, default_value = "http://127.0.0.1:11434")]
    url: String,
    #[arg(long, default_value = "bench/classifier-prompts.jsonl")]
    dataset: PathBuf,
    /// Passes over the whole set. Only the first pass is scored; all passes give latencies.
    #[arg(long, default_value_t = 1)]
    repeat: usize,
    /// Longest wait per call. Generous on purpose: the report shows how many calls would have
    /// missed the 500 ms the proxy uses.
    #[arg(long, default_value_t = 10_000)]
    timeout_ms: u64,
    /// Classifier prompt to use instead of the built-in one.
    #[arg(long)]
    prompt_file: Option<PathBuf>,
    #[arg(long, default_value_t = Classifier::default().max_chars)]
    max_chars: usize,
    /// Answers below this confidence count as "no answer" in the as-deployed numbers.
    #[arg(long, default_value_t = Classifier::default().min_confidence)]
    min_confidence: f64,
    #[arg(long, default_value = "10m")]
    keep_alive: String,
    /// Latency budget the proxy gives the classifier, for the "within budget" line.
    #[arg(long, default_value_t = Classifier::default().timeout_ms)]
    budget_ms: u64,
    /// List the prompts the model got wrong.
    #[arg(long)]
    show_wrong: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    text: String,
    expected: Tier,
    /// Repeated `prefix_repeat` times in front of `text`, to build long messages.
    prefix: Option<String>,
    #[serde(default)]
    prefix_repeat: usize,
    #[serde(default = "one")]
    messages: usize,
    #[serde(default)]
    tool_use: bool,
    #[serde(default)]
    images: usize,
    /// Why the case is interesting. Not used by the benchmark.
    #[serde(default)]
    #[allow(dead_code)]
    note: Option<String>,
}

fn one() -> usize {
    1
}

impl Case {
    fn view(&self) -> RequestView {
        let mut req = RequestView::parse(&HeaderMap::new(), br#"{"model":"claude-opus-5-5"}"#)
            .expect("a minimal body parses");
        let prefix = self.prefix.as_deref().unwrap_or("");
        req.last_user_text = format!("{}{}", prefix.repeat(self.prefix_repeat), self.text);
        req.message_count = self.messages;
        req.has_tool_use = self.tool_use;
        req.image_count = self.images;
        req
    }
}

const TIERS: [&str; 3] = ["low", "medium", "high"];

fn index(tier: Tier) -> usize {
    match tier {
        Tier::Low => 0,
        Tier::Medium => 1,
        Tier::High => 2,
    }
}

struct Outcome {
    expected: usize,
    got: Result<Classification, ClassifyError>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let cases = load(&args.dataset)?;
    let prompt = match &args.prompt_file {
        Some(path) => Prompt::from_file(path)?,
        None => Prompt::builtin(),
    };

    println!("## Run\n");
    print_environment(&args, &cases, &prompt).await;

    let mut summaries = Vec::new();
    for model in &args.models {
        match run_model(&args, model, &cases, &prompt).await {
            Ok(summary) => summaries.push(summary),
            Err(err) => println!("### {model}\n\nSkipped: {err:#}\n"),
        }
    }

    if summaries.len() > 1 {
        println!("## Comparison\n");
        println!(
            "| Model | p50 ms | p95 ms | within {} ms | Accuracy | As deployed | Over-routed | Under-routed | Sent below high |",
            args.budget_ms
        );
        println!("|---|---|---|---|---|---|---|---|---|");
        for s in &summaries {
            println!(
                "| {} | {:.0} | {:.0} | {:.0}% | {:.0}% | {:.0}% | {} | {} | {:.0}% |",
                s.model,
                s.p50,
                s.p95,
                s.within_budget * 100.0,
                s.accuracy * 100.0,
                s.deployed_accuracy * 100.0,
                s.over,
                s.under,
                s.below_high * 100.0
            );
        }
        println!();
    }
    Ok(())
}

fn load(path: &PathBuf) -> anyhow::Result<Vec<Case>> {
    let text =
        fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(i, line)| {
            serde_json::from_str(line).with_context(|| format!("{}:{}", path.display(), i + 1))
        })
        .collect()
}

async fn print_environment(args: &Args, cases: &[Case], prompt: &Prompt) {
    let mut counts = [0usize; 3];
    for case in cases {
        counts[index(case.expected)] += 1;
    }
    let version = ollama_get(&args.url, "/api/version")
        .await
        .and_then(|v| v["version"].as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".into());
    println!("- Ollama: {version} at {}", args.url);
    println!("- CPU: {}", cpu_model());
    println!("- Memory: {}", memory());
    println!(
        "- Prompt: {} ({} estimated tokens), max_chars {}",
        prompt.version(),
        prompt.estimated_tokens(),
        args.max_chars
    );
    if prompt.version() == "custom" {
        println!("- Built-in prompt for comparison: {PROMPT_VERSION}");
    }
    println!(
        "- Prompts: {} in {} (low {}, medium {}, high {}); {} pass(es), first pass scored",
        cases.len(),
        args.dataset.display(),
        counts[0],
        counts[1],
        counts[2],
        args.repeat
    );
    println!(
        "- Always answering `high` would be right {:.0}% of the time\n",
        { counts[2] as f64 / cases.len() as f64 * 100.0 }
    );
}

struct Summary {
    model: String,
    p50: f64,
    p95: f64,
    within_budget: f64,
    accuracy: f64,
    deployed_accuracy: f64,
    over: usize,
    under: usize,
    below_high: f64,
}

async fn run_model(
    args: &Args,
    model: &str,
    cases: &[Case],
    prompt: &Prompt,
) -> anyhow::Result<Summary> {
    let config = Classifier {
        url: args.url.clone(),
        model: model.into(),
        timeout_ms: args.timeout_ms,
        keep_alive: args.keep_alive.clone(),
        ..Classifier::default()
    };
    let client = OllamaClient::new(&config)?;

    let started = Instant::now();
    client
        .warm_up()
        .await
        .map_err(|err| anyhow::anyhow!("cannot load the model: {err}"))?;
    let load_time = started.elapsed();
    let processor = processor(&args.url, model).await;

    let inputs: Vec<(usize, String)> = cases
        .iter()
        .map(|c| (index(c.expected), condense(&c.view(), args.max_chars)))
        .collect();

    // The first calls after loading are slower than the steady state; they are not measured.
    for (_, input) in inputs.iter().take(2) {
        let _ = client.classify(prompt, input).await;
    }

    let mut first_pass = Vec::new();
    let mut latencies = Vec::new();
    for pass in 0..args.repeat.max(1) {
        for (done, (expected, input)) in inputs.iter().enumerate() {
            let call = Instant::now();
            let got = client.classify(prompt, input).await;
            let ms = call.elapsed().as_secs_f64() * 1000.0;
            // A dropped connection ruins the run: the rest would be scored as failures.
            if matches!(got, Err(ClassifyError::Unreachable)) {
                anyhow::bail!(
                    "Ollama stopped answering (pass {}, call {}); results would be meaningless",
                    pass + 1,
                    done + 1
                );
            }
            latencies.push(ms);
            if (done + 1) % 20 == 0 {
                eprintln!(
                    "{model}: pass {} call {}/{}",
                    pass + 1,
                    done + 1,
                    inputs.len()
                );
            }
            if pass == 0 {
                first_pass.push(Outcome {
                    expected: *expected,
                    got,
                });
            }
        }
    }
    latencies.sort_by(f64::total_cmp);

    let summary = report(
        args,
        model,
        cases,
        &first_pass,
        &latencies,
        load_time,
        &processor,
    );
    Ok(summary)
}

fn report(
    args: &Args,
    model: &str,
    cases: &[Case],
    outcomes: &[Outcome],
    sorted_ms: &[f64],
    load_time: Duration,
    processor: &str,
) -> Summary {
    let n = outcomes.len();
    let default_tier = index(Config::default().default_tier);
    let mut matrix = [[0usize; 4]; 3];
    let (mut correct, mut deployed_correct) = (0, 0);
    let (mut over, mut under, mut fallbacks) = (0, 0, 0);
    let mut below_high = 0;
    let mut timeouts = 0;
    let mut invalid = 0;
    let mut other_failures = 0;
    let (mut conf_right, mut conf_wrong) = ((0.0, 0), (0.0, 0));
    let mut wrong = Vec::new();

    for (i, o) in outcomes.iter().enumerate() {
        let deployed = match &o.got {
            Ok(c) => {
                matrix[o.expected][index(c.tier)] += 1;
                if index(c.tier) == o.expected {
                    correct += 1;
                    conf_right = (conf_right.0 + c.confidence, conf_right.1 + 1);
                } else {
                    conf_wrong = (conf_wrong.0 + c.confidence, conf_wrong.1 + 1);
                    wrong.push((i, format!("{} ({:.2})", TIERS[index(c.tier)], c.confidence)));
                }
                if c.confidence >= args.min_confidence {
                    index(c.tier)
                } else {
                    fallbacks += 1;
                    default_tier
                }
            }
            Err(err) => {
                matrix[o.expected][3] += 1;
                fallbacks += 1;
                match err {
                    ClassifyError::Timeout => timeouts += 1,
                    ClassifyError::Answer(_) => invalid += 1,
                    _ => other_failures += 1,
                }
                wrong.push((i, err.label()));
                default_tier
            }
        };
        if deployed == o.expected {
            deployed_correct += 1;
        }
        if deployed > o.expected {
            over += 1;
        }
        if deployed < o.expected {
            under += 1;
        }
        if deployed < 2 {
            below_high += 1;
        }
    }

    let p = |q: f64| percentile(sorted_ms, q);
    let mean = sorted_ms.iter().sum::<f64>() / sorted_ms.len() as f64;
    let within = sorted_ms
        .iter()
        .filter(|ms| **ms <= args.budget_ms as f64)
        .count() as f64
        / sorted_ms.len() as f64;
    let avg = |(sum, count): (f64, usize)| {
        if count == 0 {
            "-".to_string()
        } else {
            format!("{:.2}", sum / count as f64)
        }
    };

    println!("### {model}\n");
    println!(
        "- Load time: {:.1} s; running on {processor}",
        load_time.as_secs_f64()
    );
    println!(
        "- Latency over {} calls: p50 {:.0} ms, p95 {:.0} ms, mean {:.0} ms, max {:.0} ms; {:.0}% within {} ms",
        sorted_ms.len(),
        p(50.0),
        p(95.0),
        mean,
        sorted_ms.last().copied().unwrap_or(0.0),
        within * 100.0,
        args.budget_ms
    );
    println!(
        "- Accuracy: {correct}/{n} = {:.0}%",
        correct as f64 / n as f64 * 100.0
    );
    println!(
        "- As deployed (answers below confidence {} and failures go to the default tier, high): {deployed_correct}/{n} = {:.0}%, over-routed {over}, under-routed {under}, sent below high {:.0}%",
        args.min_confidence,
        deployed_correct as f64 / n as f64 * 100.0,
        below_high as f64 / n as f64 * 100.0
    );
    println!(
        "- Failures: {} (timeouts {timeouts}, invalid answers {invalid}, other {other_failures}); fallbacks used {fallbacks}",
        timeouts + invalid + other_failures
    );
    println!(
        "- Mean confidence: {} when right, {} when wrong\n",
        avg(conf_right),
        avg(conf_wrong)
    );
    println!("| Expected \\ answered | low | medium | high | no answer |");
    println!("|---|---|---|---|---|");
    for (row, name) in matrix.iter().zip(TIERS) {
        println!(
            "| {name} | {} | {} | {} | {} |",
            row[0], row[1], row[2], row[3]
        );
    }
    println!();

    if args.show_wrong && !wrong.is_empty() {
        println!("Wrong answers:\n");
        for (i, answer) in &wrong {
            let case = &cases[*i];
            let shown: String = case.text.chars().take(70).collect();
            println!(
                "- line {}: expected {}, answered {answer}: {shown}",
                i + 1,
                TIERS[index(case.expected)]
            );
        }
        println!();
    }

    Summary {
        model: model.into(),
        p50: p(50.0),
        p95: p(95.0),
        within_budget: within,
        accuracy: correct as f64 / n as f64,
        deployed_accuracy: deployed_correct as f64 / n as f64,
        over,
        under,
        below_high: below_high as f64 / n as f64,
    }
}

/// Nearest-rank percentile of an ascending list.
fn percentile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = (q / 100.0 * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

async fn ollama_get(url: &str, path: &str) -> Option<Value> {
    let response = reqwest::Client::new()
        .get(format!("{}{path}", url.trim_end_matches('/')))
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .ok()?;
    serde_json::from_slice(&response.bytes().await.ok()?).ok()
}

/// Where Ollama says it loaded the model: GPU, CPU or a mix.
async fn processor(url: &str, model: &str) -> String {
    let Some(ps) = ollama_get(url, "/api/ps").await else {
        return "unknown".into();
    };
    let loaded = ps["models"].as_array().into_iter().flatten().find(|m| {
        m["name"]
            .as_str()
            .is_some_and(|name| name == model || name.strip_suffix(":latest") == Some(model))
    });
    match loaded {
        Some(m) => {
            let size = m["size"].as_f64().unwrap_or(0.0);
            let vram = m["size_vram"].as_f64().unwrap_or(0.0);
            if size <= 0.0 || vram <= 0.0 {
                "CPU".into()
            } else if vram >= size {
                "GPU".into()
            } else {
                format!(
                    "{:.0}% GPU / {:.0}% CPU",
                    vram / size * 100.0,
                    100.0 - vram / size * 100.0
                )
            }
        }
        None => "unknown".into(),
    }
}

fn cpu_model() -> String {
    fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|info| {
            info.lines()
                .find_map(|l| l.strip_prefix("model name"))
                .map(|rest| rest.trim_start_matches([' ', '\t', ':']).trim().to_owned())
        })
        .unwrap_or_else(|| "unknown".into())
}

fn memory() -> String {
    fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|info| {
            let kb: f64 = info
                .lines()
                .find_map(|l| l.strip_prefix("MemTotal:"))?
                .trim()
                .trim_end_matches("kB")
                .trim()
                .parse()
                .ok()?;
            Some(format!("{:.0} GB", kb / 1024.0 / 1024.0))
        })
        .unwrap_or_else(|| "unknown".into())
}
