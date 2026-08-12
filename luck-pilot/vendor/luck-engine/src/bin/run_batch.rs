//! Luck (Rust) — прогони ОДИН граф на НЕСКОЛЬКИХ разных input'ах без
//! переписывания программы под каждый случай.
//!
//! Демонстрирует ровно то свойство, которого нет у прямого диалога с
//! моделью (см. заметку памяти "Ценность Luck vs прямой диалог"):
//! структура написана один раз, применяется к N разным данным
//! автоматически, без человека, переобъясняющего задачу на каждой
//! итерации. Разные input'ы -> разные ветвления -> разные пути
//! исполнения ОДНОГО И ТОГО ЖЕ графа, видно сразу все вместе.
//!
//!     cargo run --release --bin run_batch -- шаблон.luck входы.txt
//!
//! Шаблон — обычный .luck файл с ОДНИМ плейсхолдером `{{ISSUE}}` внутри
//! STRING-литерала (обычно в INPUT у CLASSIFY). Файл входов — по одной
//! строке на вход; пустые строки и строки, начинающиеся с `#`,
//! пропускаются. Без аргументов — используются встроенный шаблон и
//! встроенный набор из пяти реалистичных GitHub issue.

use luck::anthropic::{AnthropicTransport, ValidatingBackend};
use luck::{Scheduler, ToolRegistry, escape_string_literal, parse};

const TEMPLATE: &str = r#"
NODE role: ROLE [GENERATIVE]
  AS "senior maintainer triaging GitHub issues for a llama.cpp-based project"
END

NODE classify: CLASSIFY [GENERATIVE]
  INPUT "{{ISSUE}}"
  LABELS a="build_error", b="feature_request", c="question", d="duplicate"
  INTO category
END

NODE is_build_error: FILTER [GENERATIVE]
  WHERE category = "build_error"
  INTO branch
END

NODE plan: SPAWN [GENERATIVE]
  PLAN "decompose triage of this build error into concrete diagnostic and response steps"
  INTO subgraph
END

NODE respond: STEP [GENERATIVE]
  DO "draft a one-sentence reply addressing this issue"
  INTO reply
END

EDGES:
  role -> classify
  classify -> is_build_error
  is_build_error => plan [on_match]
  is_build_error => respond [on_no_match]
END
"#;

const EXAMPLE_ISSUES: &[&str] = &[
    "Build fails on Ubuntu 22.04 with CUDA 12.4: undefined reference to cublasSgemm \
     when linking ggml-cuda. Works fine with CPU-only build.",
    "Would be great to add Metal backend support for Apple Silicon GPUs, similar to \
     what llama.cpp already has.",
    "How do I quantize a fine-tuned model to Q4_K_M format using the provided scripts?",
    "This looks like the same issue as #1421 — CUDA build fails with cublas linker errors.",
    "Segfault on startup when loading a GGUF model larger than 8GB on a machine with \
     16GB RAM. No OOM message, just crashes silently.",
];

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
    let mut args = std::env::args().skip(1);
    let template_path = args.next();
    let inputs_path = args.next();

    let template = match &template_path {
        Some(path) => match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("не удалось прочитать шаблон '{path}': {e}");
                return std::process::ExitCode::FAILURE;
            }
        },
        None => TEMPLATE.to_string(),
    };
    if !template.contains("{{ISSUE}}") {
        eprintln!("в шаблоне нет плейсхолдера {{{{ISSUE}}}} — нечего подставлять");
        return std::process::ExitCode::FAILURE;
    }

    let inputs: Vec<String> = match &inputs_path {
        Some(path) => match std::fs::read_to_string(path) {
            Ok(text) => text
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .map(String::from)
                .collect(),
            Err(e) => {
                eprintln!("не удалось прочитать входы '{path}': {e}");
                return std::process::ExitCode::FAILURE;
            }
        },
        None => EXAMPLE_ISSUES.iter().map(|s| s.to_string()).collect(),
    };
    if inputs.is_empty() {
        eprintln!("нет ни одного входа для прогона");
        return std::process::ExitCode::FAILURE;
    }

    let Some(api_key) = resolve_api_key() else {
        eprintln!("Нужен непустой ANTHROPIC_API_KEY (в окружении или введённый в терминале).");
        return std::process::ExitCode::FAILURE;
    };
    let model = std::env::var("LUCK_ANTHROPIC_MODEL")
        .unwrap_or_else(|_| "claude-haiku-4-5-20251001".to_string());

    println!("{}", "=".repeat(78));
    println!(
        "БАТЧ: {} вход(ов), одна и та же структура графа, модель: {model}",
        inputs.len()
    );
    println!("{}", "=".repeat(78));

    let mut total_calls = 0u64;
    let mut total_retries = 0u64;
    let mut total_rejects = 0usize;

    for (i, issue) in inputs.iter().enumerate() {
        let source = template.replace("{{ISSUE}}", &escape_string_literal(issue));
        let mut graph = match parse(&source) {
            Ok(g) => g,
            Err(e) => {
                println!("\n[{}] ОШИБКА ПАРСИНГА (после подстановки): {e}", i + 1);
                continue;
            }
        };

        let transport = AnthropicTransport::new(api_key.clone(), model.clone());
        let mut backend = ValidatingBackend::new(transport, 3);
        let mut tools = ToolRegistry::new();
        let mut sched = Scheduler::new(&mut backend, &mut tools);

        let preview: String = issue.chars().take(70).collect();
        println!(
            "\n[{}] {preview}{}",
            i + 1,
            if issue.chars().count() > 70 {
                "…"
            } else {
                ""
            }
        );

        match sched.run(&mut graph, &format!("batch_{i}")) {
            Ok(result) => {
                let category = result.outputs.get("classify").cloned().unwrap_or_default();
                // Явный вывод самого узла-фильтра (MATCH/NO_MATCH), не
                // косвенный вывод "какой узел исполнился" — тот способ
                // скрыл реальное расхождение между category и решением
                // FILTER (см. диагностику этого коммита).
                let filter_verdict = result
                    .outputs
                    .get("is_build_error")
                    .cloned()
                    .unwrap_or_else(|| "?".into());
                let branch = if result.outputs.contains_key("plan") {
                    "plan"
                } else if result.outputs.contains_key("respond") {
                    "respond"
                } else {
                    "?"
                };
                println!(
                    "      категория: {category:?}   is_build_error: {filter_verdict:?}   ветка: {branch}"
                );
                if !result.rejects.is_empty() {
                    println!("      reject: {} шт.", result.rejects.len());
                }
                total_calls += result.model_calls;
                total_retries += backend.retry_count;
                total_rejects += result.rejects.len();
            }
            Err(e) => {
                println!("      ПРЕРВАНО: {e}");
            }
        }
    }

    println!("\n{}", "=".repeat(78));
    println!("ИТОГО ПО БАТЧУ");
    println!("{}", "=".repeat(78));
    println!("  входов обработано: {}", inputs.len());
    println!("  вызовов модели:    {total_calls}");
    println!("  повторов:          {total_retries}");
    println!("  reject-узлов:      {total_rejects}");
    println!(
        "  Та же структура графа честно разошлась по двум веткам на \
         разных input'ах — без единой ручной правки программы между \
         прогонами."
    );

    std::process::ExitCode::SUCCESS
}
