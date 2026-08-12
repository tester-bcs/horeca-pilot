//! Мета-технология «Агент-BPWin»: IDEF0-модель (функциональные блоки ICOM)
//! → исполнимый граф luck-engine (`IntentGraph`). Одна декларация блока →
//! все производные.
//!
//! Маппинг ICOM → Luck (канонический реестр vendor/luck-engine):
//!   Function        → NODE STEP/TOOL/CLASSIFY (DO/CALL/INPUT — по kind)
//!   Input/Output     → потоки данных: рёбра Seq по соответствию имён output/input
//!   Control          → VERIFY-слот (checker="not_empty" по умолчанию) — детерминированная
//!                      проверка, а не решение модели о себе
//!   Mechanism        → TOOL-имя (kind=tool; CALL <механизм>)
//!   Декомпозиция     → children (v2: SPAWN-подграфы; v1: разворачивается плоско)
//!
//! Отличие от старой версии (форк, автономный luck_plan::Node/Plan —
//! удалён): целевой IR теперь `luck_engine::parser::{IntentGraph, Node}`,
//! конструируемый НАПРЯМУЮ (`Node::new` + `IntentGraph::add_node/add_edge`),
//! а не через парсинг сгенерированного текста — идиоматичнее для типов,
//! которые уже публично конструируемы (см. vendor/luck-engine/src/lib.rs
//! тест `concurrent_identical_spawn_yields_single_node_no_corruption`,
//! строящий узлы тем же способом).
//!
//! Ограничение v1 (честно, как и раньше): IDEF0 выражает ФУНКЦИИ и ПОТОКИ,
//! не РЕШЕНИЯ. Ветвление (в терминах реестра — Branch-рёбра `=>` от
//! CLASSIFY/FILTER-узла) в IDEF0-модели не выражается через ICOM
//! напрямую — для этого блок должен объявить `kind: "branch"`, тогда он
//! маппится на CLASSIFY, а `branches` объявляет метка -> id блока
//! (Branch-рёбра, не Seq).

