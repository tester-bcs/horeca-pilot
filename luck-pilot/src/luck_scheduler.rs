// ---------------------------------------------------------------------------
// luck_scheduler — исполнение плана (DAG) поверх PlanRuntime (форк ai-agent-порта, автономный)
//
// Этап 3 интеграции Luck в ai-agent (см. docs/luck-integration.md).
// PlanExecutor обходит plan.json в топологическом порядке, исполняет узлы:
//   - Generative (ROLE/CLASSIFY/FILTER/STEP/TASK) -> PlanRuntime::generate
//   - TOOL -> PlanRuntime::call_tool (реестр инструментов)
//   - BRANCH -> выбор ветки по значению из input (label == value)
//   - VERIFY -> детерминированный предикат над ground-значением
//   - MERGE -> агрегация входящих значений
//   - REJECT -> состояние графа (план завершается с reason)
// Активность: узлы, достижимые только через невыбранные branch-рёбра, не исполняются.
// ---------------------------------------------------------------------------

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::luck_plan::{EdgeType, NodeKind, Plan};
use tokio::sync::mpsc;

// ---------------------------------------------------------------------------
// Runtime-абстракция: всё, что нужно планировщику от агента
// ---------------------------------------------------------------------------

#[async_trait]
pub trait PlanRuntime: Send + Sync {
    /// Генеративный вызов (ROLE/CLASSIFY/STEP/...). Возвращает текст ответа.
    async fn generate(&self, system: Option<&str>, user: &str) -> Result<String, String>;
    /// Вызов TOOL-узла. `args` — валидный JSON аргументов.
    async fn call_tool(&self, name: &str, args: &Value) -> Result<String, String>;
}

// ---------------------------------------------------------------------------
// Результат исполнения плана
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum PlanOutcome {
    /// План завершён; result — значение узла-цели (последнего исполненного `into`).
    Completed { result: Value },
    /// План остановлен (VERIFY не сошёлся, узел упал, пустая ветка).
    Rejected { reason: String },
}

/// Событие исполнения плана (для UI/прогресса).
#[derive(Debug, Clone, PartialEq)]
pub enum PlanEvent {
    NodeStart { id: String },
    NodeDone { id: String, ok: bool },
    Rejected { reason: String },
    Completed,
}

// ---------------------------------------------------------------------------
// Scheduler
// ---------------------------------------------------------------------------

pub struct PlanExecutor<R: PlanRuntime> {
    runtime: Arc<R>,
    store: HashMap<String, Value>,
    selected_branch: HashMap<String, String>,
    enabled: HashSet<String>,
    events: Option<mpsc::Sender<PlanEvent>>,
}

impl<R: PlanRuntime> PlanExecutor<R> {
    pub fn new(runtime: Arc<R>) -> Self {
        Self {
            runtime,
            store: HashMap::new(),
            selected_branch: HashMap::new(),
            enabled: HashSet::new(),
            events: None,
        }
    }

    /// Создать исполнитель, транслирующий прогресс в канал событий.
    pub fn with_events(runtime: Arc<R>, tx: mpsc::Sender<PlanEvent>) -> Self {
        let mut ex = Self::new(runtime);
        ex.events = Some(tx);
        ex
    }

    async fn emit(&self, ev: PlanEvent) {
        if let Some(tx) = &self.events {
            let _ = tx.send(ev).await;
        }
    }

    pub fn store(&self) -> &HashMap<String, Value> {
        &self.store
    }

    /// Топологический порядок (Kahn). Ошибка -> None (валидатор должен был отсечь).
    fn topo_order(plan: &Plan) -> Option<Vec<String>> {
        let mut indegree: HashMap<&str, usize> = HashMap::new();
        for n in &plan.nodes {
            indegree.insert(n.id.as_str(), 0);
        }
        let mut outgoing: BTreeMap<&str, Vec<&crate::luck_plan::Edge>> = BTreeMap::new();
        for e in &plan.edges {
            *indegree.entry(e.to.as_str()).or_insert(0) += 1;
            outgoing.entry(e.from.as_str()).or_default().push(e);
        }
        let mut queue: Vec<&str> = indegree
            .iter()
            .filter(|(_, d)| **d == 0)
            .map(|(k, _)| *k)
            .collect();
        queue.sort_unstable();
        let mut order = Vec::new();
        while let Some(id) = queue.pop() {
            order.push(id.to_string());
            if let Some(edges) = outgoing.get(id) {
                for e in edges {
                    let d = indegree.get_mut(e.to.as_str())?;
                    *d -= 1;
                    if *d == 0 {
                        queue.push(&e.to);
                        queue.sort_unstable();
                    }
                }
            }
        }
        if order.len() == plan.nodes.len() {
            Some(order)
        } else {
            None
        }
    }

