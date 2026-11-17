//! The model APIs a review can talk to. One for now: any OpenAI-compatible
//! `/chat/completions` (OpenAI, Ollama, vLLM, llama.cpp, LiteLLM). Another
//! (Anthropic's Messages API...) is one more implementation, picked by
//! `AI_REVIEW_PROVIDER`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, bail};
use serde_json::{Value, json};

#[async_trait::async_trait]
pub trait Provider: Send + Sync {
    /// The model's answer to one question.
    async fn complete(&self, system: &str, user: &str) -> anyhow::Result<String>;
}

pub struct OpenAiCompatible {
    client: reqwest::Client,
    base_url: String,
    model: String,
    api_key: Option<String>,
    /// The endpoint refused JSON mode once: not asked for again.
    no_json_mode: AtomicBool,
}

impl OpenAiCompatible {
    /// `ca_pem`: authorities to trust on top of the usual ones.
    pub fn new(base_url: &str, model: &str, api_key: Option<String>, ca_pem: Option<&[u8]>, timeout: Duration) -> anyhow::Result<Self> {
        let mut b = reqwest::Client::builder().timeout(timeout).user_agent(concat!("forgust-ai-review/", env!("CARGO_PKG_VERSION")));
        if let Some(pem) = ca_pem {
            let certs = reqwest::Certificate::from_pem_bundle(pem).context("AI_REVIEW_CA_FILE is not PEM")?;
            b = b.tls_certs_merge(certs);
        }
        Ok(OpenAiCompatible {
            client: b.build()?,
            base_url: base_url.trim_end_matches('/').to_string(),
            model: model.to_string(),
            api_key: api_key.filter(|k| !k.is_empty()),
            no_json_mode: AtomicBool::new(false),
        })
    }

    async fn post(&self, body: &Value) -> anyhow::Result<reqwest::Response> {
        let mut req = self.client.post(format!("{}/chat/completions", self.base_url)).json(body);
        if let Some(k) = &self.api_key {
            req = req.bearer_auth(k);
        }
        req.send().await.with_context(|| format!("reaching {}", self.base_url))
    }
}

#[async_trait::async_trait]
impl Provider for OpenAiCompatible {
    async fn complete(&self, system: &str, user: &str) -> anyhow::Result<String> {
        let mut body = json!({
            "model": self.model,
            "messages": [{ "role": "system", "content": system }, { "role": "user", "content": user }],
        });
        let json_mode = !self.no_json_mode.load(Ordering::Relaxed);
        if json_mode {
            body["response_format"] = json!({ "type": "json_object" });
        }
        let mut res = self.post(&body).await?;
        // not every server knows JSON mode: ask again without it, and from then on
        if json_mode && (res.status() == reqwest::StatusCode::BAD_REQUEST || res.status() == reqwest::StatusCode::UNPROCESSABLE_ENTITY) {
            self.no_json_mode.store(true, Ordering::Relaxed);
            body.as_object_mut().expect("object").remove("response_format");
            res = self.post(&body).await?;
        }
        let status = res.status();
        let text = res.text().await.context("reading the model's answer")?;
        if !status.is_success() {
            let msg: String = text.chars().take(300).collect();
            bail!("the model endpoint answered {status}: {msg}");
        }
        let v: Value = serde_json::from_str(&text).context("the model endpoint's answer is not JSON")?;
        let choice = &v["choices"][0];
        if choice["finish_reason"] == "length" {
            bail!("the model's answer was cut short (its output limit)");
        }
        match choice["message"]["content"].as_str() {
            Some(s) if !s.trim().is_empty() => Ok(s.to_string()),
            _ => bail!("the model gave no answer"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, http::HeaderMap, routing::post};

    async fn serve(app: Router) -> String {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        format!("http://{addr}/v1")
    }

    #[tokio::test]
    async fn asks_with_the_key_and_falls_back_without_json_mode() {
        let app = Router::new().route(
            "/v1/chat/completions",
            post(|h: HeaderMap, Json(b): Json<Value>| async move {
                assert_eq!(h["authorization"], "Bearer k-1");
                assert_eq!(b["model"], "m");
                if b.get("response_format").is_some() {
                    return (axum::http::StatusCode::BAD_REQUEST, Json(json!({ "error": "unknown field" })));
                }
                (
                    axum::http::StatusCode::OK,
                    Json(json!({ "choices": [{ "message": { "content": "{\"summary\":\"hi\"}" }, "finish_reason": "stop" }] })),
                )
            }),
        );
        let base = serve(app).await;
        let p = OpenAiCompatible::new(&base, "m", Some("k-1".into()), None, Duration::from_secs(5)).unwrap();
        assert_eq!(p.complete("s", "u").await.unwrap(), "{\"summary\":\"hi\"}");
        // the next part asks without JSON mode straight away
        assert_eq!(p.complete("s", "u").await.unwrap(), "{\"summary\":\"hi\"}");
        assert!(p.no_json_mode.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn errors_say_what_happened() {
        let app = Router::new().route("/v1/chat/completions", post(|| async { (axum::http::StatusCode::UNAUTHORIZED, "bad key") }));
        let base = serve(app).await;
        let p = OpenAiCompatible::new(&base, "m", None, None, Duration::from_secs(5)).unwrap();
        let e = p.complete("s", "u").await.unwrap_err().to_string();
        assert!(e.contains("401") && e.contains("bad key"), "{e}");
    }
}
