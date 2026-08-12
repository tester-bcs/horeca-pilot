//! Luck — форматирование отчёта об исполнении в текст
//! Практическая демонстрация, не архитектурный срез
//!
//! Вынесено сюда после того, как run_real.rs и luck_mcp.rs (MCP-обёртка,
//! src/bin/luck_mcp.rs) стали независимо нуждаться в ОДНОМ И ТОМ ЖЕ
//! человекочитаемом отчёте — до этого форматирование жило внутри
//! run_real.rs как println!-цепочка, копировать которую во второй
//! бинарник значило бы завести две копии одного знания (тот же класс
//! дублирования, что реестр типов узлов решает для канонического
//! синтаксиса — здесь то же решение для транспортного слоя).

use crate::parser::{IntentGraph, SlotData};
use crate::scheduler::ExecutionResult;

pub fn format_execution_report(
    graph: &IntentGraph,
    result: &ExecutionResult,
    model: &str,
    retries: u64,
    source_label: &str,
) -> String {
    let mut out = String::new();
    let bar = "=".repeat(66);

    out.push_str(&format!("{bar}\n"));
    out.push_str(&format!(
        "ИСПОЛНЕНИЕ (Rust, модель: {model}, источник: {source_label})\n"
    ));
    out.push_str(&format!("{bar}\n"));
    out.push_str(&format!("  обход:     {}\n", result.order.join(" -> ")));
    if !result.skipped.is_empty() {
        out.push_str(&format!("  пропущено: {:?}\n", result.skipped));
    }
    if !result.spawned.is_empty() {
        out.push_str(&format!("  порождено: {:?}\n", result.spawned));
    }
    out.push('\n');
    for (nid, val) in &result.outputs {
        let kind = graph.nodes.get(nid).map(|n| n.kind.as_str()).unwrap_or("?");
        let preview = if val.chars().count() < 60 {
            val.clone()
        } else {
            format!("{}...", val.chars().take(57).collect::<String>())
        };
        out.push_str(&format!("  {nid:12} [{kind:9}] {preview:?}\n"));
    }

    out.push('\n');
    out.push_str(&format!("{bar}\n"));
    out.push_str("КОНТРАКТ 5 -> 6\n");
    out.push_str(&format!("{bar}\n"));
    let total = result.model_calls;
    let first_ok = total.saturating_sub(retries);
    out.push_str(&format!("  вызовов модели:        {total}\n"));
    out.push_str(&format!("  из них повторов:       {retries}\n"));
    if total > 0 {
        out.push_str(&format!(
            "  доля с первой попытки: {:.0}%\n",
            100.0 * first_ok as f64 / total as f64
        ));
    }
    let syntax_rejects = result
        .rejects
        .iter()
        .filter(|r| matches!(r.slots.get("reason"), Some(SlotData::Ident(s)) if s == "SYNTAX"))
        .count();
    out.push_str(&format!("  reject(SYNTAX):        {syntax_rejects}\n"));
    for r in &result.rejects {
        let cause = match r.slots.get("cause") {
            Some(SlotData::Ident(s)) => s.as_str(),
            _ => "?",
        };
        let reason = match r.slots.get("reason") {
            Some(SlotData::Ident(s)) => s.as_str(),
            _ => "?",
        };
        out.push_str(&format!("    отказ на узле {cause}: {reason}\n"));
    }

    out.push('\n');
    out.push_str(&format!("{bar}\n"));
    out.push_str("ИНТЕРПРЕТАЦИЯ\n");
    out.push_str(&format!("{bar}\n"));
    if retries == 0 && result.rejects.is_empty() {
        out.push_str("  Грамматики из реестра читаемы для модели без единого повтора.\n");
        out.push_str("  Деградация гарантии на chat API практически безболезненна.\n");
    } else if !result.rejects.is_empty() {
        out.push_str("  Модель упёрлась: повторы не спасли. Это довод в пользу того,\n");
        out.push_str("  что Luck обязан требовать движок с нативными грамматиками,\n");
        out.push_str("  а chat API годится лишь как деградированный режим.\n");
    } else {
        out.push_str(&format!(
            "  Повторы понадобились ({retries}), но модель выправилась.\n"
        ));
        out.push_str("  Валидация постфактум работает как замена decoding-ограничения,\n");
        out.push_str("  ценой лишних вызовов — их доля и есть цена деградации.\n");
    }

    out
}