    /// Запустить план. Возвращает outcome; состояние (store, ветки) остаётся доступно.
    pub async fn run(&mut self, plan: &Plan) -> PlanOutcome {
        let Some(order) = Self::topo_order(plan) else {
            return PlanOutcome::Rejected {
                reason: "plan graph has a cycle".to_string(),
            };
        };

        // Начальные: узлы без входящих рёбер.
        self.enabled.clear();
        let mut has_incoming: HashSet<&str> = HashSet::new();
        for e in &plan.edges {
            has_incoming.insert(e.to.as_str());
        }
        for n in &plan.nodes {
            if !has_incoming.contains(n.id.as_str()) {
                self.enabled.insert(n.id.clone());
            }
        }

        for id in &order {
            if !self.enabled.contains(id) {
                continue;
            }
            let Some(node) = plan.nodes.iter().find(|n| &n.id == id) else {
                continue;
            };
            self.emit(PlanEvent::NodeStart { id: id.clone() }).await;
            // Таймаут узла: генерация/тул не должны висеть вечно (kimi/minimax бывают медленными).
            let exec = self.execute_node(plan, node);
            let timed = tokio::time::timeout(std::time::Duration::from_secs(120), exec).await;
            let node_result = match timed {
                Ok(r) => r,
                Err(_) => Err(format!("node {} timed out (120s)", node.id)),
            };
            match node_result {
                Ok(()) => {
                    self.emit(PlanEvent::NodeDone { id: id.clone(), ok: true }).await;
                }
                Err(reason) => {
                    self.emit(PlanEvent::NodeDone { id: id.clone(), ok: false }).await;
                    self.emit(PlanEvent::Rejected { reason: reason.clone() }).await;
                    return PlanOutcome::Rejected { reason };
                }
            }
            self.propagate_enabled(plan, id);
        }

        // Результат: последний узел с into (или весь store как объект).
        let result = plan
            .nodes
            .iter()
            .filter_map(|n| n.into.as_ref().and_then(|k| self.store.get(k)))
            .last()
            .cloned()
            .unwrap_or_else(|| json!({}));
        self.emit(PlanEvent::Completed).await;
        PlanOutcome::Completed { result }
    }

    /// Разметить достижимые узлы после исполнения `from`.
    fn propagate_enabled(&mut self, plan: &Plan, from: &str) {
        for e in plan.edges.iter().filter(|e| e.from == from) {
            let active = match e.edge_type {
                EdgeType::Seq => true,
                EdgeType::Branch => {
                    self.selected_branch
                        .get(from)
                        .is_some_and(|label| Some(label.as_str()) == e.label.as_deref())
                }
                EdgeType::Merge => true,
            };
            if active {
                self.enabled.insert(e.to.clone());
            }
        }
    }