use luck_engine::parser::{Edge, EdgeType, IntentGraph, Node, SlotData};
use luck_engine::registry::{Kind, Subtype};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Функциональный блок IDEF0 (ICOM + природа + декомпозиция).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Block {
    pub id: String,
    /// Функция: что делает блок (становится DO/CALL/INPUT узла).
    pub function: String,
    /// Природа: step | tool | classify | branch (default: step).
    /// "verify" из старой версии больше не отдельный kind — это слот
    /// (см. `verify` ниже), сохранён как алиас, включающий VERIFY-слот на STEP.
    #[serde(default)]
    pub kind: Option<String>,
    /// Потоки-входы (имена, ссылающиеся на outputs других блоков).
    #[serde(default)]
    pub inputs: Vec<String>,
    /// Управления: условия/пороги (control-стрелки IDEF0).
    #[serde(default)]
    pub controls: Vec<String>,
    /// Потоки-выходы (имена — становятся именем узла-источника для рёбер, первый = результат).
    #[serde(default)]
    pub outputs: Vec<String>,
    /// Механизмы: кто/что выполняет (для kind=tool — имя CALL).
    #[serde(default)]
    pub mechanisms: Vec<String>,
    /// Декомпозиция блока (уровни IDEF0; v1 — разворачивается плоско).
    #[serde(default)]
    pub children: Vec<Block>,
    /// Для kind=branch: метка ветки → id целевого блока (Branch-ребро).
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

fn s(v: &str) -> SlotData {
    SlotData::Str(v.to_string())
}
fn ident(v: &str) -> SlotData {
    SlotData::Ident(v.to_string())
}
fn args(pairs: Vec<(&str, String)>) -> SlotData {
    SlotData::Args(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

/// Маппер: IDEF0-модель → валидный граф luck-engine (`IntentGraph`).
pub fn map_to_graph(model: &Idef0Model) -> IntentGraph {
    let blocks = model.flatten();
    let mut graph = IntentGraph::default();

    for b in &blocks {
        let kind_str = b.kind.as_deref().unwrap_or("step");
        let into = b.outputs.first().cloned().unwrap_or_else(|| format!("{}_out", b.id));

        let node = match kind_str {
            "tool" => {
                let tool_name = b
                    .mechanisms
                    .first()
                    .cloned()
                    .unwrap_or_else(|| b.id.clone());
                let mut slots: BTreeMap<&'static str, SlotData> = BTreeMap::new();
                slots.insert("tool", ident(&tool_name));
                slots.insert("args", SlotData::Args(Vec::new()));
                slots.insert("policy", SlotData::Args(Vec::new()));
                Node::new(b.id.clone(), Subtype::External, Kind::Tool, slots)
            }
            "classify" | "branch" => {
                // branch: у CLASSIFY нет отдельного узла BRANCH в каноническом
                // реестре — ветвление выражается Branch-рёбрами (см. `branches`
                // ниже), сам узел — обычный CLASSIFY. LABELS строятся из
                // `branches`, если заданы, иначе из controls (пороговые условия).
                let label_source: Vec<String> = if !b.branches.is_empty() {
                    b.branches.iter().map(|(l, _)| l.clone()).collect()
                } else if !b.controls.is_empty() {
                    b.controls.clone()
                } else {
                    vec!["ok".to_string()]
                };
                let mut slots: BTreeMap<&'static str, SlotData> = BTreeMap::new();
                slots.insert("input", s(&b.function));
                slots.insert(
                    "labels",
                    args(label_source.iter().map(|l| (l.as_str(), format!("\"{l}\""))).collect()),
                );
                slots.insert("into", ident(&into));
                Node::new(b.id.clone(), Subtype::Generative, Kind::Classify, slots)
            }
            _ => {
                // step (и legacy "verify"): VERIFY-слот включается, когда
                // controls непусты (IDEF0-порог) или kind явно "verify" —
                // предикат по умолчанию not_empty (форменная честность,
                // Срез 4), домен-специфичный checker подставляется вызывающим
                // кодом постфактум (у IDEF0 самого по себе нет понятия
                // "какой бизнес-предикат" — это решение уровня Luck-графа).
                let mut slots: BTreeMap<&'static str, SlotData> = BTreeMap::new();
                slots.insert("do", s(&b.function));
                let verify_slot = if kind_str == "verify" || !b.controls.is_empty() {
                    args(vec![("checker", "\"not_empty\"".to_string())])
                } else {
                    SlotData::Args(Vec::new())
                };
                slots.insert("verify", verify_slot);
                slots.insert("into", ident(&into));
                Node::new(b.id.clone(), Subtype::Generative, Kind::Step, slots)
            }
        };
        graph.add_node(node).expect("уникальные id блоков IDEF0");
    }

    // Рёбра: соответствие output(from) ∈ input/control(to) → Seq;
    // для kind=branch — Branch-рёбра по `branches` (метка -> целевой блок).
    let ids: Vec<String> = blocks.iter().map(|b| b.id.clone()).collect();
    for from_id in &ids {
        let from_block = blocks.iter().find(|b| &b.id == from_id).unwrap();
        if from_block.kind.as_deref() == Some("branch") && !from_block.branches.is_empty() {
            for (label, target) in &from_block.branches {
                if ids.contains(target) {
                    graph.add_edge(Edge {
                        source: from_id.clone(),
                        target: target.clone(),
                        edge_type: EdgeType::Branch,
                        label: Some(label.clone()),
                    });
                }
            }
            continue;
        }
        for to_id in &ids {
            if from_id == to_id {
                continue;
            }
            let to_block = blocks.iter().find(|b| &b.id == to_id).unwrap();
            let linked = from_block
                .outputs
                .iter()
                .any(|o| to_block.inputs.contains(o) || to_block.controls.contains(o));
            if linked {
                graph.add_edge(Edge {
                    source: from_id.clone(),
                    target: to_id.clone(),
                    edge_type: EdgeType::Seq,
                    label: None,
                });
            }
        }
    }

    graph
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn map_horeca_model_to_valid_graph() {
        let model = horeca_model();
        let graph = map_to_graph(&model);
        // A0 не должен стать узлом исполнения (контекст), только дети.
        assert!(!graph.nodes.contains_key("A0"));
        assert_eq!(graph.nodes.len(), 8);
        // Валидность: рёбра ссылаются только на объявленные узлы.
        graph
            .validate_edge_endpoints()
            .expect("граф из IDEF0-модели должен быть валиден");
        // Рёбра: порядок A1→A2→…→A8.
        let e12 = graph.edges.iter().any(|e| e.source == "A1" && e.target == "A2");
        let e78 = graph.edges.iter().any(|e| e.source == "A7" && e.target == "A8");
        assert!(e12, "A1→A2 по orders");
        assert!(e78, "A7→A8 по invoice");
        // INTO = первый output.
        let a1 = &graph.nodes["A1"];
        assert_eq!(a1.slots.get("into"), Some(&SlotData::Ident("orders".to_string())));
        // Verify-узел (kind="verify" legacy): checker=not_empty в слоте verify.
        let a5 = &graph.nodes["A5"];
        match a5.slots.get("verify") {
            Some(SlotData::Args(pairs)) => {
                assert!(pairs.iter().any(|(k, v)| k == "checker" && v.contains("not_empty")));
            }
            other => panic!("A5 должен иметь непустой verify-слот, получено {other:?}"),
        }
    }

    #[test]
    fn branch_block_produces_branch_edges() {
        let model = Idef0Model {
            context: Block {
                id: "R".into(),
                function: "root".into(),
                kind: None,
                inputs: vec![],
                controls: vec![],
                outputs: vec![],
                mechanisms: vec![],
                branches: vec![],
                children: vec![
                    Block {
                        id: "gate".into(),
                        function: "оценить состояние".into(),
                        kind: Some("branch".into()),
                        inputs: vec![],
                        controls: vec![],
                        outputs: vec![],
                        mechanisms: vec![],
                        branches: vec![("ok".into(), "go".into()), ("bad".into(), "stop".into())],
                        children: vec![],
                    },
                    Block {
                        id: "go".into(),
                        function: "продолжить".into(),
                        kind: None,
                        inputs: vec![],
                        controls: vec![],
                        outputs: vec![],
                        mechanisms: vec![],
                        branches: vec![],
                        children: vec![],
                    },
                    Block {
                        id: "stop".into(),
                        function: "остановиться".into(),
                        kind: None,
                        inputs: vec![],
                        controls: vec![],
                        outputs: vec![],
                        mechanisms: vec![],
                        branches: vec![],
                        children: vec![],
                    },
                ],
            },
        };
        let graph = map_to_graph(&model);
        assert_eq!(graph.nodes["gate"].kind, Kind::Classify);
        let branch_edges: Vec<_> = graph
            .edges
            .iter()
            .filter(|e| e.edge_type == EdgeType::Branch)
            .collect();
        assert_eq!(branch_edges.len(), 2);
        assert!(branch_edges
            .iter()
            .any(|e| e.source == "gate" && e.target == "go" && e.label.as_deref() == Some("ok")));
    }
}
