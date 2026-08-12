//! Транспорты (`luck_engine::anthropic::ChatTransport`) для OpenRouter и
//! Ollama — заменяет старый `PlanRuntime` (форк удалён вместе с
//! src/luck_scheduler.rs). Канонический luck-engine ожидает `ModelBackend`
//! (scheduler.rs) — но полную грамматическую валидацию/ретраи по
//! содержимому уже реализует `anthropic::ValidatingBackend<T: ChatTransport>`
//! (см. vendor/luck-engine/src/anthropic.rs). Решение миграции: не писать
//! ModelBackend с нуля, а реализовать только тонкий ChatTransport (текст ->
//! текст, свои сетевые ретраи) и обернуть его в vendor-овский
//! ValidatingBackend — переиспользуем всю логику грамматики/few-shot/
//! повторов вместо повторной реализации.
//!
//! Модель по умолчанию: nvidia/nemotron-3-super-120b-a12b:free (free tier).
//! Ключ: OPENROUTER_API_KEY из окружения.

use luck_engine::anthropic::{ChatTransport, TransportError};
use serde_json::{json, Value};
use std::time::Duration;

pub const DEFAULT_MODEL: &str = "nvidia/nemotron-3-super-120b-a12b:free";
const API_URL: &str = "https://openrouter.ai/api/v1/chat/completions";

pub struct OpenRouterTransport {
    api_key: String,
    model: String,
    agent: ureq::Agent,
    pub calls: u64,
}

impl OpenRouterTransport {
    pub fn new(api_key: String, model: String) -> Self {
        Self {
            api_key,
            model,
            agent: ureq::AgentBuilder::new().build(),
            calls: 0,
        }
    }

    /// Из окружения; дефолтная модель nemotron free.
    pub fn from_env() -> Result<Self, String> {
        let key = std::env::var("OPENROUTER_API_KEY")
            .map_err(|_| "OPENROUTER_API_KEY не задан".to_string())?;
        let model = std::env::var("OPENROUTER_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string());
        Ok(Self::new(key, model))
    }

    pub fn is_configured(&self) -> bool {
        !self.api_key.is_empty()
    }
}

impl ChatTransport for OpenRouterTransport {
    fn call_count(&self) -> u64 {
        self.calls
    }

    fn call(&mut self, prompt: &str) -> Result<String, TransportError> {
        self.calls += 1;
        let body = json!({
            "model": self.model,
            "messages": [{"role": "user", "content": prompt}],
            "max_tokens": 2000,
            "temperature": 0.2,
        });
        let resp = self
            .agent
            .post(API_URL)
            .timeout(Duration::from_secs(180))
            .set("Authorization", &format!("Bearer {}", self.api_key))
            .set("Content-Type", "application/json")
            .send_json(body);
        match resp {
            Ok(r) => {
                let v: Value = r
                    .into_json()
                    .map_err(|e| TransportError::Fatal(format!("openrouter json: {e}")))?;
                if let Some(err) = v.get("error") {
                    return Err(TransportError::Transient(format!("openrouter error: {err}")));
                }
                let content = v["choices"][0]["message"]["content"]
                    .as_str()
                    .unwrap_or("")
                    .to_string();
                Ok(content)
            }
            Err(ureq::Error::Status(code, r)) => {
                let body_text = r.into_string().unwrap_or_default();
                if (400..500).contains(&code) && code != 429 {
                    Err(TransportError::Fatal(format!("HTTP {code}: {body_text}")))
                } else {
                    Err(TransportError::Transient(format!("HTTP {code}: {body_text}")))
                }
            }
            Err(e) => Err(TransportError::Transient(e.to_string())),
        }
    }
}

/// OllamaTransport — фоллбэк: локальная Ollama (без reasoning-моделей!).
/// Модель по умолчанию qwen2.5-coder:3b. Хост/модель: env OLLAMA_HOST
/// (default http://localhost:11434), OLLAMA_MODEL.
pub struct OllamaTransport {
    host: String,
    model: String,
    agent: ureq::Agent,
    pub calls: u64,
}

impl OllamaTransport {
    pub fn new(host: String, model: String) -> Self {
        Self {
            host,
            model,
            agent: ureq::AgentBuilder::new().build(),
            calls: 0,
        }
    }