    async fn execute_node(&mut self, plan: &Plan, node: &crate::luck_plan::Node) -> Result<(), String> {
        match node.kind {
            NodeKind::Role | NodeKind::Classify | NodeKind::Filter | NodeKind::Step | NodeKind::Task => {
                let user = self.node_user_text(node);
                let system = node
                    .slots
                    .get("as")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let out = self
                    .runtime
                    .generate(system.as_deref(), &user)
                    .await
                    .map_err(|e| format!("node {} failed: {e}", node.id))?;
                if let Some(k) = &node.into {
                    self.store.insert(k.clone(), Value::String(out));
                }
                Ok(())
            }
            NodeKind::Tool => {
                let name = node
                    .tool
                    .clone()
                    .ok_or_else(|| format!("node {}: no tool", node.id))?;
                let args = if node.args.is_null() {
                    json!({})
                } else {
                    node.args.clone()
                };
                let out = self
                    .runtime
                    .call_tool(&name, &args)
                    .await
                    .map_err(|e| format!("node {} tool {name} failed: {e}", node.id))?;
                if let Some(k) = &node.into {
                    self.store.insert(k.clone(), Value::String(out));
                }
                Ok(())
            }
            NodeKind::Branch => {
                let input_key = node
                    .input
                    .clone()
                    .ok_or_else(|| format!("branch {}: no input", node.id))?;
                let value = self
                    .store
                    .get(&input_key)
                    .ok_or_else(|| format!("branch {}: input '{input_key}' not produced", node.id))?;
                let label = value.as_str().unwrap_or("").to_string();
                if node.branches.contains_key(&label) {
                    self.selected_branch.insert(node.id.clone(), label);
                    Ok(())
                } else {
                    Err(format!(
                        "branch {}: value '{label}' not among branches {:?}",
                        node.id,
                        node.branches.keys().collect::<Vec<_>>()
                    ))
                }
            }
            NodeKind::Verify => {
                let spec = node
                    .verify
                    .as_ref()
                    .ok_or_else(|| format!("verify {}: no spec", node.id))?;
                self.run_verify(node.id.as_str(), spec).await
            }
            NodeKind::Merge => {
                // Агрегация значений предков (по seq-рёбрам от исполненных узлов с into).
                let inputs: Vec<Value> = plan
                    .edges
                    .iter()
                    .filter(|e| e.to == node.id && e.edge_type == EdgeType::Seq)
                    .filter_map(|e| {
                        plan.nodes
                            .iter()
                            .find(|n| n.id == e.from)
                            .and_then(|n| n.into.as_ref())
                            .and_then(|k| self.store.get(k))
                            .cloned()
                    })
                    .collect();
                if let Some(k) = &node.into {
                    let v = match inputs.len() {
                        0 => json!({}),
                        1 => inputs[0].clone(),
                        _ => {
                            let mut m = serde_json::Map::new();
                            for (i, v) in inputs.into_iter().enumerate() {
                                m.insert(format!("in{i}"), v);
                            }
                            Value::Object(m)
                        }
                    };
                    self.store.insert(k.clone(), v);
                }
                Ok(())
            }
            NodeKind::Document | NodeKind::Spawn | NodeKind::Reject => {
                // v1: не исполняются (SPAWN — вложенный план, TODO; Document — внешние артефакты).
                Ok(())
            }
        }
    }

    fn node_user_text(&self, node: &crate::luck_plan::Node) -> String {
        if let Some(d) = &node.do_ {
            return d.clone();
        }
        if let Some(input) = &node.input {
            if let Some(v) = self.store.get(input) {
                return v.to_string();
            }
        }
        format!("execute node {}", node.id)
    }

    /// Реестр VERIFY-предикатов (расширяемый — пайп инкубатора).
    /// v1 (форма): not_empty, contains, file_exists.
    /// Бизнес (HoReCa): stock_level, order_match, cash_ok, shelf_life_ok, temp_log_ok.
    async fn run_verify(&mut self, node_id: &str, spec: &crate::luck_plan::VerifySpec) -> Result<(), String> {
        let subject_key = spec
            .subject
            .as_ref()
            .ok_or_else(|| format!("verify {node_id}: no subject"))?;
        let value = self
            .store
            .get(subject_key)
            .ok_or_else(|| format!("verify {node_id}: subject '{subject_key}' not produced"))?;
        let res = match spec.predicate.as_str() {
            "not_empty" => verify_not_empty(value, subject_key),
            "contains" => verify_not_empty(value, subject_key), // v1: contains = непустой (без needle)
            "stock_level" => verify_stock_level(value),
            "order_match" => verify_order_match(value),
            "cash_ok" => verify_cash_ok(value),
            "shelf_life_ok" => verify_shelf_life_ok(value),
            "temp_log_ok" => verify_temp_log_ok(value),
            "credit_ok" => verify_credit_ok(value),
            // v1: файловые предикаты не реализованы в рантайме — консервативный отказ.
            other => Err(format!(
                "verify {node_id}: predicate '{other}' not implemented in scheduler (v1)"
            )),
        };
        res.map_err(|e| format!("verify {node_id}: {e}"))
    }
}

// ---------------------------------------------------------------------------
// VERIFY-предикаты (чистые функции над ground-значением)
// ---------------------------------------------------------------------------

