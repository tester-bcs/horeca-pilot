//! Мета-технология «Агент-BPWin»: IDEF0-модель (функциональные блоки ICOM)
//! → исполнимый Luck-план. Одна декларация блока → все производные.
//!
//! Маппинг ICOM → Luck:
//!   Function        → NODE (kind: step/tool/classify/verify/branch)
//!   Input/Output    → потоки данных (INPUT/INTO), рёбра по соответствию имён
//!   Control         → рёбра-управления + VERIFY-предикаты (v2)
//!   Mechanism       → TOOL-имя (v2)
//!   Декомпозиция    → children (v2: SPAWN-подграфы; v1: документируется)
//!
//! Ограничение v1 (честно): IDEF0 выражает ФУНКЦИИ и ПОТОКИ, но не РЕШЕНИЯ.
//! Ветвление (BRANCH) в IDEF0 не выражается — добавляется на этапе Luck-графа
//! (см. docs/AGENT-BPWIN.md).

use crate::luck_plan::{Edge, EdgeType, Limits, Node, NodeKind, Plan, VerifySpec};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Функциональный блок IDEF0 (ICOM + природа + декомпозиция).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Block {
    pub id: String,
    /// Функция: что делает блок (становится DO узла).
    pub function: String,
    /// Природа: step | tool | classify | verify | branch (default: step).
    #[serde(default)]
    pub kind: Option<String>,
    /// Потоки-входы (имена, ссылающиеся на outputs других блоков).
    #[serde(default)]
    pub inputs: Vec<String>,
    /// Управления: условия/пороги (control-стрелки IDEF0).
    #[serde(default)]
    pub controls: Vec<String>,
    /// Потоки-выходы (имена — становятся INTO узла, первый = результат).
    #[serde(default)]
    pub outputs: Vec<String>,
    /// Механизмы: кто/что выполняет (система, человек, ERP).
    #[serde(default)]
    pub mechanisms: Vec<String>,
    /// Декомпозиция блока (уровни IDEF0; v1 — документируется, v2 — SPAWN).
    #[serde(default)]
    pub children: Vec<Block>,
    /// Для kind=branch: метка ветки → id целевого блока.
    #[serde(default)]
    pub branches: Vec<(String, String)>,
}

/// IDEF0-модель: A-0 контекст (корень) + декомпозиция.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Idef0Model {
    pub context: Block,
}

impl Idef0Model {
    /// Плоский список блоков ДЛЯ ИСПОЛНЕНИЯ: декомпозиция корня (A-0 — рамка,
    /// не исполняется). Если у корня нет детей — исполняется сам корень.
    pub fn flatten(&self) -> Vec<Block> {
        fn walk(b: &Block, out: &mut Vec<Block>) {
            out.push(b.clone());
            for c in &b.children {
                walk(c, out);
            }
        }
        let mut all = Vec::new();
        if self.context.children.is_empty() {
            all.push(self.context.clone());
        } else {
            for c in &self.context.children {
                walk(c, &mut all);
            }
        }
        all
    }
}

