//! The classifier router: a local model grades each new message and the grade picks the tier.
//!
//! The model gets the condensed message (see [`crate::classifier`]) and answers with a tier, an
//! effort and a confidence. The tier gives the model through the `[tiers]` table. The effort is
//! the tier's configured effort, lowered when the classifier asks for less.
//!
//! When the classifier gives no usable answer (Ollama stopped, too slow, invalid answer) the
//! request is routed to the default tier, so a failure here never fails or delays the request
//! by more than the configured timeout. The reason says why, without any of the message.

use crate::{
    classifier::{condense, Classification, Prompt},
    config::{Config, Tier, Tiers},
    ollama::{ClassifyError, OllamaClient},
    router::{BoxFuture, DecisionSource, RequestView, Router, RoutingDecision},
};

pub struct ClassifierRouter {
    client: OllamaClient,
    prompt: Prompt,
    tiers: Tiers,
    default_tier: Tier,
    max_chars: usize,
}

impl ClassifierRouter {
    pub fn new(config: &Config, client: OllamaClient, prompt: Prompt) -> Self {
        Self {
            client,
            prompt,
            tiers: config.tiers.clone(),
            default_tier: config.default_tier,
            max_chars: config.classifier.max_chars,
        }
    }

    /// Loads the model into Ollama's memory so the first real call is fast.
    pub async fn warm_up(&self) -> Result<(), ClassifyError> {
        self.client.warm_up().await
    }

    pub async fn route(&self, req: &RequestView) -> RoutingDecision {
        let input = condense(req, self.max_chars);
        match self.client.classify(&self.prompt, &input).await {
            Ok(grade) => self.decision(grade),
            Err(err) => {
                tracing::warn!(cause = %err.label(), "classifier failed, using the default tier");
                self.fallback(&err)
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

    fn fallback(&self, err: &ClassifyError) -> RoutingDecision {
        let tier = self.tiers.get(self.default_tier);
        RoutingDecision {
            model: tier.model.clone(),
            effort: Some(tier.effort),
            reason: format!("classifier failed ({}), default tier", err.label()),
            source: DecisionSource::Classifier,
        }
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