    pub fn from_env() -> Self {
        let host = std::env::var("OLLAMA_HOST").unwrap_or_else(|_| "http://localhost:11434".into());
        let model = std::env::var("OLLAMA_MODEL").unwrap_or_else(|_| "qwen2.5-coder:3b".into());
        Self::new(host, model)
    }
}

impl ChatTransport for OllamaTransport {
    fn call_count(&self) -> u64 {
        self.calls
    }

    fn call(&mut self, prompt: &str) -> Result<String, TransportError> {
        self.calls += 1;
        let body = json!({
            "model": self.model,
            "messages": [{"role": "user", "content": prompt}],
            "stream": false,
            "options": {"num_predict": 512, "temperature": 0.2},
        });
        let resp = self
            .agent
            .post(&format!("{}/api/chat", self.host))
            .timeout(Duration::from_secs(120))
            .send_json(body);
        match resp {
            Ok(r) => {
                let v: Value = r
                    .into_json()
                    .map_err(|e| TransportError::Fatal(format!("ollama json: {e}")))?;
                Ok(v["message"]["content"].as_str().unwrap_or("").to_string())
            }
            Err(e) => Err(TransportError::Transient(format!("ollama http: {e}"))),
        }
    }
}

/// FallbackTransport — цепочка: OpenRouter (nemotron free) -> Ollama
/// (локальная). Если OpenRouter недоступен/ошибка временная — узел
/// исполняется на Ollama. Fatal-ошибки OpenRouter (401/403/...) тоже
/// падают в Ollama (в отличие от одиночного транспорта, где Fatal
/// останавливает весь обход) — это осознанное расширение поведения
/// старого FallbackRuntime, не регресс: единственный транспорт не может
/// решить "остановиться" за пользователя, если есть второй под рукой.
pub struct FallbackTransport {
    openrouter: OpenRouterTransport,
    ollama: OllamaTransport,
    ollama_only: bool,
}

impl FallbackTransport {
    pub fn from_env() -> Result<Self, String> {
        Ok(Self {
            openrouter: OpenRouterTransport::from_env()?,
            ollama: OllamaTransport::from_env(),
            ollama_only: std::env::var("OLLAMA_ONLY").is_ok(),
        })
    }

    /// Только Ollama (без попыток OpenRouter).
    pub fn ollama_only() -> Self {
        Self {
            openrouter: OpenRouterTransport::new(String::new(), String::new()),
            ollama: OllamaTransport::from_env(),
            ollama_only: true,
        }
    }
}

impl ChatTransport for FallbackTransport {
    fn call_count(&self) -> u64 {
        self.openrouter.calls + self.ollama.calls
    }

    fn call(&mut self, prompt: &str) -> Result<String, TransportError> {
        if self.ollama_only || !self.openrouter.is_configured() {
            return self.ollama.call(prompt);
        }
        match self.openrouter.call(prompt) {
            Ok(v) => Ok(v),
            Err(e) => {
                eprintln!("[fallback] openrouter: {e} -> ollama");
                self.ollama.call(prompt)
            }
        }
    }
}

/// Реестр демо-инструментов (внешние ERP/API ещё не подключены, v1) —
/// регистрируется в `luck_engine::scheduler::ToolRegistry`, используется
/// всеми 4 сценариями HoReCa. Помечает вывод как детерминированный мок
/// (не [mock]-префикс, как в старом PlanRuntime::call_tool — здесь тул
/// сам решает, что вернуть, планировщик не отличает мок от реального
/// вызова, поэтому обманывать формат незачем).
pub fn register_demo_tools(tools: &mut luck_engine::scheduler::ToolRegistry) {
    tools.register("count_api", |_args| {
        r#"{"book": 100, "actual": 98, "allowed": 5}"#.to_string()
    });
    tools.register("receivables_api", |_args| {
        r#"{"total": 250000, "due_30": 80000}"#.to_string()
    });
    tools.register("credit_api", |_args| {
        r#"{"limit": 100000, "outstanding": 80000}"#.to_string()
    });
}
