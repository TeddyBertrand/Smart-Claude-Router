//! The rules router: a checklist from the config file that maps a message to a tier.
//!
//! Order of decision, first hit wins:
//! 1. An override marker in the message (`!low`, `!medium` or `!high`).
//! 2. The configured rules, top to bottom. A rule matches when every condition it lists holds.
//! 3. The default tier.
//!
//! The chosen tier gives the model and the effort. The reason says which step decided, naming
//! the rule, and never contains any of the message.

use crate::{
    config::{Config, Rule, Tier, Tiers},
    router::{BoxFuture, DecisionSource, RequestView, Router, RoutingDecision},
};

pub struct RulesRouter {
    tiers: Tiers,
    default_tier: Tier,
    rules: Vec<CompiledRule>,
}

struct CompiledRule {
    rule: Rule,
    /// Lowercased, so matching ignores letter case.
    keywords: Option<Vec<String>>,
    model_contains: Option<String>,
}

impl RulesRouter {
    pub fn new(config: &Config) -> Self {
        let rules = config
            .rules
            .rule
            .iter()
            .map(|rule| CompiledRule {
                keywords: rule
                    .keywords
                    .as_ref()
                    .map(|words| words.iter().map(|w| w.to_lowercase()).collect()),
                model_contains: rule.model_contains.as_ref().map(|m| m.to_lowercase()),
                rule: rule.clone(),
            })
            .collect();
        Self {
            tiers: config.tiers.clone(),
            default_tier: config.default_tier,
            rules,
        }
    }

    pub fn route(&self, req: &RequestView) -> RoutingDecision {
        let (tier, reason) = if let Some(tier) = override_marker(&req.last_user_text) {
            (tier, format!("override !{}", tier_name(tier)))
        } else if let Some(rule) = self.first_match(req) {
            (rule.tier, format!("rule {}", rule.name))
        } else {
            (self.default_tier, "no rule matched, default tier".into())
        };
        let model = self.tiers.get(tier);
        RoutingDecision {
            model: model.model.clone(),
            effort: Some(model.effort),
            reason,
            source: DecisionSource::Rules,
        }
    }

    fn first_match(&self, req: &RequestView) -> Option<&Rule> {
        let text = req.last_user_text.to_lowercase();
        let model = req.model.to_lowercase();
        let chars = req.last_user_text.chars().count();
        self.rules
            .iter()
            .find(|r| r.matches(&text, &model, chars, req))
            .map(|r| &r.rule)
    }
}

impl CompiledRule {
    fn matches(&self, text: &str, model: &str, chars: usize, req: &RequestView) -> bool {
        let rule = &self.rule;
        self.keywords
            .as_ref()
            .is_none_or(|words| words.iter().any(|w| text.contains(w.as_str())))
            && rule.min_chars.is_none_or(|n| chars >= n)
            && rule.max_chars.is_none_or(|n| chars <= n)
            && rule.min_messages.is_none_or(|n| req.message_count >= n)
            && rule.max_messages.is_none_or(|n| req.message_count <= n)
            && rule.images.is_none_or(|want| (req.image_count > 0) == want)
            && self
                .model_contains
                .as_ref()
                .is_none_or(|part| model.contains(part.as_str()))
    }
}

impl Router for RulesRouter {
    fn decide<'a>(&'a self, req: &'a RequestView) -> BoxFuture<'a, RoutingDecision> {
        Box::pin(async move { self.route(req) })
    }
}

/// The last `!low`, `!medium` or `!high` written as a word of its own, in any letter case.
/// Punctuation right after it is fine (`!low.`).
pub fn override_marker(text: &str) -> Option<Tier> {
    text.split_whitespace()
        .filter_map(|word| {
            let word = word.trim_end_matches(['.', ',', ';', ':', '?', ')']);
            match word.to_ascii_lowercase().as_str() {
                "!low" => Some(Tier::Low),
                "!medium" => Some(Tier::Medium),
                "!high" => Some(Tier::High),
                _ => None,
            }
        })
        .next_back()
}

