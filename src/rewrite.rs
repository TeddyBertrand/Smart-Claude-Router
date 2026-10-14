//! Applies a [`RoutingDecision`] to a `/v1/messages` request body.
//!
//! The edit is surgical: only the text of the values being changed is replaced, and every other
//! byte of the body stays as Claude Code sent it. Values are copied as raw text, never parsed
//! and printed again, so numbers, escapes and key order cannot drift and the prompt cache
//! behaves as before. (Whitespace between top-level keys is not kept. Claude Code sends none.)
//!
//! Three places can change:
//! - top-level `model`
//! - top-level `output_config.effort`
//! - `output_config.effort` of the latest `role: "system"` message that has one
//!   (see `docs/spike-request-shape.md`)
//!
//! Effort is only changed where the request already has it. `thinking` is never touched.

use std::fmt;

use anyhow::{anyhow, Context};
use serde::{
    de::{MapAccess, Visitor},
    Deserialize, Deserializer,
};
use serde_json::value::RawValue;

use crate::router::RoutingDecision;

/// New body when `decision` changes anything, `None` when the original bytes should be
/// forwarded. An error means the body has an unexpected shape; the caller forwards it unchanged.
pub fn apply(body: &[u8], decision: &RoutingDecision) -> anyhow::Result<Option<Vec<u8>>> {
    let text = std::str::from_utf8(body).context("body is not UTF-8")?;
    let mut changed = false;
    let mut members = Members::parse(text)?;
    for (key, value) in &mut members.0 {
        let new = match key.as_str() {
            "model" => set_model(value, &decision.model)?,
            "output_config" => match decision.effort {
                Some(effort) => set_effort(value, effort.as_str())?,
                None => None,
            },
            "messages" => match decision.effort {
                Some(effort) => set_latest_system_effort(value, effort.as_str())?,
                None => None,
            },
            _ => None,
        };
        if let Some(new) = new {
            *value = new;
            changed = true;
        }
    }
    Ok(changed.then(|| members.render().into_bytes()))
}

fn set_model(value: &str, model: &str) -> anyhow::Result<Option<String>> {
    let current: String = serde_json::from_str(value).context("`model` is not a string")?;
    Ok((current != model).then(|| json_string(model)))
}

/// `value` is an `output_config` object. Replaces its `effort`, if it has one.
fn set_effort(value: &str, effort: &str) -> anyhow::Result<Option<String>> {
    let mut members = Members::parse(value).context("`output_config` is not an object")?;
    let Some((_, current)) = members.0.iter_mut().find(|(key, _)| key == "effort") else {
        return Ok(None);
    };
    let old: String = serde_json::from_str(current).context("`effort` is not a string")?;
    if old == effort {
        return Ok(None);
    }
    *current = json_string(effort);
    Ok(Some(members.render()))
}

/// `value` is the `messages` array. Changes the latest system message that carries an effort.
fn set_latest_system_effort(value: &str, effort: &str) -> anyhow::Result<Option<String>> {
    let messages: Vec<&RawValue> =
        serde_json::from_str(value).context("`messages` is not an array")?;
    for (index, message) in messages.iter().enumerate().rev() {
        let mut members = Members::parse(message.get()).context("a message is not an object")?;
        let is_system = members.0.iter().any(|(key, value)| {
            key == "role" && serde_json::from_str::<String>(value).is_ok_and(|r| r == "system")
        });
        let Some((_, output_config)) = members
            .0
            .iter_mut()
            .find(|(key, _)| key == "output_config")
            .filter(|_| is_system)
        else {
            continue;
        };
        let Some(new) = set_effort(output_config, effort)? else {
            // Latest one with an effort already matches (or has none); older ones are history.
            if has_effort(output_config)? {
                return Ok(None);
            }
            continue;
        };
        *output_config = new;
        let mut parts: Vec<&str> = messages.iter().map(|m| m.get()).collect();
        let rewritten = members.render();
        parts[index] = &rewritten;
        return Ok(Some(format!("[{}]", parts.join(","))));
    }
    Ok(None)
}

fn has_effort(output_config: &str) -> anyhow::Result<bool> {
    Ok(Members::parse(output_config)?
        .0
        .iter()
        .any(|(key, _)| key == "effort"))
}

fn json_string(value: &str) -> String {
    serde_json::to_string(value).expect("a string always serializes")
}

/// The members of one JSON object: keys, and each value as its raw text, in the original order.
struct Members(Vec<(String, String)>);

impl Members {
    fn parse(json: &str) -> anyhow::Result<Self> {
        serde_json::from_str(json).map_err(|e| anyhow!("not a JSON object: {e}"))
    }

