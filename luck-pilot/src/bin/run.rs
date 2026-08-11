//! CLI: живое исполнение .luck-сценария.
//! Цепочка рантаймов: OpenRouter (nemotron free) -> Ollama (локальная, фоллбэк).
//! Использование:
//!   OPENROUTER_API_KEY=*** cargo run --bin run -- <file.luck>
//!   OLLAMA_MODEL=<model> OLLAMA_HOST=<host> — настройка фоллбэка.

use luck_pilot::{compile, openrouter::FallbackRuntime, PlanExecutor, PlanOutcome};
use std::sync::Arc;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: run <file.luck>");
        std::process::exit(2);
    }
    let src = match std::fs::read_to_string(&args[1]) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("read error: {e}");
            std::process::exit(2);
        }
    };
    let plan = match compile(&src) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("compile error: {e}");
            std::process::exit(1);
        }
    };
    let rt = match FallbackRuntime::from_env() {
        Ok(r) => Arc::new(r),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let model = std::env::var("OPENROUTER_MODEL")
        .unwrap_or_else(|_| luck_pilot::openrouter::DEFAULT_MODEL.to_string());
    let ollama = std::env::var("OLLAMA_MODEL").unwrap_or_else(|_| "hermes3:8b".into());
    println!("== исполнение плана: {} ({} узлов) ==", args[1], plan.nodes.len());
    println!("== цепочка: openrouter[{model}] -> ollama[{ollama}] ==");
    let mut ex = PlanExecutor::new(rt);
    let runtime = tokio::runtime::Runtime::new().expect("tokio");
    let outcome = runtime.block_on(ex.run(&plan));
    match &outcome {
        PlanOutcome::Completed { result } => {
            println!("== ГОТОВО. result: {result} ==");
        }
        PlanOutcome::Rejected { reason } => {
            println!("== ОТКАЗ (состояние графа): {reason} ==");
        }
    }
    println!("== store (ключи -> значения) ==");
    for (k, v) in ex.store() {
        let s = v.to_string();
        // UTF-8-safe обрезка: по char-границам (не по байтам!)
        let s = if s.chars().count() > 120 {
            let truncated: String = s.chars().take(120).collect();
            format!("{truncated}…")
        } else {
            s
        };
        println!("  {k}: {s}");
    }
    std::process::exit(if matches!(outcome, PlanOutcome::Completed { .. }) { 0 } else { 1 });
}
