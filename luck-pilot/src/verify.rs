//! VERIFY-предикаты и enforcement поверх vendor/luck-engine::Scheduler.
//!
//! vendor/luck-engine парсит слот VERIFY (checker=..., см. registry.rs
//! Kind::Step/Kind::Task) на узлах, но НЕ проверяет его в scheduler.rs —
//! grep не находит ни одного упоминания "verify" в scheduler.rs исполнения.
//! Раз vendor/luck-engine — read-only (никогда не модифицируется), эта
//! проверка сделана обёрткой здесь: `run_verified` вызывает
//! `Scheduler::run`, затем проходит по узлам графа с непустым слотом
//! `verify` и прогоняет соответствующий предикат над сохранённым выводом.
//!
//! Соглашение `subject` (не часть грамматики vendor, обычная ARGS-пара
//! слота VERIFY): id узла, чей вывод проверяется. По умолчанию — сам узел
//! (для STEP, читающего собственный INTO). Нужно для TOOL-узлов (нет слота
//! VERIFY в реестре) — следующий STEP ссылается на TOOL через
//! `VERIFY checker="...", subject="tool_node_id"`.
//!
//! Бизнес-предикаты (HoReCa) портированы near-verbatim из старого
//! src/luck_scheduler.rs (форк, ныне удалён) — та же логика, тот же набор:
//! stock_level, order_match, cash_ok, shelf_life_ok, temp_log_ok, credit_ok,
//! плюс форменные not_empty/contains.

use luck_engine::parser::{IntentGraph, SlotData};
use luck_engine::scheduler::ExecutionResult;
use serde_json::Value;

/// Итог исполнения с учётом VERIFY-enforcement (в дополнение к тому, что
/// уже проверяет vendor Scheduler — reject-узлы синтаксиса/типа/бюджета).
#[derive(Debug)]
pub enum VerifiedOutcome {
    Completed(ExecutionResult),
    Rejected { reason: String, partial: ExecutionResult },
}

/// Прогнать граф через Scheduler, затем проверить все VERIFY-слоты.
/// Первое несовпавшее условие останавливает интерпретацию результата как
/// Rejected (сам граф уже полностью исполнен — Scheduler не умеет
/// останавливаться посреди обхода по VERIFY, это ограничение enforcement
/// постфактум, задокументировано явно, не молча).
pub fn run_verified(
    sched: &mut luck_engine::scheduler::Scheduler<'_>,
    graph: &mut IntentGraph,
    branch_id: &str,
) -> Result<VerifiedOutcome, String> {
    let result = sched.run(graph, branch_id)?;
    if !result.rejects.is_empty() {
        let reasons: Vec<String> = result
            .rejects
            .iter()
            .map(|n| {
                let reason = match n.slots.get("reason") {
                    Some(SlotData::Ident(r)) => r.clone(),
                    _ => "?".to_string(),
                };
                format!("{}({})", n.slots.get("cause").map(|c| format!("{c:?}")).unwrap_or_default(), reason)
            })
            .collect();
        return Ok(VerifiedOutcome::Rejected {
            reason: format!("узел(ы) графа отклонены: {}", reasons.join(", ")),
            partial: result,
        });
    }

    for (node_id, node) in &graph.nodes {
        let Some(SlotData::Args(pairs)) = node.slots.get("verify") else {
            continue;
        };
        if pairs.is_empty() {
            continue;
        }
        let checker = pairs
            .iter()
            .find(|(k, _)| k == "checker")
            .map(|(_, v)| unquote(v));
        let Some(checker) = checker else { continue };
        let subject_id = pairs
            .iter()
            .find(|(k, _)| k == "subject")
            .map(|(_, v)| unquote(v))
            .unwrap_or_else(|| node_id.clone());
        let Some(output) = result.outputs.get(&subject_id) else {
            // Узел с VERIFY не исполнился (мёртвая ветка) — не отказ.
            continue;
        };
        if let Err(reason) = run_predicate(&checker, output) {
            return Ok(VerifiedOutcome::Rejected {
                reason: format!("verify {node_id} (subject={subject_id}): {reason}"),
                partial: result,
            });
        }
    }

    Ok(VerifiedOutcome::Completed(result))
}

fn unquote(v: &str) -> String {
    v.trim_matches('"').to_string()
}

