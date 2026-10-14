//! TOML config file: where the proxy listens, where it forwards, and the model tiers routing
//! will choose from (v0.2.0). Every field has a default, so no file at all is a valid setup.
//!
//! Lookup: an explicit path (must exist) > `$XDG_CONFIG_HOME/smart-claude-router/config.toml`
//! (or `~/.config/...`) if present > built-in defaults. `config.example.toml` at the repo root
//! documents every key and must stay equal to the defaults (a test enforces it).

use std::{
    ffi::OsString,
    fs,
    net::SocketAddr,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context};
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Address the proxy listens on. Keep it on localhost: the proxy forwards your login.
    pub listen: SocketAddr,
    /// Base URL requests are forwarded to; the incoming path and query are appended.
    pub upstream: String,
    /// Tier used when routing has no better answer.
    pub default_tier: Tier,
    /// Whether routing decisions are applied to requests or only logged.
    pub mode: Mode,
    pub tiers: Tiers,
    pub rules: Rules,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Routing decisions change the request.
    Route,
    /// Routing decisions are logged, every request is forwarded unchanged (dry run).
    Observe,
}

/// The rules router: an ordered checklist that maps a message to a tier.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Rules {
    /// Off by default: requests pass through untouched until this is `true`.
    pub enabled: bool,
    /// Tried top to bottom; the first one that matches decides.
    pub rule: Vec<Rule>,
}

/// One rule. Every condition it lists has to hold; conditions it leaves out are not checked.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    /// Shown as the reason in the log.
    pub name: String,
    pub tier: Tier,
    /// The message contains at least one of these words or phrases, in any letter case.
    pub keywords: Option<Vec<String>>,
    /// Length of your message in characters.
    pub min_chars: Option<usize>,
    pub max_chars: Option<usize>,
    /// Number of messages in the conversation so far.
    pub min_messages: Option<usize>,
    pub max_messages: Option<usize>,
    /// The message includes at least one image (`true`), or none (`false`).
    pub images: Option<bool>,
    /// The model Claude Code asked for contains this text, e.g. `haiku`.
    pub model_contains: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Low,
    Medium,
    High,
}

/// Each tier is optional in the file; a missing one keeps its default.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Tiers {
    pub low: TierModel,
    pub medium: TierModel,
    pub high: TierModel,
}

/// A tier written in the file must give both its model and its effort.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TierModel {
    pub model: String,
    pub effort: Effort,
}

/// Values accepted by `output_config.effort` (see `docs/spike-request-shape.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl Effort {
    /// The value as written in `output_config.effort`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen: SocketAddr::from(([127, 0, 0, 1], 8899)),
            upstream: "https://api.anthropic.com".into(),
            // What Claude Code already uses on its own, so defaults change nothing.
            default_tier: Tier::High,
            mode: Mode::Route,
            tiers: Tiers::default(),
            rules: Rules::default(),
        }
    }
}

impl Tiers {
    pub fn get(&self, tier: Tier) -> &TierModel {
        match tier {
            Tier::Low => &self.low,
            Tier::Medium => &self.medium,
            Tier::High => &self.high,
        }
    }
}

impl Default for Tiers {
    fn default() -> Self {
        let tier = |model: &str, effort| TierModel {
            model: model.into(),
            effort,
        };
        Self {
            low: tier("claude-haiku-5-5", Effort::Medium),
            medium: tier("claude-sonnet-5-5", Effort::High),
            high: tier("claude-opus-5-5", Effort::Xhigh),
        }
    }
}

impl Config {
    /// Loads the config, returning it with the file it came from (`None` = built-in defaults).
    pub fn load(explicit: Option<&Path>) -> anyhow::Result<(Self, Option<PathBuf>)> {
        let path = match explicit {
            Some(path) => path.to_path_buf(),
            None => match default_path(
                std::env::var_os("XDG_CONFIG_HOME"),
                std::env::var_os("HOME"),
            ) {
                Some(path) if path.exists() => path,
                _ => return Ok((Self::default(), None)),
            },
        };
        let text = fs::read_to_string(&path)
            .with_context(|| format!("cannot read config file {}", path.display()))?;
        let config = Self::from_toml(&text)
            .with_context(|| format!("invalid config file {}", path.display()))?;
        Ok((config, Some(path)))
    }

