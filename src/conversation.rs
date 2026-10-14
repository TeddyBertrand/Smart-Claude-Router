//! Remembers the routing decision of each conversation, so that one user turn stays on one model.
//!
//! A user turn is many requests (see [`Turn`]). Only the first one asks the router. The decision
//! is kept here and reused for the rest of the turn. The memory is bounded: entries expire after
//! a while without use, and the least recently used one is dropped when the store is full.
//! Nothing is written to disk.

use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

use crate::router::{RequestView, RoutingDecision, Turn};

/// How long an idle conversation is remembered. A turn that pauses longer than this (a very slow
/// tool) is no longer recognised and its remaining requests go through unrouted.
pub const DEFAULT_TTL: Duration = Duration::from_secs(60 * 60);
pub const DEFAULT_MAX_ENTRIES: usize = 1024;

/// A session id, plus the agent id for subagent requests, which share their parent's session.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ConversationKey {
    session_id: String,
    agent_id: Option<String>,
}

impl ConversationKey {
    /// `None` when the request has no session id; such a request cannot be followed up.
    pub fn from_view(view: &RequestView) -> Option<Self> {
        Some(Self {
            session_id: view.session_id.clone()?,
            agent_id: view.agent_id.clone(),
        })
    }
}

/// What to do with one request, given its turn and what is remembered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// Ask the router, then call [`Conversations::remember`] with the answer.
    Decide,
    /// Same turn as before: apply this earlier decision.
    Reuse(RoutingDecision),
    /// A continuation with nothing remembered (proxy restarted, entry expired, no session id).
    /// Forwarding it unchanged is safer than choosing a model halfway through a turn.
    Skip,
}

pub struct Conversations {
    ttl: Duration,
    max_entries: usize,
    entries: Mutex<HashMap<ConversationKey, Entry>>,
}

struct Entry {
    decision: RoutingDecision,
    last_used: Instant,
}

impl Default for Conversations {
    fn default() -> Self {
        Self::new(DEFAULT_TTL, DEFAULT_MAX_ENTRIES)
    }
}

impl Conversations {
    pub fn new(ttl: Duration, max_entries: usize) -> Self {
        Self {
            ttl,
            max_entries: max_entries.max(1),
            entries: Mutex::default(),
        }
    }

    pub fn plan(&self, view: &RequestView) -> Plan {
        self.plan_at(view, Instant::now())
    }

    fn plan_at(&self, view: &RequestView, now: Instant) -> Plan {
        match view.turn {
            Turn::New => Plan::Decide,
            Turn::Continuation => ConversationKey::from_view(view)
                .and_then(|key| self.reuse(&key, now))
                .map_or(Plan::Skip, Plan::Reuse),
        }
    }

    /// Stores `decision` as the one for this conversation. Does nothing without a session id.
    pub fn remember(&self, view: &RequestView, decision: &RoutingDecision) {
        self.remember_at(view, decision, Instant::now());
    }

