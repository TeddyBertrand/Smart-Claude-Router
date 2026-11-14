//! The labeled prompts the classifier benchmark runs on stay well-formed and balanced.

use std::collections::HashSet;

use serde_json::Value;

const DATASET: &str = include_str!("../bench/classifier-prompts.jsonl");

const KEYS: [&str; 7] = [
    "text",
    "expected",
    "prefix",
    "prefix_repeat",
    "messages",
    "tool_use",
    "images",
];

fn cases() -> Vec<Value> {
    DATASET
        .lines()
        .filter(|l| !l.trim().is_empty())
        .enumerate()
        .map(|(i, line)| {
            serde_json::from_str(line).unwrap_or_else(|e| panic!("line {}: {e}", i + 1))
        })
        .collect()
}

#[test]
fn every_line_is_a_known_shape() {
    for (i, case) in cases().iter().enumerate() {
        let line = i + 1;
        let object = case
            .as_object()
            .unwrap_or_else(|| panic!("line {line}: not an object"));
        for key in object.keys() {
            assert!(
                key == "note" || KEYS.contains(&key.as_str()),
                "line {line}: unknown key {key}"
            );
        }
        assert!(
            object["text"]
                .as_str()
                .is_some_and(|t| !t.trim().is_empty()),
            "line {line}: text"
        );
        assert!(
            matches!(object["expected"].as_str(), Some("low" | "medium" | "high")),
            "line {line}: expected"
        );
        assert_eq!(
            object.contains_key("prefix"),
            object.contains_key("prefix_repeat"),
            "line {line}: prefix and prefix_repeat go together"
        );
    }
}

#[test]
fn there_are_enough_prompts_of_every_tier_and_no_duplicates() {
    let cases = cases();
    assert!(cases.len() >= 50, "{} prompts", cases.len());
    for tier in ["low", "medium", "high"] {
        let count = cases.iter().filter(|c| c["expected"] == tier).count();
        assert!(count >= 10, "{count} prompts for {tier}");
    }
    let mut seen = HashSet::new();
    for case in &cases {
        // The same text may appear twice only with a different conversation shape.
        let key = (case["text"].to_string(), case["messages"].to_string());
        assert!(seen.insert(key), "duplicate prompt: {}", case["text"]);
    }
}

#[test]
fn the_set_covers_the_tricky_cases() {
    let cases = cases();
    assert!(cases
        .iter()
        .any(|c| c.get("prefix").is_some() && c["expected"] == "low"));
    assert!(cases
        .iter()
        .any(|c| c.get("prefix").is_some() && c["expected"] == "high"));
    assert!(cases.iter().any(|c| c["images"].as_u64().unwrap_or(0) > 0));
    // Short but hard.
    assert!(cases.iter().any(|c| {
        c["expected"] == "high" && c["text"].as_str().is_some_and(|t| t.chars().count() < 80)
    }));
}
