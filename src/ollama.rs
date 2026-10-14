//! Client for Ollama's chat API. It sends the classifier prompt and the condensed message to a
//! local model and reads back a [`Classification`].
//!
//! One HTTP client is shared by every call. `keep_alive` asks Ollama to keep the model loaded
//! between calls, and [`OllamaClient::warm_up`] loads it at startup, so a call does not wait for
//! the model to load. Every call has a hard timeout; nothing here retries.
//!
//! Errors carry only a cause, never the message sent or the text received.

use std::{fmt, time::Duration};

use anyhow::Context;
use reqwest::header::CONTENT_TYPE;
use serde::Deserialize;
use serde_json::json;

use crate::{
    classifier::{output_schema, Classification, ClassificationError, Prompt},
    config::Classifier,
};

/// Loading a model from disk can take a while; only [`OllamaClient::warm_up`] waits this long.
const WARM_UP_TIMEOUT: Duration = Duration::from_secs(60);

/// Room for the JSON answer (about 25 tokens) and nothing more.
const MAX_ANSWER_TOKENS: u32 = 64;

pub struct OllamaClient {
    http: reqwest::Client,
    chat_url: String,
    generate_url: String,
    model: String,
    keep_alive: String,
    timeout: Duration,
}

/// Why a classification did not produce an answer.
#[derive(Debug, Clone, PartialEq)]
pub enum ClassifyError {
    /// No answer within `timeout_ms`.
    Timeout,
    /// Could not reach Ollama (not running, wrong URL, connection dropped).
    Unreachable,
    /// Ollama answered with an error status, such as 404 for a model that is not pulled.
    Status(u16),
    /// Ollama's reply was not the chat reply we expect.
    BadResponse,
    /// The model's answer broke the schema.
    Answer(ClassificationError),
}

impl ClassifyError {
    /// Short, fixed description for logs and routing reasons.
    pub fn label(&self) -> String {
        match self {
            Self::Timeout => "timeout".into(),
            Self::Unreachable => "ollama unreachable".into(),
            Self::Status(code) => format!("ollama status {code}"),
            Self::BadResponse => "unreadable ollama reply".into(),
            Self::Answer(_) => "invalid answer".into(),
        }
    }
}

impl fmt::Display for ClassifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Answer(err) => write!(f, "{err}"),
            other => f.write_str(&other.label()),
        }
    }
}

impl std::error::Error for ClassifyError {}

impl OllamaClient {
    pub fn new(config: &Classifier) -> anyhow::Result<Self> {
        let timeout = Duration::from_millis(config.timeout_ms);
        let http = reqwest::Client::builder()
            .connect_timeout(timeout)
            .build()
            .context("cannot build the HTTP client for Ollama")?;
        let base = config.url.trim_end_matches('/');
        Ok(Self {
            http,
            chat_url: format!("{base}/api/chat"),
            generate_url: format!("{base}/api/generate"),
            model: config.model.clone(),
            keep_alive: config.keep_alive.clone(),
            timeout,
        })
    }

    /// Asks the model to grade `input` (see `classifier::condense`) under `prompt`.
    pub async fn classify(
        &self,
        prompt: &Prompt,
        input: &str,
    ) -> Result<Classification, ClassifyError> {
        let body = self.chat_body(prompt, input);
        let reply = self.post(&self.chat_url, body, self.timeout).await?;
        let chat: Chat = serde_json::from_slice(&reply).map_err(|_| ClassifyError::BadResponse)?;
        Classification::parse(&chat.message.content).map_err(ClassifyError::Answer)
    }

    /// Loads the model into memory without generating anything.
    pub async fn warm_up(&self) -> Result<(), ClassifyError> {
        let body = json!({ "model": self.model, "keep_alive": self.keep_alive }).to_string();
        self.post(&self.generate_url, body, WARM_UP_TIMEOUT).await?;
        Ok(())
    }

    fn chat_body(&self, prompt: &Prompt, input: &str) -> String {
        json!({
            "model": self.model,
            "messages": [
                { "role": "system", "content": prompt.text() },
                { "role": "user", "content": input },
            ],
            "stream": false,
            "think": false,
            "format": output_schema(),
            "keep_alive": self.keep_alive,
            "options": { "temperature": 0, "num_predict": MAX_ANSWER_TOKENS },
        })
        .to_string()
    }

    async fn post(
        &self,
        url: &str,
        body: String,
        timeout: Duration,
    ) -> Result<Vec<u8>, ClassifyError> {
        let response = self
            .http
            .post(url)
            .header(CONTENT_TYPE, "application/json")
            .body(body)
            .timeout(timeout)
            .send()
            .await
            .map_err(cause)?;
        let status = response.status();
        if !status.is_success() {
            return Err(ClassifyError::Status(status.as_u16()));
        }
        // The call's timeout also covers reading the body.
        response
            .bytes()
            .await
            .map(|bytes| bytes.to_vec())
            .map_err(cause)
    }
}

fn cause(err: reqwest::Error) -> ClassifyError {
    if err.is_timeout() {
        ClassifyError::Timeout
    } else {
        ClassifyError::Unreachable
    }
}

#[derive(Deserialize)]
struct Chat {
    message: ChatMessage,
}

#[derive(Deserialize)]
struct ChatMessage {
    content: String,
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;

    fn client() -> OllamaClient {
        let config = Classifier {
            url: "http://127.0.0.1:11434/".into(),
            model: "tiny".into(),
            keep_alive: "5m".into(),
            ..Classifier::default()
        };
        OllamaClient::new(&config).unwrap()
    }

    #[test]
    fn urls_do_not_double_the_slash() {
        let client = client();
        assert_eq!(client.chat_url, "http://127.0.0.1:11434/api/chat");
        assert_eq!(client.generate_url, "http://127.0.0.1:11434/api/generate");
    }

    #[test]
    fn chat_request_asks_for_one_constrained_answer() {
        let prompt = Prompt::builtin();
        let body: Value = serde_json::from_str(&client().chat_body(&prompt, "message: \"hi\"\n"))
            .expect("body is JSON");
        assert_eq!(body["model"], "tiny");
        assert_eq!(body["stream"], false);
        assert_eq!(body["think"], false);
        assert_eq!(body["keep_alive"], "5m");
        assert_eq!(body["format"], output_schema());
        assert_eq!(body["options"]["temperature"], 0);
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][0]["content"], prompt.text());
        assert_eq!(body["messages"][1]["role"], "user");
        assert_eq!(body["messages"][1]["content"], "message: \"hi\"\n");
    }

    #[test]
    fn labels_name_the_cause_and_nothing_else() {
        assert_eq!(ClassifyError::Timeout.label(), "timeout");
        assert_eq!(ClassifyError::Status(404).label(), "ollama status 404");
        let answer = ClassifyError::Answer(ClassificationError::Invalid("secret".into()));
        assert_eq!(answer.label(), "invalid answer");
    }
}