fn tier_name(tier: Tier) -> &'static str {
    match tier {
        Tier::Low => "low",
        Tier::Medium => "medium",
        Tier::High => "high",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Effort;
    use axum::http::HeaderMap;
    use serde_json::json;

    fn router(rules_toml: &str) -> RulesRouter {
        let config =
            Config::from_toml(&format!("default_tier = \"medium\"\n{rules_toml}")).unwrap();
        RulesRouter::new(&config)
    }

    fn view(text: &str) -> RequestView {
        view_with(text, 1, 0, "claude-opus-5-5")
    }

    fn view_with(text: &str, messages: usize, images: usize, model: &str) -> RequestView {
        let mut view = RequestView::parse(
            &HeaderMap::new(),
            json!({"model": model, "messages": [{"role": "user", "content": text}]})
                .to_string()
                .as_bytes(),
        )
        .unwrap();
        view.message_count = messages;
        view.image_count = images;
        view
    }

    fn decided(router: &RulesRouter, view: &RequestView) -> (String, String) {
        let d = router.route(view);
        (d.model, d.reason)
    }

    #[test]
    fn override_markers_are_found_as_words() {
        assert_eq!(override_marker("fix this !low"), Some(Tier::Low));
        assert_eq!(override_marker("!HIGH please"), Some(Tier::High));
        assert_eq!(override_marker("do it !medium."), Some(Tier::Medium));
        assert_eq!(override_marker("(see !low)"), Some(Tier::Low));
        assert_eq!(override_marker("!low then !high"), Some(Tier::High));
        assert_eq!(override_marker("nothing here"), None);
        assert_eq!(override_marker("a!low b !lower !highest !!low"), None);
        assert_eq!(override_marker(""), None);
    }

    #[test]
    fn no_match_uses_the_default_tier_and_its_effort() {
        let r = router("");
        let d = r.route(&view("hello"));
        assert_eq!(d.model, "claude-sonnet-5-5");
        assert_eq!(d.effort, Some(Effort::High));
        assert_eq!(d.reason, "no rule matched, default tier");
        assert_eq!(d.source, DecisionSource::Rules);
    }

    #[test]
    fn each_condition_decides_on_its_own() {
        // (rules, view, expected model)
        let cases: Vec<(&str, RequestView, &str)> = vec![
            (
                "keywords = [\"typo\", \"RENAME\"]",
                view("please fix a Typo"),
                "claude-haiku-5-5",
            ),
            (
                "keywords = [\"typo\", \"rename\"]",
                view("write a parser"),
                "claude-sonnet-5-5",
            ),
            ("max_chars = 5", view("short"), "claude-haiku-5-5"),
            ("max_chars = 5", view("too long"), "claude-sonnet-5-5"),
            ("min_chars = 6", view("exactly"), "claude-haiku-5-5"),
            ("min_chars = 6", view("tiny"), "claude-sonnet-5-5"),
            (
                "min_messages = 10",
                view_with("x", 10, 0, "claude-opus-5-5"),
                "claude-haiku-5-5",
            ),
            (
                "min_messages = 10",
                view_with("x", 9, 0, "claude-opus-5-5"),
                "claude-sonnet-5-5",
            ),
            (
                "max_messages = 2",
                view_with("x", 2, 0, "claude-opus-5-5"),
                "claude-haiku-5-5",
            ),
            (
                "max_messages = 2",
                view_with("x", 3, 0, "claude-opus-5-5"),
                "claude-sonnet-5-5",
            ),
            (
                "images = true",
                view_with("x", 1, 1, "claude-opus-5-5"),
                "claude-haiku-5-5",
            ),
            ("images = true", view("x"), "claude-sonnet-5-5"),
            ("images = false", view("x"), "claude-haiku-5-5"),
            (
                "images = false",
                view_with("x", 1, 2, "claude-opus-5-5"),
                "claude-sonnet-5-5",
            ),
            ("model_contains = \"OPUS\"", view("x"), "claude-haiku-5-5"),
            ("model_contains = \"haiku\"", view("x"), "claude-sonnet-5-5"),
        ];
        for (conditions, view, expected) in cases {
            let r = router(&format!(
                "[[rules.rule]]\nname = \"r\"\ntier = \"low\"\n{conditions}"
            ));
            assert_eq!(r.route(&view).model, expected, "{conditions}");
        }
    }

    #[test]
    fn a_rule_needs_all_its_conditions() {
        let r = router(
            "[[rules.rule]]\nname = \"r\"\ntier = \"low\"\nkeywords = [\"typo\"]\nmax_chars = 20",
        );
        let reason = |text: &str| decided(&r, &view(text)).1;
        assert_eq!(reason("fix typo"), "rule r");
        assert_eq!(
            reason("fix this typo in the long file"),
            "no rule matched, default tier"
        );
        assert_eq!(reason("fix this"), "no rule matched, default tier");
    }

    #[test]
    fn the_first_matching_rule_wins_and_is_named() {
        let r = router(
            "[[rules.rule]]\nname = \"first\"\ntier = \"low\"\nkeywords = [\"fix\"]\n\
             [[rules.rule]]\nname = \"second\"\ntier = \"high\"\nkeywords = [\"fix\", \"design\"]\n\
             [[rules.rule]]\nname = \"catch-all\"\ntier = \"high\"",
        );
        assert_eq!(
            decided(&r, &view("fix it")),
            ("claude-haiku-5-5".into(), "rule first".into())
        );
        assert_eq!(
            decided(&r, &view("design it")),
            ("claude-opus-5-5".into(), "rule second".into())
        );
        assert_eq!(
            decided(&r, &view("anything")),
            ("claude-opus-5-5".into(), "rule catch-all".into())
        );
    }

    #[test]
    fn override_markers_beat_every_rule() {
        let r = router("[[rules.rule]]\nname = \"typos\"\ntier = \"low\"\nkeywords = [\"typo\"]");
        let d = r.route(&view("fix this typo !high"));
        assert_eq!(d.model, "claude-opus-5-5");
        assert_eq!(d.effort, Some(Effort::Xhigh));
        assert_eq!(d.reason, "override !high");
        assert_eq!(
            r.route(&view("big redesign !low")).model,
            "claude-haiku-5-5"
        );
    }

    #[test]
    fn tiers_from_the_config_are_used() {
        let r = router("[tiers.low]\nmodel = \"my-small\"\neffort = \"max\"");
        let d = r.route(&view("x !low"));
        assert_eq!(
            (d.model.as_str(), d.effort),
            ("my-small", Some(Effort::Max))
        );
    }

    #[test]
    fn reasons_never_contain_the_message() {
        let r = router("[[rules.rule]]\nname = \"r\"\ntier = \"low\"\nkeywords = [\"secret\"]");
        for text in [
            "my SECRET plan",
            "unrelated secretive",
            "my SECRET plan !low",
        ] {
            assert!(!r
                .route(&view(text))
                .reason
                .to_lowercase()
                .contains("secret plan"));
        }
    }
}
