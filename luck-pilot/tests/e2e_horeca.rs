//! E2E: исполнение сценариев HoReCa через PlanExecutor (мок-рантайм, без сети).
//! Сценарии подключаются include_str! из ../examples_luck/ — любые правки
//! сценариев автоматически проверяются этим харнессом.

use luck_pilot::{compile, PlanExecutor, PlanOutcome, PlanRuntime};
use serde_json::Value;
use std::sync::Arc;

/// Рантайм с реалистичными ответами узлов HoReCa (по содержимому DO / имени тула).
struct HorecaRuntime {
    /// Переопределение: какой ответ вернуть для forecast-узла (для негативных кейсов).
    forecast_response: &'static str,
}

impl HorecaRuntime {
    fn new() -> Self {
        Self {
            forecast_response: r#"{"cash": 500000, "obligations": 400000}"#,
        }
    }
}

#[async_trait::async_trait]
impl PlanRuntime for HorecaRuntime {
    async fn generate(&self, _system: Option<&str>, user: &str) -> Result<String, String> {
        // Ветки CLASSIFY по содержимому DO узла.
        if user.contains("оценить остатки") {
            return Ok("ok".to_string()); // daily-cycle: остатков хватает
        }
        if user.contains("классифицировать причину возврата") {
            return Ok("defect".to_string()); // returns: брак производства
        }
        if user.contains("сверить учётные") {
            return Ok("ok".to_string()); // inventory: расхождение в норме
        }
        if user.contains("оценить расхождение") {
            return Ok("ok".to_string()); // inventory: классификатор -> метка ok
        }
        if user.contains("рассчитать кэш-прогноз") {
            return Ok(self.forecast_response.to_string()); // cashflow: JSON для cash_ok
        }
        if user.contains("оценить риск кассового разрыва") {
            return Ok("ok".to_string()); // cashflow: классификатор -> метка ok
        }
        Ok("ok".to_string())
    }

    async fn call_tool(&self, name: &str, _args: &Value) -> Result<String, String> {
        match name {
            "count_api" => Ok(r#"{"book": 100, "actual": 98, "allowed": 5}"#.to_string()),
            "receivables_api" => Ok(r#"{"total": 250000, "due_30": 80000}"#.to_string()),
            "credit_api" => Ok(r#"{"limit": 100000, "outstanding": 80000}"#.to_string()),
            _ => Ok("{}".to_string()),
        }
    }
}

fn run_plan(src: &str, rt: &Arc<HorecaRuntime>) -> PlanOutcome {
    let plan = compile(src).expect("compile");
    let mut ex = PlanExecutor::new(rt.clone());
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    let outcome = runtime.block_on(ex.run(&plan));
    outcome
}

#[test]
fn e2e_daily_cycle_completes() {
    let src = include_str!("../../examples_luck/horeca-daily-cycle.luck");
    let rt = Arc::new(HorecaRuntime::new());
    let outcome = run_plan(src, &rt);
    assert!(
        matches!(&outcome, PlanOutcome::Completed { .. }),
        "daily-cycle: expected Completed, got {outcome:?}"
    );
}

#[test]
fn e2e_returns_defect_branch_only() {
    let src = include_str!("../../examples_luck/horeca-returns.luck");
    let rt = Arc::new(HorecaRuntime::new());
    let outcome = run_plan(src, &rt);
    assert!(
        matches!(&outcome, PlanOutcome::Completed { .. }),
        "returns: expected Completed, got {outcome:?}"
    );
}

#[test]
fn e2e_inventory_ok_branch() {
    let src = include_str!("../../examples_luck/horeca-inventory.luck");
    let rt = Arc::new(HorecaRuntime::new());
    let outcome = run_plan(src, &rt);
    assert!(
        matches!(&outcome, PlanOutcome::Completed { .. }),
        "inventory: expected Completed, got {outcome:?}"
    );
}

#[test]
fn e2e_cashflow_completes_when_cash_ok() {
    let src = include_str!("../../examples_luck/horeca-cashflow.luck");
    let rt = Arc::new(HorecaRuntime::new());
    let outcome = run_plan(src, &rt);
    assert!(
        matches!(&outcome, PlanOutcome::Completed { .. }),
        "cashflow: expected Completed, got {outcome:?}"
    );
}

#[test]
fn e2e_cashflow_rejects_when_cash_short() {
    let src = include_str!("../../examples_luck/horeca-cashflow.luck");
    let rt = Arc::new(HorecaRuntime {
        forecast_response: r#"{"cash": 300000, "obligations": 400000}"#,
    });
    let outcome = run_plan(src, &rt);
    assert!(
        matches!(&outcome, PlanOutcome::Rejected { reason } if reason.contains("cash_ok")),
        "cashflow short: expected Rejected(cash_ok), got {outcome:?}"
    );
}