    fn remember_at(&self, view: &RequestView, decision: &RoutingDecision, now: Instant) {
        let Some(key) = ConversationKey::from_view(view) else {
            return;
        };
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries.retain(|_, entry| now.duration_since(entry.last_used) < self.ttl);
        if entries.len() >= self.max_entries && !entries.contains_key(&key) {
            let oldest = entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                entries.remove(&oldest);
            }
        }
        entries.insert(
            key,
            Entry {
                decision: decision.clone(),
                last_used: now,
            },
        );
    }

    fn reuse(&self, key: &ConversationKey, now: Instant) -> Option<RoutingDecision> {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let entry = entries.get_mut(key)?;
        if now.duration_since(entry.last_used) >= self.ttl {
            entries.remove(key);
            return None;
        }
        entry.last_used = now;
        Some(entry.decision.clone())
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.lock().unwrap().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::router::DecisionSource;

    fn view(session: Option<&str>, agent: Option<&str>, turn: Turn) -> RequestView {
        let mut headers = axum::http::HeaderMap::new();
        if let Some(session) = session {
            headers.insert("x-claude-code-session-id", session.parse().unwrap());
        }
        if let Some(agent) = agent {
            headers.insert("x-claude-code-agent-id", agent.parse().unwrap());
        }
        let mut view = RequestView::parse(
            &headers,
            br#"{"model":"claude-opus-5-5","messages":[{"role":"user","content":"hi"}]}"#,
        )
        .unwrap();
        view.turn = turn;
        view
    }

    fn decision(model: &str) -> RoutingDecision {
        RoutingDecision {
            model: model.into(),
            effort: None,
            reason: "test".into(),
            source: DecisionSource::Rules,
        }
    }

    const HOUR: Duration = Duration::from_secs(3600);

    #[test]
    fn new_turns_always_decide() {
        let store = Conversations::default();
        let new = view(Some("s"), None, Turn::New);
        assert_eq!(store.plan(&new), Plan::Decide);
        store.remember(&new, &decision("haiku"));
        assert_eq!(store.plan(&new), Plan::Decide);
    }

    #[test]
    fn continuations_reuse_the_remembered_decision() {
        let store = Conversations::default();
        store.remember(&view(Some("s"), None, Turn::New), &decision("haiku"));
        let next = view(Some("s"), None, Turn::Continuation);
        assert_eq!(store.plan(&next), Plan::Reuse(decision("haiku")));
        assert_eq!(store.plan(&next), Plan::Reuse(decision("haiku")));
    }

    #[test]
    fn a_new_turn_replaces_the_decision() {
        let store = Conversations::default();
        store.remember(&view(Some("s"), None, Turn::New), &decision("haiku"));
        store.remember(&view(Some("s"), None, Turn::New), &decision("opus"));
        let next = view(Some("s"), None, Turn::Continuation);
        assert_eq!(store.plan(&next), Plan::Reuse(decision("opus")));
    }

    #[test]
    fn subagents_and_sessions_are_separate_conversations() {
        let store = Conversations::default();
        store.remember(&view(Some("s"), None, Turn::New), &decision("main"));
        store.remember(&view(Some("s"), Some("a1"), Turn::New), &decision("sub"));
        let plan = |s, a| store.plan(&view(Some(s), a, Turn::Continuation));
        assert_eq!(plan("s", None), Plan::Reuse(decision("main")));
        assert_eq!(plan("s", Some("a1")), Plan::Reuse(decision("sub")));
        assert_eq!(plan("s", Some("a2")), Plan::Skip);
        assert_eq!(plan("other", None), Plan::Skip);
    }

    #[test]
    fn continuations_without_memory_or_session_are_skipped() {
        let store = Conversations::default();
        assert_eq!(
            store.plan(&view(Some("s"), None, Turn::Continuation)),
            Plan::Skip
        );
        let anonymous = view(None, None, Turn::New);
        store.remember(&anonymous, &decision("haiku"));
        assert_eq!(store.len(), 0);
        assert_eq!(
            store.plan(&view(None, None, Turn::Continuation)),
            Plan::Skip
        );
    }

    #[test]
    fn idle_conversations_expire_and_use_keeps_them_alive() {
        let store = Conversations::new(HOUR, 10);
        let start = Instant::now();
        let new = view(Some("s"), None, Turn::New);
        let next = view(Some("s"), None, Turn::Continuation);
        store.remember_at(&new, &decision("haiku"), start);

        let later = start + HOUR - Duration::from_secs(1);
        assert_eq!(store.plan_at(&next, later), Plan::Reuse(decision("haiku")));
        // The use at `later` restarted the clock.
        let much_later = later + HOUR - Duration::from_secs(1);
        assert_eq!(
            store.plan_at(&next, much_later),
            Plan::Reuse(decision("haiku"))
        );
        assert_eq!(store.plan_at(&next, much_later + HOUR), Plan::Skip);
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn expired_entries_are_dropped_when_remembering() {
        let store = Conversations::new(HOUR, 10);
        let start = Instant::now();
        store.remember_at(&view(Some("a"), None, Turn::New), &decision("x"), start);
        store.remember_at(
            &view(Some("b"), None, Turn::New),
            &decision("x"),
            start + HOUR,
        );
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn the_least_recently_used_entry_goes_when_full() {
        let store = Conversations::new(HOUR, 2);
        let start = Instant::now();
        let second = Duration::from_secs(1);
        store.remember_at(&view(Some("a"), None, Turn::New), &decision("a"), start);
        store.remember_at(
            &view(Some("b"), None, Turn::New),
            &decision("b"),
            start + second,
        );
        // Using "a" makes "b" the oldest.
        let next = view(Some("a"), None, Turn::Continuation);
        assert!(matches!(
            store.plan_at(&next, start + 2 * second),
            Plan::Reuse(_)
        ));
        store.remember_at(
            &view(Some("c"), None, Turn::New),
            &decision("c"),
            start + 3 * second,
        );

        assert_eq!(store.len(), 2);
        let plan =
            |s: &str| store.plan_at(&view(Some(s), None, Turn::Continuation), start + 4 * second);
        assert!(matches!(plan("a"), Plan::Reuse(_)));
        assert_eq!(plan("b"), Plan::Skip);
        assert!(matches!(plan("c"), Plan::Reuse(_)));
    }

    #[test]
    fn updating_an_existing_conversation_does_not_evict_another() {
        let store = Conversations::new(HOUR, 2);
        let start = Instant::now();
        for (i, s) in ["a", "b", "b"].into_iter().enumerate() {
            store.remember_at(
                &view(Some(s), None, Turn::New),
                &decision(s),
                start + Duration::from_secs(i as u64),
            );
        }
        assert_eq!(store.len(), 2);
    }
}
