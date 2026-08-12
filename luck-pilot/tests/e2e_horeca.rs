//! E2E: исполнение сценариев HoReCa через luck-engine Scheduler +
//! luck_pilot::verify::run_verified (мок-бэкенд, без сети).
//! Сценарии подключаются include_str! из ../examples_luck/ — любые правки
//! сценариев автоматически проверяются этим харнессом.

use luck_engine::decoding::Constraint;
use luck_engine::parser::{parse, SlotData};
use luck_engine::registry::Kind;
use luck_engine::scheduler::{InvalidModelOutput, ModelBackend, Scheduler, ToolRegistry};
use luck_pilot::openrouter::register_demo_tools;
use luck_pilot::verify::{run_verified, VerifiedOutcome};
use std::collections::BTreeMap;

/// Бэкенд с реалистичными ответами узлов HoReCa (по содержимому DO/INPUT-
/// слота узла — прямой аналог старого `user.contains(...)` на PlanRuntime).
struct HorecaBackend {
    /// Переопределение: какой JSON вернуть для forecast-узла (негативные кейсы).
    forecast_response: &'static str,
}

impl HorecaBackend {
    fn new() -> Self {
        Self {
            forecast_response: r#"{"cash": 500000, "obligations": 400000}"#,
        }
    }
}

impl ModelBackend for HorecaBackend {
    fn generate(
        &mut self,
        _prompt: &str,
        _constraint: &Constraint,
        kind: Kind,
        slots: &BTreeMap<&'static str, SlotData>,
    ) -> Result<String, InvalidModelOutput> {
        match kind {
            Kind::Classify => {
                let input = match slots.get("input") {
                    Some(SlotData::Str(s)) => s.as_str(),
                    _ => "",
                };
                let label = if input.contains("причина возврата") {
                    "defect" // returns: брак производства
                } else {
                    // stock_check (остатки), classify_diff (расхождение),
                    // classify_risk (риск разрыва) — все "ok" по умолчанию.
                    "ok"
                };
                Ok(label.to_string())
            }
            Kind::Step => {
                let do_ = match slots.get("do") {
                    Some(SlotData::Str(s)) => s.as_str(),
                    _ => "",
                };
                if do_.contains("кэш-прогноз") {
                    Ok(self.forecast_response.to_string())
                } else {
                    Ok("ok".to_string())
                }
            }
            _ => Ok(String::new()),
        }
    }
}

fn run_scenario(src: &str, backend: &mut HorecaBackend) -> VerifiedOutcome {
    let mut graph = parse(src).expect("compile");
    let mut tools = ToolRegistry::new();
    register_demo_tools(&mut tools);
    let mut sched = Scheduler::new(backend, &mut tools);
    run_verified(&mut sched, &mut graph, "root").expect("run() не должен фатально падать")
}

#[test]
fn e2e_daily_cycle_completes() {
    let src = include_str!("../../examples_luck/horeca-daily-cycle.luck");
    let outcome = run_scenario(src, &mut HorecaBackend::new());
    assert!(
        matches!(outcome, VerifiedOutcome::Completed(_)),
        "daily-cycle: expected Completed, got {outcome:?}"
    );
}

#[test]
fn e2e_returns_defect_branch_only() {
    let src = include_str!("../../examples_luck/horeca-returns.luck");
    let outcome = run_scenario(src, &mut HorecaBackend::new());
    assert!(
        matches!(outcome, VerifiedOutcome::Completed(_)),
        "returns: expected Completed, got {outcome:?}"
    );
}

#[test]
fn e2e_inventory_ok_branch() {
    let src = include_str!("../../examples_luck/horeca-inventory.luck");
    let outcome = run_scenario(src, &mut HorecaBackend::new());
    assert!(
        matches!(outcome, VerifiedOutcome::Completed(_)),
        "inventory: expected Completed, got {outcome:?}"
    );
}

#[test]
fn e2e_cashflow_completes_when_cash_ok() {
    let src = include_str!("../../examples_luck/horeca-cashflow.luck");
    let outcome = run_scenario(src, &mut HorecaBackend::new());
    assert!(
        matches!(outcome, VerifiedOutcome::Completed(_)),
        "cashflow: expected Completed, got {outcome:?}"
    );
}

#[test]
fn e2e_cashflow_rejects_when_cash_short() {
    let src = include_str!("../../examples_luck/horeca-cashflow.luck");
    let mut backend = HorecaBackend {
        forecast_response: r#"{"cash": 300000, "obligations": 400000}"#,
    };
    let outcome = run_scenario(src, &mut backend);
    match outcome {
        VerifiedOutcome::Rejected { reason, .. } => {
            assert!(reason.contains("cash_ok"), "reason: {reason}");
        }
        other => panic!("cashflow short: expected Rejected(cash_ok), got {other:?}"),
    }
}
