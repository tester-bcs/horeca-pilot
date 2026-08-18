//! Валидатор (CLI) — инструмент пайпа инкубатора.
//!
//! Два режима по расширению файла:
//!   validate <file.luck>  — разбор графа Luck (парсер vendor/luck-engine)
//!   validate <file.json>  — проверка ICOM-баланса IDEF0-модели (icom.rs)
use luck_engine::parser::parse;
use luck_pilot::icom::{has_errors, validate_model, Severity};
use luck_pilot::idef0::Idef0Model;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: validate <file.luck | file.json>");
        std::process::exit(2);
    }
    let src = match std::fs::read_to_string(&args[1]) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("read error: {e}");
            std::process::exit(2);
        }
    };
    if args[1].ends_with(".json") {
        validate_idef0(&src);
        return;
    }
    match parse(&src) {
        Ok(graph) => {
            println!("OK: {} nodes, {} edges", graph.nodes.len(), graph.edges.len());
            for (id, n) in &graph.nodes {
                println!("  node {id} kind={:?} subtype={:?}", n.kind, n.subtype);
            }
            for e in &graph.edges {
                println!(
                    "  edge {} -> {} type={:?} label={:?}",
                    e.source, e.target, e.edge_type, e.label
                );
            }
        }
        Err(e) => {
            println!("FAIL: {e}");
            std::process::exit(1);
        }
    }
}

/// Проверка полноты IDEF0-модели. Ошибки дают код возврата 1,
/// предупреждения печатаются, но не проваливают проверку.
fn validate_idef0(src: &str) {
    let model: Idef0Model = match serde_json::from_str(src) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("не разобрать IDEF0-модель: {e}");
            std::process::exit(2);
        }
    };
    let violations = validate_model(&model);
    if violations.is_empty() {
        println!("OK: модель сбалансирована, нарушений нет");
        return;
    }
    for v in &violations {
        let tag = match v.severity {
            Severity::Error => "ОШИБКА",
            Severity::Warning => "ПРЕДУПРЕЖДЕНИЕ",
        };
        let arrow = v.arrow.as_deref().map(|a| format!(" [{a}]")).unwrap_or_default();
        println!("{tag} {:?} {}{}: {}", v.rule, v.block, arrow, v.message);
    }
    if has_errors(&violations) {
        std::process::exit(1);
    }
    println!("OK (с предупреждениями): блокирующих ошибок нет");
}