fn run_predicate(name: &str, output: &str) -> Result<(), String> {
    match name {
        "not_empty" => verify_not_empty(output),
        "contains" => verify_not_empty(output), // v1: contains = непустой (без needle)
        "stock_level" => verify_stock_level(&ground_value(output)),
        "order_match" => verify_order_match(&ground_value(output)),
        "cash_ok" => verify_cash_ok(&ground_value(output)),
        "shelf_life_ok" => verify_shelf_life_ok(&ground_value(output)),
        "temp_log_ok" => verify_temp_log_ok(&ground_value(output)),
        "credit_ok" => verify_credit_ok(&ground_value(output)),
        other => Err(format!("predicate '{other}' not implemented (v1)")),
    }
}

// ---------------------------------------------------------------------------
// VERIFY-предикаты (чистые функции над ground-значением)
// ---------------------------------------------------------------------------

/// Нормализация ground-значения: строка может содержать JSON — пробуем
/// распарсить. Если модель/тул обернули JSON в пояснения — сначала пробуем
/// весь текст, затем вытаскиваем первый {...} объект.
pub fn ground_value(s: &str) -> Value {
    let t = s.trim();
    if let Ok(v) = serde_json::from_str::<Value>(t) {
        return v;
    }
    if let (Some(start), Some(end)) = (t.find('{'), t.rfind('}')) {
        if end > start {
            if let Ok(v) = serde_json::from_str(&t[start..=end]) {
                return v;
            }
        }
    }
    Value::String(t.to_string())
}

fn num_field(obj: &serde_json::Map<String, Value>, key: &str) -> Option<f64> {
    obj.get(key).and_then(|v| {
        v.as_f64()
            .or_else(|| v.as_str().and_then(|s| s.parse::<f64>().ok()))
    })
}

fn str_field(obj: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    obj.get(key).and_then(|v| v.as_str().map(str::to_string))
}

pub fn verify_not_empty(output: &str) -> Result<(), String> {
    if output.trim().is_empty() {
        Err("not_empty failed: empty output".to_string())
    } else {
        Ok(())
    }
}

/// И1: остаток сырья >= потребность плана. Ожидает {stock, need}.
pub fn verify_stock_level(value: &Value) -> Result<(), String> {
    let obj = value
        .as_object()
        .ok_or("stock_level: ожидается JSON-объект {stock, need}")?;
    let stock = num_field(obj, "stock").ok_or("stock_level: нет поля stock")?;
    let need = num_field(obj, "need").ok_or("stock_level: нет поля need")?;
    if stock >= need {
        Ok(())
    } else {
        Err(format!("stock_level: остаток {stock} < потребность {need}"))
    }
}

/// И4: комплектация покрывает заказ. Ожидает {ordered, picked}.
pub fn verify_order_match(value: &Value) -> Result<(), String> {
    let obj = value
        .as_object()
        .ok_or("order_match: ожидается JSON-объект {ordered, picked}")?;
    let ordered = num_field(obj, "ordered").ok_or("order_match: нет поля ordered")?;
    let picked = num_field(obj, "picked").ok_or("order_match: нет поля picked")?;
    if picked >= ordered {
        Ok(())
    } else {
        Err(format!("order_match: скомплектовано {picked} < заказано {ordered}"))
    }
}

/// И2: кэш-прогноз >= обязательства (нет кассового разрыва). Ожидает {cash, obligations}.
pub fn verify_cash_ok(value: &Value) -> Result<(), String> {
    let obj = value
        .as_object()
        .ok_or("cash_ok: ожидается JSON-объект {cash, obligations}")?;
    let cash = num_field(obj, "cash").ok_or("cash_ok: нет поля cash")?;
    let obligations = num_field(obj, "obligations").ok_or("cash_ok: нет поля obligations")?;
    if cash >= obligations {
        Ok(())
    } else {
        Err(format!("cash_ok: кэш {cash} < обязательства {obligations}"))
    }
}

/// И3: срок годности > горизонт доставки. Ожидает {expires, horizon} — ISO-даты (YYYY-MM-DD).
pub fn verify_shelf_life_ok(value: &Value) -> Result<(), String> {
    let obj = value
        .as_object()
        .ok_or("shelf_life_ok: ожидается JSON-объект {expires, horizon}")?;
    let expires = str_field(obj, "expires").ok_or("shelf_life_ok: нет поля expires")?;
    let horizon = str_field(obj, "horizon").ok_or("shelf_life_ok: нет поля horizon")?;
    if expires >= horizon {
        Ok(())
    } else {
        Err(format!("shelf_life_ok: срок {expires} раньше горизонта {horizon}"))
    }
}

