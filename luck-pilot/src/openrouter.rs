//! OpenRouterRuntime — PlanRuntime через OpenRouter API (реальная модель).
//! Модель по умолчанию: nvidia/nemotron-3-super-120b-a12b:free (free tier).
//! Ключ: OPENROUTER_API_KEY из окружения.
//! call_tool: v1 — детерминированный мок с демо-данными (внешние ERP/API
//! ещё не подключены); помечается в выводе как [mock].

use crate::luck_scheduler::PlanRuntime;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::time::Duration;

pub const DEFAULT_MODEL: &str = "nvidia/nemotron-3-super-120b-a12b:free";
const API_URL: &str = "https://openrouter.ai/api/v1/chat/completions";

pub struct OpenRouterRuntime {
    api_key: String,
    model: String,
}

impl OpenRouterRuntime {
    pub fn new(api_key: String, model: String) -> Self {
        Self { api_key, model }
    }

    /// Из окружения; дефолтная модель nemotron free.
    pub fn from_env() -> Result<Self, String> {
        let key = std::env::var("OPENROUTER_API_KEY")
            .map_err(|_| "OPENROUTER_API_KEY не задан".to_string())?;
        let model = std::env::var("OPENROUTER_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string());
        Ok(Self::new(key, model))
    }

    fn chat(&self, system: Option<&str>, user: &str) -> Result<String, String> {
        let mut messages = Vec::new();
        if let Some(sys) = system {
            messages.push(json!({"role": "system", "content": sys}));
        }
        messages.push(json!({"role": "user", "content": user}));
        let body = json!({
            "model": self.model,
            "messages": messages,
            "max_tokens": 256,
            "temperature": 0.2,
        });
        let resp = ureq::post(API_URL)
            .timeout(Duration::from_secs(10))
            .set("Authorization", &format!("Bearer {}", self.api_key))
            .set("Content-Type", "application/json")
            .send_json(body)
            .map_err(|e| format!("openrouter http: {e}"))?;
        let v: Value = resp.into_json().map_err(|e| format!("openrouter json: {e}"))?;
        if let Some(err) = v.get("error") {
            return Err(format!("openrouter error: {err}"));
        }
        let content = v["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("")
            .trim()
            .to_string();
        if content.is_empty() {
            return Err("openrouter: пустой ответ модели".to_string());
        }
        Ok(content)
    }
}

#[async_trait]
impl PlanRuntime for OpenRouterRuntime {
    async fn generate(&self, system: Option<&str>, user: &str) -> Result<String, String> {
        let system = system.map(str::to_string);
        let user = user.to_string();
        let key = self.api_key.clone();
        let model = self.model.clone();
        tokio::task::spawn_blocking(move || {
            let rt = OpenRouterRuntime::new(key, model);
            rt.chat(system.as_deref(), &user)
        })
        .await
        .map_err(|e| format!("task: {e}"))?
    }

    async fn call_tool(&self, name: &str, _args: &Value) -> Result<String, String> {
        Ok(format!("[mock:{name}] {{}}"))
    }
}

/// OllamaRuntime — фоллбэк: локальная Ollama (без reasoning-моделей!).
/// Модель по умолчанию qwen2.5-coder:3b (быстрая, без thinking-бюджета).
/// Хост/модель: env OLLAMA_HOST (default http://localhost:11434), OLLAMA_MODEL.
pub struct OllamaRuntime {
    host: String,
    model: String,
}

impl OllamaRuntime {
    pub fn new(host: String, model: String) -> Self {
        Self { host, model }
    }

    pub fn from_env() -> Self {
        let host = std::env::var("OLLAMA_HOST").unwrap_or_else(|_| "http://localhost:11434".into());
        let model = std::env::var("OLLAMA_MODEL").unwrap_or_else(|_| "qwen2.5-coder:3b".into());
        Self::new(host, model)
    }

    fn chat(&self, system: Option<&str>, user: &str) -> Result<String, String> {
        let mut messages = Vec::new();
        if let Some(sys) = system {
            messages.push(json!({"role": "system", "content": sys}));
        }
        messages.push(json!({"role": "user", "content": user}));
        let body = json!({
            "model": self.model,
            "messages": messages,
            "stream": false,
            "options": {"num_predict": 256, "temperature": 0.2},
        });
        let resp = ureq::post(&format!("{}/api/chat", self.host))
            .timeout(Duration::from_secs(120))
            .send_json(body)
            .map_err(|e| format!("ollama http: {e}"))?;
        let v: Value = resp.into_json().map_err(|e| format!("ollama json: {e}"))?;
        let content = v["message"]["content"].as_str().unwrap_or("").trim().to_string();
        if content.is_empty() {
            return Err("ollama: пустой ответ модели".to_string());
        }
        Ok(content)
    }
}

#[async_trait]
impl PlanRuntime for OllamaRuntime {
    async fn generate(&self, system: Option<&str>, user: &str) -> Result<String, String> {
        let system = system.map(str::to_string);
        let user = user.to_string();
        let host = self.host.clone();
        let model = self.model.clone();
        tokio::task::spawn_blocking(move || {
            let rt = OllamaRuntime::new(host, model);
            rt.chat(system.as_deref(), &user)
        })
        .await
        .map_err(|e| format!("task: {e}"))?
    }

    async fn call_tool(&self, name: &str, _args: &Value) -> Result<String, String> {
        Ok(format!("[mock:{name}] {{}}"))
    }
}

/// FallbackRuntime — цепочка: OpenRouter (nemotron free) -> Ollama (локальная).
/// Если OpenRouter недоступен/ошибка/таймаут — узел исполняется на Ollama.
pub struct FallbackRuntime {
    openrouter: OpenRouterRuntime,
    ollama: OllamaRuntime,
}

impl FallbackRuntime {
    pub fn from_env() -> Result<Self, String> {
        Ok(Self {
            openrouter: OpenRouterRuntime::from_env()?,
            ollama: OllamaRuntime::from_env(),
        })
    }

    /// Только Ollama (без попыток OpenRouter): env OLLAMA_ONLY=1.
    pub fn ollama_only() -> Self {
        Self {
            openrouter: OpenRouterRuntime::new(String::new(), String::new()),
            ollama: OllamaRuntime::from_env(),
        }
    }
}

#[async_trait]
impl PlanRuntime for FallbackRuntime {
    async fn generate(&self, system: Option<&str>, user: &str) -> Result<String, String> {
        let ollama_only = std::env::var("OLLAMA_ONLY").is_ok();
        if ollama_only || self.openrouter.api_key.is_empty() {
            return self.ollama.generate(system, user).await;
        }
        match self.openrouter.generate(system, user).await {
            Ok(v) => Ok(v),
            Err(e) => {
                eprintln!("[fallback] openrouter: {e} -> ollama");
                self.ollama.generate(system, user).await
            }
        }
    }

    async fn call_tool(&self, name: &str, args: &Value) -> Result<String, String> {
        // v1: демо-данные внешних систем (помечены [mock]).
        let demo = match name {
            "count_api" => r#"{"book": 100, "actual": 98, "allowed": 5}"#,
            "receivables_api" => r#"{"total": 250000, "due_30": 80000}"#,
            "credit_api" => r#"{"limit": 100000, "outstanding": 80000}"#,
            _ => "{}",
        };
        Ok(format!("[mock:{name}] {demo}"))
    }
}
