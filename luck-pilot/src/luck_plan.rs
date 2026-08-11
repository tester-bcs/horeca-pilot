// ---------------------------------------------------------------------------
// luck_plan — формат plan.json (Luck → Rust) и валидатор
//
// Этап 1 интеграции Luck в ai-agent (см. docs/luck-integration.md):
// Luck-компилятор генерирует plan.json (DAG узлов с контрактами), этот модуль
// валидирует его на «компиляции»: типы, рёбра, циклы, покрытие BRANCH-меток,
// контракты VERIFY/POLICY. Контракты — по рекомендациям жюри
// (notes/jury-policy-verify.md): VERIFY = ground-значение + зарегистрированный
// предикат; POLICY = ALLOW+REQUIRE (без deny_effect); REJECT = состояние графа.
// ---------------------------------------------------------------------------

use std::collections::{BTreeMap, HashMap, HashSet};
use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------------------------------------------------------------------------
// Типы плана
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NodeKind {
    Role,
    Classify,
    Filter,
    Step,
    Task,
    Spawn,
    Tool,
    Document,
    Branch,
    Merge,
    Verify,
    Reject,
}

impl NodeKind {
    pub fn is_generative(&self) -> bool {
        matches!(
            self,
            NodeKind::Role
                | NodeKind::Classify
                | NodeKind::Filter
                | NodeKind::Step
                | NodeKind::Task
                | NodeKind::Spawn
        )
    }
    pub fn is_external(&self) -> bool {
        matches!(self, NodeKind::Tool | NodeKind::Document)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum EdgeType {
    #[default]
    Seq,
    Branch,
    Merge,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Policy {
    #[serde(default)]
    pub require: Option<String>,
    #[serde(default)]
    pub allow: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifySpec {
    pub predicate: String,
    #[serde(default)]
    pub subject: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Node {
    pub id: String,
    pub kind: NodeKind,
    /// Куда пишется результат узла (имя значения в потоке данных).
    #[serde(default)]
    pub into: Option<String>,
    /// Откуда берётся вход (имя значения).
    #[serde(default)]
    pub input: Option<String>,
    /// Для CLASSIFY: допустимые метки.
    #[serde(default)]
    pub labels: Vec<String>,
    /// Для BRANCH: метка -> целевые узлы.
    #[serde(default)]
    pub branches: BTreeMap<String, Vec<String>>,
    /// Для TOOL: имя зарегистрированного тула (напр. "mcp:runbook", "shell").
    #[serde(default)]
    pub tool: Option<String>,
    /// Для TOOL: аргументы вызова (свободный JSON — зависит от тула).
    #[serde(default)]
    pub args: Value,
    /// Контракт исполнения (TOOL).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<Policy>,
    /// Контракт проверки (VERIFY-узел или на STEP/TASK).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify: Option<VerifySpec>,
    /// Поведение при отказе узла: "reject" (по умолчанию) и т.п.
    #[serde(default)]
    pub on_fail: Option<String>,
    /// Свободные слоты (ROLE as=..., STEP do=...). "do" — ключевое слово serde.
    #[serde(default, rename = "do", skip_serializing_if = "Option::is_none")]
    pub do_: Option<String>,
    #[serde(default)]
    pub slots: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub from: String,
    pub to: String,
    #[serde(rename = "type", default)]
    pub edge_type: EdgeType,
    /// Для BRANCH-рёбер: метка ветки.
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Limits {
    #[serde(default)]
    pub max_nodes: usize,
    #[serde(default)]
    pub max_depth: usize,
    #[serde(default)]
    pub max_tokens_per_node: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    #[serde(default = "default_plan_version")]
    pub plan_version: u32,
    pub nodes: Vec<Node>,
    #[serde(default)]
    pub edges: Vec<Edge>,
    #[serde(default)]
    pub limits: Limits,
}

fn default_plan_version() -> u32 {
    1
}

// ---------------------------------------------------------------------------
// Ошибки валидации
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanError {
    #[error("unsupported plan_version {0}, expected 1")]
    UnsupportedVersion(u32),
    #[error("duplicate node id: {0}")]
    DuplicateNodeId(String),
    #[error("edge references unknown node: {0}")]
    UnknownNode(String),
    #[error("graph contains a cycle involving node: {0}")]
    Cycle(String),
    #[error("branch labels not covered by BRANCH edges for node {0}: {1:?}")]
    UncoveredBranch(String, Vec<String>),
    #[error("VERIFY node {0} has empty predicate")]
    EmptyPredicate(String),
    #[error("VERIFY node {0} uses unknown predicate: {1}")]
    UnknownPredicate(String, String),
    #[error("VERIFY node {0} has no subject (ground value)")]
    MissingSubject(String),
    #[error("TOOL node {0} has no tool name")]
    MissingTool(String),
    #[error("BRANCH node {0} declares no branches")]
    EmptyBranches(String),
    #[error("node count {0} exceeds max_nodes limit {1}")]
    TooManyNodes(usize, usize),
    #[error("plan has no entry node (all nodes have incoming edges)")]
    NoEntry,
    #[error("edge has unsupported type: {0}")]
    UnsupportedEdgeType(String),
}

// Зарегистрированные детерминированные предикаты VERIFY (v1).
// Расширяется вместе с реестром тулов/предикатов (см. открытые вопросы в плане).
// v1-форма: grep/contains/file_exists/not_empty. Бизнес (HoReCa-пайп):
// stock_level (И1), order_match (И4), cash_ok (И2), shelf_life_ok/temp_log_ok (И3).
const KNOWN_PREDICATES: &[&str] = &[
    "grep",
    "contains",
    "file_exists",
    "not_empty",
    "stock_level",
    "order_match",
    "cash_ok",
    "shelf_life_ok",
    "temp_log_ok",
    "credit_ok",
];

// ---------------------------------------------------------------------------
// Валидатор
// ---------------------------------------------------------------------------

pub fn validate(plan: &Plan) -> Result<(), PlanError> {
    if plan.plan_version != 1 {
        return Err(PlanError::UnsupportedVersion(plan.plan_version));
    }

    // Лимиты.
    if plan.limits.max_nodes > 0 && plan.nodes.len() > plan.limits.max_nodes {
        return Err(PlanError::TooManyNodes(plan.nodes.len(), plan.limits.max_nodes));
    }

    // Уникальность id и индекс.
    let mut ids: HashMap<&str, &Node> = HashMap::new();
    for n in &plan.nodes {
        if ids.insert(n.id.as_str(), n).is_some() {
            return Err(PlanError::DuplicateNodeId(n.id.clone()));
        }
    }

    // Рёбра: известные узлы + индексы исходящих.
    let mut outgoing: BTreeMap<&str, Vec<&Edge>> = BTreeMap::new();
    for e in &plan.edges {
        if !ids.contains_key(e.from.as_str()) {
            return Err(PlanError::UnknownNode(e.from.clone()));
        }
        if !ids.contains_key(e.to.as_str()) {
            return Err(PlanError::UnknownNode(e.to.clone()));
        }
        outgoing.entry(e.from.as_str()).or_default().push(e);
    }

    // Пер-узел контракты.
    for n in &plan.nodes {
        match n.kind {
            NodeKind::Tool => {
                if n.tool.is_none() || n.tool.as_deref().unwrap_or("").is_empty() {
                    return Err(PlanError::MissingTool(n.id.clone()));
                }
            }
            NodeKind::Verify => validate_verify(n)?,
            NodeKind::Branch => {
                if n.branches.is_empty() {
                    return Err(PlanError::EmptyBranches(n.id.clone()));
                }
            }
            _ => {}
        }
        // VERIFY-контракт на STEP/TASK тоже проверяем.
        if let Some(v) = &n.verify {
            validate_verify_spec(&n.id, v)?;
        }
    }

    // BRANCH: каждая объявленная метка должна иметь исходящее BRANCH-ребро с label.
    for n in &plan.nodes {
        if n.kind != NodeKind::Branch {
            continue;
        }
        let covered: HashSet<&str> = outgoing
            .get(n.id.as_str())
            .map(|edges| {
                edges
                    .iter()
                    .filter(|e| e.edge_type == EdgeType::Branch)
                    .filter_map(|e| e.label.as_deref())
                    .collect()
            })
            .unwrap_or_default();
        let missing: Vec<String> = n
            .branches
            .keys()
            .filter(|l| !covered.contains(l.as_str()))
            .cloned()
            .collect();
        if !missing.is_empty() {
            return Err(PlanError::UncoveredBranch(n.id.clone(), missing));
        }
    }

    // Циклы (DFS) и наличие entry-узла.
    // NoEntry проверяем ДО циклов: кольцо (все узлы с входом) — это «нет точки входа»,
    // и оно же содержит цикл; для диагностики важнее NoEntry.
    let indegree: HashMap<&str, usize> = {
        let mut m: HashMap<&str, usize> = HashMap::new();
        for n in &plan.nodes {
            m.insert(n.id.as_str(), 0);
        }
        for e in &plan.edges {
            *m.entry(e.to.as_str()).or_insert(0) += 1;
        }
        m
    };
    if !indegree.values().any(|&d| d == 0) {
        return Err(PlanError::NoEntry);
    }

    let mut state: HashMap<&str, u8> = HashMap::new(); // 0=white 1=gray 2=black
    fn dfs<'a>(
        node: &'a str,
        outgoing: &BTreeMap<&'a str, Vec<&'a Edge>>,
        state: &mut HashMap<&'a str, u8>,
    ) -> Result<(), PlanError> {
        match state.get(node).copied().unwrap_or(0) {
            1 => return Err(PlanError::Cycle(node.to_string())),
            2 => return Ok(()),
            _ => {}
        }
        state.insert(node, 1);
        if let Some(edges) = outgoing.get(node) {
            for e in edges {
                dfs(&e.to, outgoing, state)?;
            }
        }
        state.insert(node, 2);
        Ok(())
    }
    for n in &plan.nodes {
        if state.get(n.id.as_str()).copied().unwrap_or(0) == 0 {
            dfs(&n.id, &outgoing, &mut state)?;
        }
    }

    if !indegree.values().any(|&d| d == 0) {
        return Err(PlanError::NoEntry);
    }

    Ok(())
}

fn validate_verify(n: &Node) -> Result<(), PlanError> {
    let spec = n
        .verify
        .as_ref()
        .ok_or_else(|| PlanError::EmptyPredicate(n.id.clone()))?;
    validate_verify_spec(&n.id, spec)
}

fn validate_verify_spec(node_id: &str, spec: &VerifySpec) -> Result<(), PlanError> {
    if spec.predicate.is_empty() {
        return Err(PlanError::EmptyPredicate(node_id.to_string()));
    }
    if !KNOWN_PREDICATES.contains(&spec.predicate.as_str()) {
        return Err(PlanError::UnknownPredicate(node_id.to_string(), spec.predicate.clone()));
    }
    if spec.subject.is_none() || spec.subject.as_deref().unwrap_or("").is_empty() {
        return Err(PlanError::MissingSubject(node_id.to_string()));
    }
    Ok(())
}

/// Распарсить plan.json в Plan и провалидировать.
pub fn parse_and_validate(json: &str) -> Result<Plan, PlanError> {
    let plan: Plan = serde_json::from_str(json).map_err(|e| PlanError::UnsupportedEdgeType(e.to_string()))?;
    validate(&plan)?;
    Ok(plan)
}

// ---------------------------------------------------------------------------
// Тесты
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn base_plan() -> String {
        r#"{
          "plan_version": 1,
          "nodes": [
            {"id": "role", "kind": "role", "into": "ctx"},
            {"id": "severity", "kind": "classify", "input": "ctx", "labels": ["critical","warning"], "into": "level"},
            {"id": "fork", "kind": "branch", "branches": {"critical": ["probe"], "warning": ["probe"]}},
            {"id": "probe", "kind": "tool", "tool": "shell", "args": {"cmd": "echo hi"}, "into": "out"},
            {"id": "merge", "kind": "merge", "inputs": ["out"], "into": "final"},
            {"id": "report", "kind": "step", "do": "summarize", "verify": {"predicate": "not_empty", "subject": "final"}, "into": "result"}
          ],
          "edges": [
            {"from": "role", "to": "severity", "type": "seq"},
            {"from": "severity", "to": "fork", "type": "seq"},
            {"from": "fork", "to": "probe", "type": "branch", "label": "critical"},
            {"from": "fork", "to": "probe", "type": "branch", "label": "warning"},
            {"from": "probe", "to": "merge", "type": "seq"},
            {"from": "merge", "to": "report", "type": "seq"}
          ],
          "limits": {"max_nodes": 32}
        }"#
        .to_string()
    }

    #[test]
    fn valid_plan_passes() {
        let p = parse_and_validate(&base_plan()).expect("valid plan should pass");
        assert_eq!(p.nodes.len(), 6);
    }

    #[test]
    fn duplicate_id_rejected() {
        let j = base_plan().replace(
            r#"{"id": "report", "kind": "step""#,
            r#"{"id": "role", "kind": "step""#,
        );
        assert!(matches!(
            parse_and_validate(&j),
            Err(PlanError::DuplicateNodeId(_))
        ));
    }

    #[test]
    fn edge_to_unknown_node_rejected() {
        let j = base_plan().replace(
            r#"{"from": "probe", "to": "merge", "type": "seq"}"#,
            r#"{"from": "probe", "to": "ghost", "type": "seq"}"#,
        );
        assert!(matches!(parse_and_validate(&j), Err(PlanError::UnknownNode(_))));
    }

    #[test]
    fn cycle_rejected() {
        let j = base_plan().replace(
            r#"{"from": "merge", "to": "report", "type": "seq"}"#,
            r#"{"from": "merge", "to": "probe", "type": "seq"}"#,
        );
        assert!(matches!(parse_and_validate(&j), Err(PlanError::Cycle(_))));
    }

    #[test]
    fn uncovered_branch_label_rejected() {
        let j = base_plan().replace(
            r#"{"from": "fork", "to": "probe", "type": "branch", "label": "warning"}"#,
            r#"{"from": "fork", "to": "probe", "type": "branch", "label": "info"}"#,
        );
        assert!(matches!(
            parse_and_validate(&j),
            Err(PlanError::UncoveredBranch(_, _))
        ));
    }

    #[test]
    fn verify_unknown_predicate_rejected() {
        let j = base_plan().replace("not_empty", "llm_judge");
        assert!(matches!(
            parse_and_validate(&j),
            Err(PlanError::UnknownPredicate(_, _))
        ));
    }

    #[test]
    fn tool_without_tool_name_rejected() {
        let j = base_plan().replace(
            r#"{"id": "probe", "kind": "tool", "tool": "shell", "args": {"cmd": "echo hi"}, "into": "out"}"#,
            r#"{"id": "probe", "kind": "tool", "args": {"cmd": "echo hi"}, "into": "out"}"#,
        );
        assert!(matches!(parse_and_validate(&j), Err(PlanError::MissingTool(_))));
    }

    #[test]
    fn too_many_nodes_rejected() {
        let j = base_plan().replace("max_nodes\": 32", "max_nodes\": 3");
        assert!(matches!(parse_and_validate(&j), Err(PlanError::TooManyNodes(_, _))));
    }

    #[test]
    fn no_entry_rejected() {
        // Кольцо: у каждого узла есть входящее ребро — нет точки входа.
        let j = r#"{
          "plan_version": 1,
          "nodes": [
            {"id": "a", "kind": "step"},
            {"id": "b", "kind": "step"},
            {"id": "c", "kind": "step"}
          ],
          "edges": [
            {"from": "a", "to": "b", "type": "seq"},
            {"from": "b", "to": "c", "type": "seq"},
            {"from": "c", "to": "a", "type": "seq"}
          ]
        }"#;
        assert!(matches!(parse_and_validate(j), Err(PlanError::NoEntry)));
    }

    #[test]
    fn unsupported_version_rejected() {
        let j = base_plan().replace("plan_version\": 1", "plan_version\": 2");
        assert!(matches!(
            parse_and_validate(&j),
            Err(PlanError::UnsupportedVersion(2))
        ));
    }
}