    fn render(&self) -> String {
        let members: Vec<String> = self
            .0
            .iter()
            .map(|(key, value)| format!("{}:{value}", json_string(key)))
            .collect();
        format!("{{{}}}", members.join(","))
    }
}

impl<'de> Deserialize<'de> for Members {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct MembersVisitor;

        impl<'de> Visitor<'de> for MembersVisitor {
            type Value = Members;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a JSON object")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Members, A::Error> {
                let mut members = Vec::new();
                while let Some((key, value)) = map.next_entry::<String, Box<RawValue>>()? {
                    members.push((key, value.get().to_owned()));
                }
                Ok(Members(members))
            }
        }

        deserializer.deserialize_map(MembersVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::Effort, router::DecisionSource};

    fn decision(model: &str, effort: Option<Effort>) -> RoutingDecision {
        RoutingDecision {
            model: model.into(),
            effort,
            reason: "test".into(),
            source: DecisionSource::Rules,
        }
    }

    fn apply_str(body: &str, decision: &RoutingDecision) -> Option<String> {
        apply(body.as_bytes(), decision)
            .unwrap()
            .map(|b| String::from_utf8(b).unwrap())
    }

    #[test]
    fn replaces_model_and_keeps_every_other_byte() {
        let body = r#"{"model":"claude-opus-5-5","max_tokens":128000,"temperature":1.0,"messages":[{"role":"user","content":"café  \n"}],"stream":true}"#;
        let out = apply_str(body, &decision("claude-haiku-5-5", None)).unwrap();
        assert_eq!(out, body.replace("claude-opus-5-5", "claude-haiku-5-5"));
    }

    #[test]
    fn unchanged_decision_returns_none() {
        let body = r#"{"model":"claude-opus-5-5","output_config":{"effort":"xhigh"}}"#;
        assert_eq!(
            apply_str(body, &decision("claude-opus-5-5", Some(Effort::Xhigh))),
            None
        );
        assert_eq!(apply_str(body, &decision("claude-opus-5-5", None)), None);
    }

    #[test]
    fn rewrites_top_level_effort_and_keeps_other_output_config_keys() {
        let body =
            r#"{"model":"m","output_config":{"format":{"type":"json_schema"},"effort":"xhigh"}}"#;
        let out = apply_str(body, &decision("m", Some(Effort::Low))).unwrap();
        assert_eq!(
            out,
            r#"{"model":"m","output_config":{"format":{"type":"json_schema"},"effort":"low"}}"#
        );
    }

    #[test]
    fn rewrites_effort_of_latest_system_message_with_one() {
        let body = concat!(
            r#"{"model":"m","messages":["#,
            r#"{"role":"user","content":"a"},"#,
            r#"{"role":"system","content":"old","output_config":{"effort":"max"}},"#,
            r#"{"role":"system","content":"new","output_config":{"effort":"xhigh"}},"#,
            r#"{"role":"user","content":[{"type":"tool_result"}]},"#,
            r#"{"role":"system","content":"no effort"}],"#,
            r#""output_config":{"effort":"xhigh"}}"#
        );
        let out = apply_str(body, &decision("m", Some(Effort::Medium))).unwrap();
        assert_eq!(
            out,
            body.replace(
                r#""content":"new","output_config":{"effort":"xhigh"}"#,
                r#""content":"new","output_config":{"effort":"medium"}"#
            )
            .replace(
                r#""output_config":{"effort":"xhigh"}}"#,
                r#""output_config":{"effort":"medium"}}"#
            )
        );
    }

    #[test]
    fn does_not_add_effort_where_the_request_has_none() {
        let body = r#"{"model":"m","messages":[{"role":"system","content":"x"}],"output_config":{"format":{}}}"#;
        assert_eq!(apply_str(body, &decision("m", Some(Effort::Low))), None);
        let no_config = r#"{"model":"m","messages":[]}"#;
        assert_eq!(
            apply_str(no_config, &decision("m", Some(Effort::Low))),
            None
        );
    }

    #[test]
    fn only_system_messages_count_for_per_message_effort() {
        let body = r#"{"model":"m","messages":[{"role":"user","output_config":{"effort":"max"}}]}"#;
        assert_eq!(apply_str(body, &decision("m", Some(Effort::Low))), None);
    }

    #[test]
    fn unexpected_shapes_are_errors_not_panics() {
        let d = decision("m", Some(Effort::Low));
        for body in [
            &b"not json"[..],
            b"[1]",
            br#"{"model":3}"#,
            br#"{"model":"m","messages":{}}"#,
            br#"{"model":"m","output_config":{"effort":1}}"#,
            b"\xff\xfe",
        ] {
            assert!(
                apply(body, &d).is_err(),
                "{}",
                String::from_utf8_lossy(body)
            );
        }
    }
}