    pub fn from_toml(text: &str) -> anyhow::Result<Self> {
        let config: Self = toml::from_str(text)?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> anyhow::Result<()> {
        let url = reqwest::Url::parse(&self.upstream)
            .with_context(|| format!("upstream: {:?} is not a URL", self.upstream))?;
        if !matches!(url.scheme(), "http" | "https") || url.host().is_none() {
            bail!(
                "upstream: {:?} must be an http(s) URL with a host",
                self.upstream
            );
        }
        if url.query().is_some() || url.fragment().is_some() {
            bail!(
                "upstream: {:?} must not have a query or fragment",
                self.upstream
            );
        }
        for (name, tier) in [
            ("low", &self.tiers.low),
            ("medium", &self.tiers.medium),
            ("high", &self.tiers.high),
        ] {
            if tier.model.trim().is_empty() {
                bail!("tiers.{name}.model must not be empty");
            }
        }
        for (i, rule) in self.rules.rule.iter().enumerate() {
            let at = format!("rules.rule #{} ({:?})", i + 1, rule.name);
            if rule.name.trim().is_empty() {
                bail!("rules.rule #{}: name must not be empty", i + 1);
            }
            if let Some(keywords) = &rule.keywords {
                if keywords.is_empty() || keywords.iter().any(|k| k.trim().is_empty()) {
                    bail!("{at}: keywords must be a list of non-empty words");
                }
            }
            if rule.model_contains.as_deref().is_some_and(str::is_empty) {
                bail!("{at}: model_contains must not be empty");
            }
            for (low, high, name) in [
                (rule.min_chars, rule.max_chars, "chars"),
                (rule.min_messages, rule.max_messages, "messages"),
            ] {
                if low.zip(high).is_some_and(|(low, high)| low > high) {
                    bail!("{at}: min_{name} is larger than max_{name}, so it can never match");
                }
            }
        }
        Ok(())
    }
}

/// `$XDG_CONFIG_HOME/smart-claude-router/config.toml`, else `$HOME/.config/...`.
fn default_path(xdg_config_home: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    let base = match xdg_config_home.filter(|dir| !dir.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(home.filter(|dir| !dir.is_empty())?).join(".config"),
    };
    Some(base.join("smart-claude-router").join("config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(text: &str) -> String {
        format!("{:#}", Config::from_toml(text).unwrap_err())
    }

    #[test]
    fn empty_file_means_defaults() {
        assert_eq!(Config::from_toml("").unwrap(), Config::default());
    }

    #[test]
    fn example_file_matches_defaults() {
        let example = include_str!("../config.example.toml");
        assert_eq!(Config::from_toml(example).unwrap(), Config::default());
    }

    #[test]
    fn partial_file_keeps_other_defaults() {
        let config = Config::from_toml(
            r#"
            listen = "127.0.0.1:9000"

            [tiers.low]
            model = "claude-haiku-4-5"
            effort = "low"
            "#,
        )
        .unwrap();
        assert_eq!(config.listen.port(), 9000);
        assert_eq!(config.tiers.low.model, "claude-haiku-4-5");
        assert_eq!(config.tiers.low.effort, Effort::Low);
        assert_eq!(config.upstream, Config::default().upstream);
        assert_eq!(config.tiers.high, Tiers::default().high);
    }

    #[test]
    fn mistakes_are_named_in_the_error() {
        assert!(err("lisen = \"127.0.0.1:1\"").contains("lisen"));
        assert!(err("[tiers.ultra]\nmodel = \"x\"\neffort = \"low\"").contains("ultra"));
        assert!(err("[tiers.low]\nmodel = \"x\"\neffort = \"huge\"").contains("huge"));
        assert!(err("[tiers.low]\nmodel = \"x\"").contains("effort"));
        assert!(err("default_tier = \"top\"").contains("top"));
        assert!(err("mode = \"watch\"").contains("watch"));
        assert!(err("listen = \"localhost\"").contains("listen"));
        assert!(err("[tiers.high]\nmodel = \" \"\neffort = \"max\"").contains("tiers.high.model"));
    }

    #[test]
    fn mode_defaults_to_route_and_accepts_observe() {
        assert_eq!(Config::default().mode, Mode::Route);
        assert_eq!(
            Config::from_toml("mode = \"observe\"").unwrap().mode,
            Mode::Observe
        );
        assert_eq!(
            Config::from_toml("mode = \"route\"").unwrap().mode,
            Mode::Route
        );
    }

    #[test]
    fn rules_are_read_in_order_and_off_by_default() {
        assert!(!Config::default().rules.enabled);
        let config = Config::from_toml(
            r#"
            [rules]
            enabled = true

            [[rules.rule]]
            name = "typos"
            tier = "low"
            keywords = ["typo", "rename"]
            max_chars = 200

            [[rules.rule]]
            name = "big"
            tier = "high"
            min_messages = 20
            images = false
            model_contains = "opus"
            "#,
        )
        .unwrap();
        let rules = config.rules;
        assert!(rules.enabled);
        assert_eq!(rules.rule.len(), 2);
        assert_eq!(rules.rule[0].name, "typos");
        assert_eq!(rules.rule[0].tier, Tier::Low);
        assert_eq!(rules.rule[0].max_chars, Some(200));
        assert_eq!(rules.rule[1].min_messages, Some(20));
        assert_eq!(rules.rule[1].images, Some(false));
        assert_eq!(rules.rule[1].keywords, None);
    }

    #[test]
    fn rule_mistakes_are_named_in_the_error() {
        let rule = |body: &str| err(&format!("[[rules.rule]]\n{body}"));
        assert!(rule("name = \"a\"").contains("tier"));
        assert!(rule("tier = \"low\"").contains("name"));
        assert!(rule("name = \"a\"\ntier = \"low\"\nkeyword = [\"x\"]").contains("keyword"));
        assert!(rule("name = \"a\"\ntier = \"top\"").contains("top"));
        assert!(rule("name = \" \"\ntier = \"low\"").contains("name must not be empty"));
        assert!(rule("name = \"a\"\ntier = \"low\"\nkeywords = []").contains("keywords"));
        assert!(rule("name = \"a\"\ntier = \"low\"\nkeywords = [\"\"]").contains("keywords"));
        assert!(
            rule("name = \"a\"\ntier = \"low\"\nmin_chars = 9\nmax_chars = 1")
                .contains("min_chars is larger than max_chars")
        );
        assert!(
            rule("name = \"a\"\ntier = \"low\"\nmin_messages = 9\nmax_messages = 1")
                .contains("min_messages is larger than max_messages")
        );
    }

    #[test]
    fn upstream_must_be_a_plain_http_base_url() {
        assert!(Config::from_toml("upstream = \"http://127.0.0.1:1234/prefix\"").is_ok());
        for bad in [
            "api.anthropic.com",
            "ftp://example.com",
            "https://x.dev/?a=1",
        ] {
            assert!(
                err(&format!("upstream = {bad:?}")).contains("upstream"),
                "{bad}"
            );
        }
    }

    #[test]
    fn default_path_prefers_xdg_then_home() {
        let path = |xdg: Option<&str>, home: Option<&str>| {
            default_path(xdg.map(Into::into), home.map(Into::into))
        };
        let file = Path::new("smart-claude-router/config.toml");
        assert_eq!(
            path(Some("/x"), Some("/h")),
            Some(Path::new("/x").join(file))
        );
        assert_eq!(
            path(Some(""), Some("/h")),
            Some(Path::new("/h/.config").join(file))
        );
        assert_eq!(path(None, None), None);
    }

    #[test]
    fn explicit_path_must_exist_and_be_valid() {
        let dir = std::env::temp_dir().join(format!("scr-config-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();

        let missing = dir.join("missing.toml");
        let msg = format!("{:#}", Config::load(Some(&missing)).unwrap_err());
        assert!(msg.contains("missing.toml"), "{msg}");

        let broken = dir.join("broken.toml");
        fs::write(&broken, "listen = 1").unwrap();
        let msg = format!("{:#}", Config::load(Some(&broken)).unwrap_err());
        assert!(
            msg.contains("broken.toml") && msg.contains("listen"),
            "{msg}"
        );

        let good = dir.join("good.toml");
        fs::write(&good, "listen = \"127.0.0.1:9001\"").unwrap();
        let (config, source) = Config::load(Some(&good)).unwrap();
        assert_eq!((config.listen.port(), source), (9001, Some(good)));

        fs::remove_dir_all(&dir).unwrap();
    }
}
