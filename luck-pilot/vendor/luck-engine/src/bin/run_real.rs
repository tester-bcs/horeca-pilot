//! Luck (Rust) — запусти СВОЮ программу на Luck против реального Claude.
//!
//!     cargo run --release --bin run_real -- мой_пайплайн.luck
//!     (запросит ключ в терминале — вставь из буфера обмена и Enter)
//!
//! Без аргумента — используется встроенный пример-пайплайн (тот же,
//! что examples/run_real.py в Python-ветке, для сравнимости результатов
//! между реализациями). С аргументом — читает файл и разбирает ЕГО как
//! исходник Luck: NODE ... END блоки + EDGES: ... END, тот же синтаксис,
//! что описан в docs/SPEC.md. Пример минимальной программы:
//!
//!     NODE role: ROLE [GENERATIVE]
//!       AS "senior architect"
//!     END
//!     NODE step: STEP [GENERATIVE]
//!       DO "greet the reader"
//!       INTO out
//!     END
//!     EDGES:
//!       role -> step
//!     END
//!
//! Ключ можно и не вводить руками каждый раз: ANTHROPIC_API_KEY в
//! окружении имеет приоритет, интерактивный запрос — только если
//! переменная не задана (или пуста).
//!
//! Необязательные переменные окружения (те же имена, что в Python):
//!     LUCK_ANTHROPIC_MODEL     (по умолчанию claude-haiku-4-5-20251001)
//!     LUCK_ANTHROPIC_BASE_URL  (по умолчанию https://api.anthropic.com)
//!     LUCK_MAX_TOKENS          (по умолчанию 4096)
//!     LUCK_MAX_ATTEMPTS        (по умолчанию 3)

use luck::anthropic::{AnthropicTransport, ValidatingBackend};
use luck::{Scheduler, ToolRegistry, format_execution_report, parse, register_default_tools};

const EXAMPLE_PIPELINE: &str = r#"
NODE role: ROLE [GENERATIVE]
  AS "incident triage engineer"
END

NODE severity: CLASSIFY [GENERATIVE]
  INPUT "database connection pool exhausted in production"
  LABELS a="critical", b="warning", c="info"
  INTO level
END

NODE is_critical: FILTER [GENERATIVE]
  WHERE level = "critical"
  INTO branch
END

NODE plan: SPAWN [GENERATIVE]
  PLAN "decompose the incident response into concrete Luck nodes"
  INTO subgraph
END

NODE routine: STEP [GENERATIVE]
  DO "write a short routine acknowledgement"
  INTO ack
END

EDGES:
  role -> severity
  severity -> is_critical
  is_critical => plan [on_match]
  is_critical => routine [on_no_match]
END
"#;

/// ANTHROPIC_API_KEY из окружения, если непустой; иначе — интерактивный
/// запрос в терминале (ввод скрыт, как пароль — ключ не должен светиться
/// на экране/в истории терминала). `.trim()` обязателен: вставка из
/// буфера обмена нередко тащит завершающий перевод строки, который иначе
/// стал бы частью значения x-api-key и дал бы тот же 401, что и вообще
/// без ключа — источник путаницы, который стоило исключить сразу.
fn resolve_api_key() -> Option<String> {
    if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
        let trimmed = key.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    match rpassword::prompt_password("Вставь ANTHROPIC_API_KEY и нажми Enter: ") {
        Ok(key) if !key.trim().is_empty() => Some(key.trim().to_string()),
        _ => None,
    }
}

fn main() -> std::process::ExitCode {
    let arg = std::env::args().nth(1);
    let (source, source_label): (String, String) = match &arg {
        Some(path) => match std::fs::read_to_string(path) {
            Ok(text) => (text, path.clone()),
            Err(e) => {
                eprintln!("не удалось прочитать файл '{path}': {e}");
                return std::process::ExitCode::FAILURE;
            }
        },
        None => (
            EXAMPLE_PIPELINE.to_string(),
            "встроенный пример".to_string(),
        ),
    };

    let mut graph = match parse(&source) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("ошибка в исходнике ({source_label}): {e}");
            return std::process::ExitCode::FAILURE;
        }
    };

    let Some(api_key) = resolve_api_key() else {
        eprintln!("Нужен непустой ANTHROPIC_API_KEY (в окружении или введённый в терминале).");
        return std::process::ExitCode::FAILURE;
    };

    let model = std::env::var("LUCK_ANTHROPIC_MODEL")
        .unwrap_or_else(|_| "claude-haiku-4-5-20251001".to_string());
    let base_url = std::env::var("LUCK_ANTHROPIC_BASE_URL")
        .unwrap_or_else(|_| "https://api.anthropic.com".to_string());
    let max_tokens: u32 = std::env::var("LUCK_MAX_TOKENS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(4096);
    let max_attempts: u32 = std::env::var("LUCK_MAX_ATTEMPTS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3);

    let transport = AnthropicTransport::new(api_key, model.clone())
        .with_base_url(base_url)
        .with_max_tokens(max_tokens);
    let mut backend = ValidatingBackend::new(transport, max_attempts);
    let mut tools = ToolRegistry::new();
    register_default_tools(&mut tools);

    let mut sched = Scheduler::new(&mut backend, &mut tools);
    // Err(String) здесь — либо цикл в графе, либо fatal-сбой транспорта
    // (см. anthropic.rs: 401/403/400/404 — не исправится повтором ни на
    // одном узле, останавливает весь прогон, а не один узел).
    let result = match sched.run(&mut graph, "real_run") {
        Ok(r) => r,
        Err(e) => {
            eprintln!("прогон прерван: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };

    let report =
        format_execution_report(&graph, &result, &model, backend.retry_count, &source_label);
    println!("{report}");

    std::process::ExitCode::SUCCESS
}