/// И3: температура хранения в норме. Ожидает {temp, max}.
pub fn verify_temp_log_ok(value: &Value) -> Result<(), String> {
    let obj = value
        .as_object()
        .ok_or("temp_log_ok: ожидается JSON-объект {temp, max}")?;
    let temp = num_field(obj, "temp").ok_or("temp_log_ok: нет поля temp")?;
    let max = num_field(obj, "max").ok_or("temp_log_ok: нет поля max")?;
    if temp <= max {
        Ok(())
    } else {
        Err(format!("temp_log_ok: температура {temp} > {max}"))
    }
}

/// И2 (кредитный контроль): задолженность клиента в пределах лимита. Ожидает {limit, outstanding}.
pub fn verify_credit_ok(value: &Value) -> Result<(), String> {
    let obj = value
        .as_object()
        .ok_or("credit_ok: ожидается JSON-объект {limit, outstanding}")?;
    let limit = num_field(obj, "limit").ok_or("credit_ok: нет поля limit")?;
    let outstanding = num_field(obj, "outstanding").ok_or("credit_ok: нет поля outstanding")?;
    if outstanding <= limit {
        Ok(())
    } else {
        Err(format!("credit_ok: задолженность {outstanding} > лимит {limit}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn stock_level_passes_when_enough() {
        let v = json!({"stock": 30.0, "need": 25.0});
        assert!(verify_stock_level(&v).is_ok());
    }

    #[test]
    fn stock_level_rejects_when_short() {
        let v = json!({"stock": 20.0, "need": 25.0});
        assert!(verify_stock_level(&v).is_err());
    }

    #[test]
    fn stock_level_parses_json_string() {
        let v = ground_value(r#"{"stock": 40, "need": 40}"#);
        assert!(verify_stock_level(&v).is_ok());
    }

    #[test]
    fn order_match_passes_when_full() {
        assert!(verify_order_match(&json!({"ordered": 10.0, "picked": 10.0})).is_ok());
        assert!(verify_order_match(&json!({"ordered": 10.0, "picked": 12.0})).is_ok());
    }

    #[test]
    fn order_match_rejects_when_short() {
        assert!(verify_order_match(&json!({"ordered": 10.0, "picked": 8.0})).is_err());
    }

    #[test]
    fn cash_ok_passes_and_rejects() {
        assert!(verify_cash_ok(&json!({"cash": 500_000.0, "obligations": 400_000.0})).is_ok());
        assert!(verify_cash_ok(&json!({"cash": 300_000.0, "obligations": 400_000.0})).is_err());
    }

    #[test]
    fn shelf_life_ok_compares_iso_dates() {
        assert!(verify_shelf_life_ok(&json!({"expires": "2026-08-20", "horizon": "2026-08-15"})).is_ok());
        assert!(verify_shelf_life_ok(&json!({"expires": "2026-08-10", "horizon": "2026-08-15"})).is_err());
    }

    #[test]
    fn temp_log_ok_checks_temperature() {
        assert!(verify_temp_log_ok(&json!({"temp": 4.0, "max": 8.0})).is_ok());
        assert!(verify_temp_log_ok(&json!({"temp": 9.5, "max": 8.0})).is_err());
    }

    #[test]
    fn credit_ok_checks_client_limit() {
        assert!(verify_credit_ok(&json!({"limit": 100_000.0, "outstanding": 80_000.0})).is_ok());
        assert!(verify_credit_ok(&json!({"limit": 100_000.0, "outstanding": 120_000.0})).is_err());
    }

    #[test]
    fn business_predicates_reject_malformed_input() {
        assert!(verify_stock_level(&json!({"stock": 10.0})).is_err());
        assert!(verify_cash_ok(&ground_value("not json")).is_err());
        assert!(verify_order_match(&json!({})).is_err());
    }

    #[test]
    fn not_empty_rejects_blank() {
        assert!(verify_not_empty("   ").is_err());
        assert!(verify_not_empty("ok").is_ok());
    }
}