/// Маппер: IDEF0-модель → валидный Luck-план.
pub fn map_to_plan(model: &Idef0Model) -> Plan {
    let blocks = model.flatten();

    // Узлы: каждый блок → Node.
    let mut nodes: Vec<Node> = Vec::new();
    for b in &blocks {
        let kind = match b.kind.as_deref().unwrap_or("step") {
            "tool" => NodeKind::Tool,
            "classify" => NodeKind::Classify,
            "verify" => NodeKind::Verify,
            "branch" => NodeKind::Branch,
            _ => NodeKind::Step,
        };
        let into = b.outputs.first().cloned();
        let mut node = Node {
            id: b.id.clone(),
            kind,
            into,
            input: b.inputs.first().cloned(),
            labels: Vec::new(),
            branches: BTreeMap::new(),
            tool: None,
            args: serde_json::Value::Null,
            policy: None,
            verify: None,
            on_fail: None,
            do_: Some(b.function.clone()),
            slots: serde_json::json!({}),
        };
        if kind == NodeKind::Verify {
            node.verify = Some(crate::luck_plan::VerifySpec {
                predicate: "not_empty".into(),
                subject: b.inputs.first().cloned(),
            });
        }
        if kind == NodeKind::Branch {
            for (label, target) in &b.branches {
                node.branches.entry(label.clone()).or_default().push(target.clone());
            }
        }
        nodes.push(node);
    }

    // Рёбра: соответствие output(from) ∈ input/control(to).
    let mut edges: Vec<Edge> = Vec::new();
    let ids: Vec<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
    for from in &ids {
        let from_block = blocks.iter().find(|b| b.id.as_str() == *from).unwrap();
        for to in &ids {
            if from == to {
                continue;
            }
            let to_block = blocks.iter().find(|b| b.id.as_str() == *to).unwrap();
            let linked = from_block
                .outputs
                .iter()
                .any(|o| to_block.inputs.contains(o) || to_block.controls.contains(o));
            if linked {
                edges.push(Edge {
                    from: from.to_string(),
                    to: to.to_string(),
                    edge_type: EdgeType::Seq,
                    label: None,
                });
            }
        }
    }

    Plan {
        plan_version: 1,
        nodes,
        edges,
        limits: Limits {
            max_nodes: 64,
            max_depth: 8,
            max_tokens_per_node: 4096,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::luck_plan::validate;

    /// Демо: HoReCa-дневной цикл, описанный как IDEF0-модель.
    fn horeca_model() -> Idef0Model {
        Idef0Model {
            context: Block {
                id: "A0".into(),
                function: "Обеспечивать производство и торговлю HoReCa".into(),
                kind: None,
                inputs: vec!["заказы".into()],
                controls: vec!["нормы".into()],
                outputs: vec!["отчёт_дня".into()],
                mechanisms: vec![],
                children: vec![
                    Block {
                        id: "A1".into(),
                        function: "Принять заказы клиентов".into(),
                        kind: None,
                        inputs: vec!["заказы".into()],
                        controls: vec![],
                        outputs: vec!["orders".into()],
                        mechanisms: vec![],
                        children: vec![],
                        branches: vec![],
                    },
                    Block {
                        id: "A2".into(),
                        function: "Составить план производства".into(),
                        kind: None,
                        inputs: vec!["orders".into()],
                        controls: vec![],
                        outputs: vec!["production_plan".into()],
                        mechanisms: vec![],
                        children: vec![],
                        branches: vec![],
                    },
                    Block {
                        id: "A3".into(),
                        function: "Произвести продукцию".into(),
                        kind: None,
                        inputs: vec!["production_plan".into()],
                        controls: vec![],
                        outputs: vec!["finished_goods".into()],
                        mechanisms: vec![],
                        children: vec![],
                        branches: vec![],
                    },
                    Block {
                        id: "A4".into(),
                        function: "Скомплектовать заказ".into(),
                        kind: None,
                        inputs: vec!["finished_goods".into()],
                        controls: vec![],
                        outputs: vec!["picked_order".into()],
                        mechanisms: vec![],
                        children: vec![],
                        branches: vec![],
                    },
                    Block {
                        id: "A5".into(),
                        function: "Проверить полноту комплектации".into(),
                        kind: Some("verify".into()),
                        inputs: vec!["picked_order".into()],
                        controls: vec![],
                        outputs: vec!["full_order".into()],
                        mechanisms: vec![],
                        children: vec![],
                        branches: vec![],
                    },
                    Block {
                        id: "A6".into(),
                        function: "Доставить заказ клиенту".into(),
                        kind: None,
                        inputs: vec!["full_order".into()],
                        controls: vec![],
                        outputs: vec!["delivery_note".into()],
                        mechanisms: vec![],
                        children: vec![],
                        branches: vec![],
                    },
                    Block {
                        id: "A7".into(),
                        function: "Выставить счёт".into(),
                        kind: None,
                        inputs: vec!["delivery_note".into()],
                        controls: vec![],
                        outputs: vec!["invoice".into()],
                        mechanisms: vec![],
                        children: vec![],
                        branches: vec![],
                    },
                    Block {
                        id: "A8".into(),
                        function: "Сформировать отчёт дня".into(),
                        kind: None,
                        inputs: vec!["invoice".into()],
                        controls: vec!["нормы".into()],
                        outputs: vec!["отчёт_дня".into()],
                        mechanisms: vec![],
                        children: vec![],
                        branches: vec![],
                    },
                ],
                branches: vec![],
            },
        }
    }

    #[test]
    fn map_horeca_model_to_valid_plan() {
        let model = horeca_model();
        let plan = map_to_plan(&model);
        // A0 не должен стать узлом исполнения (контекст), только дети.
        assert!(plan.nodes.iter().all(|n| n.id != "A0"));
        assert_eq!(plan.nodes.len(), 8);
        // Валидность: версия, дубли, ghost-узлы, циклы, вход.
        validate(&plan).expect("план из IDEF0-модели должен быть валиден");
        // Рёбра: порядок A1→A2→…→A8.
        let e12 = plan.edges.iter().any(|e| e.from == "A1" && e.to == "A2");
        let e78 = plan.edges.iter().any(|e| e.from == "A7" && e.to == "A8");
        assert!(e12, "A1→A2 по orders");
        assert!(e78, "A7→A8 по invoice");
        // INTO = первый output.
        let a1 = plan.nodes.iter().find(|n| n.id == "A1").unwrap();
        assert_eq!(a1.into.as_deref(), Some("orders"));
        // Verify-узел: предикат not_empty, subject = первый input.
        let a5 = plan.nodes.iter().find(|n| n.id == "A5").unwrap();
        let v = a5.verify.as_ref().expect("A5 должен иметь verify-спека");
        assert_eq!(v.predicate, "not_empty");
        assert_eq!(v.subject.as_deref(), Some("picked_order"));
    }
}
