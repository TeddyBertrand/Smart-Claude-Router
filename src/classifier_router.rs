//! The classifier router: a local model grades each new message and the grade picks the tier.
//!
//! The model gets the condensed message (see [`crate::classifier`]) and answers with a tier, an
//! effort and a confidence. The tier gives the model through the `[tiers]` table. The effort is
//! the tier's configured effort, lowered when the classifier asks for less.
//!
//! When the classifier gives no usable answer, the fallback decides instead: the rules router if
//! it is enabled, else the default tier. That happens when the call fails (Ollama stopped, too
//! slow, invalid answer), when the answer is less confident than `min_confidence`, and while the
//! circuit breaker has paused the classifier after repeated failures. The decision's source is
//! `Fallback` and its reason names the cause, never any of the message. Added latency never
//! exceeds `timeout_ms`, and is zero while the classifier is paused.

use std::time::{Duration, Instant};

use crate::{
    breaker::{Breaker, Change},
    classifier::{condense, Classification, Prompt},
    config::{Config, Tier, Tiers},
    ollama::{ClassifyError, OllamaClient},
    router::{BoxFuture, DecisionSource, RequestView, Router, RoutingDecision},
    rules::RulesRouter,
};

pub struct ClassifierRouter {
    client: OllamaClient,
    prompt: Prompt,
    tiers: Tiers,
    default_tier: Tier,
    max_chars: usize,
    min_confidence: f64,
    /// Decides when the classifier cannot. `None` means the default tier.
    rules: Option<RulesRouter>,
    breaker: Breaker,
    cooldown: Duration,
}

impl ClassifierRouter {
    pub fn new(config: &Config, client: OllamaClient, prompt: Prompt) -> Self {
        let classifier = &config.classifier;
        let cooldown = Duration::from_secs(classifier.cooldown_secs);
        let timeout = Duration::from_millis(classifier.timeout_ms);
        Self {
            client,
            prompt,
            tiers: config.tiers.clone(),
            default_tier: config.default_tier,
            max_chars: classifier.max_chars,
            min_confidence: classifier.min_confidence,
            rules: config.rules.enabled.then(|| RulesRouter::new(config)),
            // While paused, one call per timeout window checks whether Ollama is back.
            breaker: Breaker::new(classifier.failure_limit, cooldown, timeout),
            cooldown,
        }
    }

    /// Loads the model into Ollama's memory so the first real call is fast.
    pub async fn warm_up(&self) -> Result<(), ClassifyError> {
        self.client.warm_up().await
    }

    pub async fn route(&self, req: &RequestView) -> RoutingDecision {
        if !self.breaker.allow(Instant::now()) {
            tracing::debug!("classifier paused, using the fallback");
            return self.fallback(req, "paused after repeated failures");
        }
        let input = condense(req, self.max_chars);
        match self.client.classify(&self.prompt, &input).await {
            Ok(grade) => {
                if self.breaker.record_success() == Change::Closed {
                    tracing::info!("classifier answering again");
                }
                if grade.confidence < self.min_confidence {
                    return self.fallback(
                        req,
                        &format!(
                            "confidence {:.2} below {:.2}",
                            grade.confidence, self.min_confidence
                        ),
                    );
                }
                self.decision(grade)
            }
            Err(err) => {
                if self.breaker.record_failure(Instant::now()) == Change::Opened {
                    tracing::warn!(
                        cause = %err.label(),
                        "classifier failed, pausing it for {}s",
                        self.cooldown.as_secs()
                    );
                } else {
                    tracing::warn!(cause = %err.label(), "classifier failed, using the fallback");
                }
                self.fallback(req, &err.label())
            }
        }
    }

    fn decision(&self, grade: Classification) -> RoutingDecision {
        let tier = self.tiers.get(grade.tier);
        RoutingDecision {
            model: tier.model.clone(),
            // Never above what the tier is configured for.
            effort: Some(tier.effort.min(grade.effort)),
            reason: format!(
                "classifier {}, confidence {:.2}",
                tier_name(grade.tier),
                grade.confidence
            ),
            source: DecisionSource::Classifier,
        }
    }

    fn fallback(&self, req: &RequestView, cause: &str) -> RoutingDecision {
        let mut decision = match &self.rules {
            Some(rules) => rules.route(req),
            None => {
                let tier = self.tiers.get(self.default_tier);
                RoutingDecision {
                    model: tier.model.clone(),
                    effort: Some(tier.effort),
                    reason: "default tier".into(),
                    source: DecisionSource::Fallback,
                }
            }
        };
        decision.reason = format!("classifier fallback ({cause}): {}", decision.reason);
        decision.source = DecisionSource::Fallback;
        decision
    }
}

impl Router for ClassifierRouter {
    fn decide<'a>(&'a self, req: &'a RequestView) -> BoxFuture<'a, RoutingDecision> {
        Box::pin(self.route(req))
    }
}

fn tier_name(tier: Tier) -> &'static str {
    match tier {
        Tier::Low => "low",
        Tier::Medium => "medium",
        Tier::High => "high",
    }
}