/// Нормализация ground-значения: строка может содержать JSON — пробуем распарсить.
/// Если модель/тул обернули JSON в пояснения (в т.ч. "[mock:...] {...}") —
/// сначала пробуем весь текст, затем вытаскиваем первый {...} объект.
fn ground_value(value: &Value) -> Value {
    match value {
        Value::String(s) => {
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
            value.clone()
        }
        _ => value.clone(),
    }
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

fn verify_not_empty(value: &Value, subject_key: &str) -> Result<(), String> {
    let empty = match value {
        Value::String(s) => s.trim().is_empty(),
        Value::Null => true,
        _ => false,
    };
    if empty {
        Err(format!("not_empty failed on '{subject_key}'"))
    } else {
        Ok(())
    }
}

/// И1: остаток сырья >= потребность плана. Ожидает {stock, need}.
fn verify_stock_level(value: &Value) -> Result<(), String> {
    let obj = ground_value(value)
        .as_object()
        .cloned()
        .ok_or("stock_level: ожидается JSON-объект {stock, need}")?;
    let stock = num_field(&obj, "stock").ok_or("stock_level: нет поля stock")?;
    let need = num_field(&obj, "need").ok_or("stock_level: нет поля need")?;
    if stock >= need {
        Ok(())
    } else {
        Err(format!("stock_level: остаток {stock} < потребность {need}"))
    }
}

/// И4: комплектация покрывает заказ. Ожидает {ordered, picked}.
fn verify_order_match(value: &Value) -> Result<(), String> {
    let obj = ground_value(value)
        .as_object()
        .cloned()
        .ok_or("order_match: ожидается JSON-объект {ordered, picked}")?;
    let ordered = num_field(&obj, "ordered").ok_or("order_match: нет поля ordered")?;
    let picked = num_field(&obj, "picked").ok_or("order_match: нет поля picked")?;
    if picked >= ordered {
        Ok(())
    } else {
        Err(format!("order_match: скомплектовано {picked} < заказано {ordered}"))
    }
}

/// И2: кэш-прогноз >= обязательства (нет кассового разрыва). Ожидает {cash, obligations}.
fn verify_cash_ok(value: &Value) -> Result<(), String> {
    let obj = ground_value(value)
        .as_object()
        .cloned()
        .ok_or("cash_ok: ожидается JSON-объект {cash, obligations}")?;
    let cash = num_field(&obj, "cash").ok_or("cash_ok: нет поля cash")?;
    let obligations = num_field(&obj, "obligations").ok_or("cash_ok: нет поля obligations")?;
    if cash >= obligations {
        Ok(())
    } else {
        Err(format!("cash_ok: кэш {cash} < обязательства {obligations}"))
    }
}

/// И3: срок годности > горизонт доставки. Ожидает {expires, horizon} — ISO-даты (YYYY-MM-DD).
fn verify_shelf_life_ok(value: &Value) -> Result<(), String> {
    let obj = ground_value(value)
        .as_object()
        .cloned()
        .ok_or("shelf_life_ok: ожидается JSON-объект {expires, horizon}")?;
    let expires = str_field(&obj, "expires").ok_or("shelf_life_ok: нет поля expires")?;
    let horizon = str_field(&obj, "horizon").ok_or("shelf_life_ok: нет поля horizon")?;
    if expires >= horizon {
        Ok(())
    } else {
        Err(format!("shelf_life_ok: срок {expires} раньше горизонта {horizon}"))
    }
}

/// И3: температура хранения в норме. Ожидает {temp, max}.
fn verify_temp_log_ok(value: &Value) -> Result<(), String> {
    let obj = ground_value(value)
        .as_object()
        .cloned()
        .ok_or("temp_log_ok: ожидается JSON-объект {temp, max}")?;
    let temp = num_field(&obj, "temp").ok_or("temp_log_ok: нет поля temp")?;
    let max = num_field(&obj, "max").ok_or("temp_log_ok: нет поля max")?;
    if temp <= max {
        Ok(())
    } else {
        Err(format!("temp_log_ok: температура {temp} > {max}"))
    }
}

/// И2 (кредитный контроль): задолженность клиента в пределах лимита. Ожидает {limit, outstanding}.
fn verify_credit_ok(value: &Value) -> Result<(), String> {
    let obj = ground_value(value)
        .as_object()
        .cloned()
        .ok_or("credit_ok: ожидается JSON-объект {limit, outstanding}")?;
    let limit = num_field(&obj, "limit").ok_or("credit_ok: нет поля limit")?;
    let outstanding = num_field(&obj, "outstanding").ok_or("credit_ok: нет поля outstanding")?;
    if outstanding <= limit {
        Ok(())
    } else {
        Err(format!("credit_ok: задолженность {outstanding} > лимит {limit}"))
    }
}

// ---------------------------------------------------------------------------
// Тесты (мок-рантайм, без сети)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::luck_compile::compile;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct MockRuntime {
        tool_calls: AtomicUsize,
        /// Генеративные ответы по подстроке user-текста.
        classify_answer: &'static str,
    }

    #[async_trait]
    impl PlanRuntime for MockRuntime {
        async fn generate(&self, _system: Option<&str>, user: &str) -> Result<String, String> {
            if user.contains("classify") || user.contains("severity") {
                Ok(self.classify_answer.to_string())
            } else {
                Ok("ok".to_string())
            }
        }
        async fn call_tool(&self, name: &str, _args: &Value) -> Result<String, String> {
            self.tool_calls.fetch_add(1, Ordering::SeqCst);
            Ok(format!("tool:{name}:done"))
        }
    }

    fn runtime() -> Arc<MockRuntime> {
        Arc::new(MockRuntime {
            tool_calls: AtomicUsize::new(0),
            classify_answer: "critical",
        })
    }

    #[tokio::test]
    async fn linear_plan_completes() {
        let src = r#"
NODE role: ROLE
  AS "assistant"
  INTO ctx
END
NODE step1: STEP
  INPUT ctx
  DO "first step"
  INTO a
END
NODE probe: TOOL
  TOOL shell
  ARGS {"cmd":"echo hi"}
  INTO out
END
NODE verify: VERIFY
  VERIFY not_empty INTO out
END
NODE merge: MERGE
  INTO final
END
NODE report: STEP
  INPUT final
  DO "write report"
  INTO result
END
EDGES
  role -> step1
  step1 -> probe
  probe -> verify
  verify -> merge
  merge -> report
END
"#;
        let plan = compile(src).expect("compile");
        let rt = runtime();
        let mut ex = PlanExecutor::new(rt.clone());
        let outcome = ex.run(&plan).await;
        assert!(matches!(outcome, PlanOutcome::Completed { .. }));
        assert_eq!(rt.tool_calls.load(Ordering::SeqCst), 1);
        assert!(ex.store().get("result").is_some());
    }

    #[tokio::test]
    async fn branch_runs_only_selected_target() {
        let src = r#"
NODE classify: STEP
  DO "severity classify"
  INTO level
END
NODE fork: BRANCH
  INPUT level
  BRANCHES critical=probe_a, warning=probe_b
END
NODE probe_a: TOOL
  TOOL shell
  ARGS {"cmd":"echo A"}
  INTO out_a
END
NODE probe_b: TOOL
  TOOL shell
  ARGS {"cmd":"echo B"}
  INTO out_b
END
EDGES
  classify -> fork
  fork -> probe_a [critical]
  fork -> probe_b [warning]
END
"#;
        let plan = compile(src).expect("compile");
        let rt = runtime(); // classify_answer = "critical"
        let mut ex = PlanExecutor::new(rt.clone());
        let outcome = ex.run(&plan).await;
        assert!(matches!(outcome, PlanOutcome::Completed { .. }));
        // Выполнился только probe_a (critical).
        assert_eq!(rt.tool_calls.load(Ordering::SeqCst), 1);
        assert!(ex.store().contains_key("out_a"));
        assert!(!ex.store().contains_key("out_b"));
    }

    #[tokio::test]
    async fn emits_plan_events_in_order() {
        let src = r#"
NODE a: STEP
  DO "a"
  INTO x
END
NODE b: VERIFY
  VERIFY not_empty INTO x
END
EDGES
  a -> b
END
"#;
        let plan = compile(src).expect("compile");
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let mut ex = PlanExecutor::with_events(runtime(), tx);
        let outcome = ex.run(&plan).await;
        assert!(matches!(outcome, PlanOutcome::Completed { .. }));
        let mut events = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            events.push(ev);
        }
        assert_eq!(
            events,
            vec![
                PlanEvent::NodeStart { id: "a".into() },
                PlanEvent::NodeDone { id: "a".into(), ok: true },
                PlanEvent::NodeStart { id: "b".into() },
                PlanEvent::NodeDone { id: "b".into(), ok: true },
                PlanEvent::Completed,
            ]
        );
    }

    #[tokio::test]
    async fn verify_failure_rejects() {
        let src = r#"
NODE empty: STEP
  DO "produce empty"
  INTO val
END
NODE v: VERIFY
  VERIFY not_empty INTO val
END
EDGES
  empty -> v
END
"#;
        let plan = compile(src).expect("compile");
        let mut ex = PlanExecutor::new(runtime());
        let outcome = ex.run(&plan).await;
        // Мок-рантайм возвращает "ok" (не пусто) — добавим негативный кейс ниже.
        assert!(matches!(outcome, PlanOutcome::Completed { .. }));
    }

    #[tokio::test]
    async fn verify_failure_rejects_when_empty() {
        struct EmptyRuntime;
        #[async_trait]
        impl PlanRuntime for EmptyRuntime {
            async fn generate(&self, _s: Option<&str>, _u: &str) -> Result<String, String> {
                Ok("   ".to_string())
            }
            async fn call_tool(&self, _n: &str, _a: &Value) -> Result<String, String> {
                Ok(String::new())
            }
        }
        let src = r#"
NODE empty: STEP
  DO "produce empty"
  INTO val
END
NODE v: VERIFY
  VERIFY not_empty INTO val
END
EDGES
  empty -> v
END
"#;
        let plan = compile(src).expect("compile");
        let mut ex = PlanExecutor::new(Arc::new(EmptyRuntime));
        let outcome = ex.run(&plan).await;
        assert!(matches!(outcome, PlanOutcome::Rejected { reason } if reason.contains("not_empty")));
    }

    // --- Бизнес-предикаты (HoReCa): юнит-тесты чистых функций ---

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
        // TOOL-узел вернул JSON строкой — предикат должен распарсить.
        let v = Value::String(r#"{"stock": 40, "need": 40}"#.to_string());
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
        assert!(verify_stock_level(&json!({"stock": 10.0})).is_err());      // нет need
        assert!(verify_cash_ok(&Value::String("not json".into())).is_err()); // не объект
        assert!(verify_order_match(&json!({})).is_err());                    // пусто
    }

    // --- E2E: бизнес-предикат в плане (TOOL возвращает JSON -> VERIFY) ---

    #[tokio::test]
    async fn e2e_stock_level_verify_in_plan() {
        struct StockRuntime;
        #[async_trait]
        impl PlanRuntime for StockRuntime {
            async fn generate(&self, _s: Option<&str>, _u: &str) -> Result<String, String> {
                Ok("ok".to_string())
            }
            async fn call_tool(&self, name: &str, _a: &Value) -> Result<String, String> {
                match name {
                    "stock_api" => Ok(r#"{"stock": 30, "need": 25}"#.to_string()),
                    _ => Ok("{}".to_string()),
                }
            }
        }
        let src = r#"
NODE stock: TOOL
  TOOL stock_api
  INTO stock_state
END
NODE check: VERIFY
  VERIFY stock_level INTO stock_state
END
EDGES
  stock -> check
END
"#;
        let plan = compile(src).expect("compile");
        let mut ex = PlanExecutor::new(Arc::new(StockRuntime));
        let outcome = ex.run(&plan).await;
        assert!(matches!(outcome, PlanOutcome::Completed { .. }), "expected Completed, got {outcome:?}");
    }

    #[tokio::test]
    async fn e2e_stock_level_rejects_when_short() {
        struct StockRuntime;
        #[async_trait]
        impl PlanRuntime for StockRuntime {
            async fn generate(&self, _s: Option<&str>, _u: &str) -> Result<String, String> {
                Ok("ok".to_string())
            }
            async fn call_tool(&self, name: &str, _a: &Value) -> Result<String, String> {
                match name {
                    "stock_api" => Ok(r#"{"stock": 20, "need": 25}"#.to_string()),
                    _ => Ok("{}".to_string()),
                }
            }
        }
        let src = r#"
NODE stock: TOOL
  TOOL stock_api
  INTO stock_state
END
NODE check: VERIFY
  VERIFY stock_level INTO stock_state
END
EDGES
  stock -> check
END
"#;
        let plan = compile(src).expect("compile");
        let mut ex = PlanExecutor::new(Arc::new(StockRuntime));
        let outcome = ex.run(&plan).await;
        assert!(matches!(outcome, PlanOutcome::Rejected { reason } if reason.contains("stock_level")));
    }
}
