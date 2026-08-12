//! CLI: живое исполнение .luck-сценария на каноническом luck-engine.
//! Цепочка транспортов: OpenRouter (nemotron free) -> Ollama (локальная, фоллбэк),
//! обёрнутая vendor-овским `anthropic::ValidatingBackend` (грамматика/ретраи).
//! Использование:
//!   OPENROUTER_API_KEY=*** cargo run --bin run -- <file.luck>
//!   OLLAMA_MODEL=<model> OLLAMA_HOST=<host> — настройка фоллбэка.
//!   OLLAMA_ONLY=1 — только локальная Ollama.

use luck_engine::anthropic::ValidatingBackend;
use luck_engine::parser::parse;
use luck_engine::scheduler::{Scheduler, ToolRegistry};
use luck_pilot::openrouter::{register_demo_tools, FallbackTransport};
use luck_pilot::verify::{run_verified, VerifiedOutcome};

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
    let mut graph = match parse(&src) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("compile error: {e}");
            std::process::exit(1);
        }
    };

    let transport = if std::env::var("OLLAMA_ONLY").is_ok() {
        println!("== режим: только Ollama ==");
        FallbackTransport::ollama_only()
    } else {
        match FallbackTransport::from_env() {
            Ok(t) => t,
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(2);
            }
        }
    };
    let mut backend = ValidatingBackend::new(transport, 3);
    let mut tools = ToolRegistry::new();
    register_demo_tools(&mut tools);

    println!("== исполнение плана: {} ({} узлов) ==", args[1], graph.nodes.len());
    let mut sched = Scheduler::new(&mut backend, &mut tools);
    let outcome = match run_verified(&mut sched, &mut graph, "root") {
        Ok(o) => o,
        Err(fatal) => {
            eprintln!("== ФАТАЛЬНЫЙ СБОЙ: {fatal} ==");
            std::process::exit(2);
        }
    };

    let exit_code = match &outcome {
        VerifiedOutcome::Completed(result) => {
            println!("== ГОТОВО ==");
            print_outputs(&result.outputs);
            0
        }
        VerifiedOutcome::Rejected { reason, partial } => {
            println!("== ОТКАЗ: {reason} ==");
            print_outputs(&partial.outputs);
            1
        }
    };
    std::process::exit(exit_code);
}

fn print_outputs(outputs: &std::collections::BTreeMap<String, String>) {
    println!("== store (node_id -> вывод) ==");
    for (k, v) in outputs {
        let s: String = if v.chars().count() > 120 {
            format!("{}…", v.chars().take(120).collect::<String>())
        } else {
            v.clone()
        };
        println!("  {k}: {s}");
    }
}
