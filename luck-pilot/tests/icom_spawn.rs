//! Тесты `icom::validate_spawned` — проверки подграфа, порождённого SPAWN.
//!
//! Долг: функция была написана без единого теста. Здесь она проверяется
//! и сквозным прогоном (реальный Scheduler порождает узлы), и точечно
//! (граф и ExecutionResult собраны вручную — так воспроизводятся случаи,
//! которых MockBackend не выдаёт).
use luck_pilot::icom::{has_errors, validate_spawned, Rule};
use luck_pilot::{
    ExecutionResult, IntentGraph, Kind, MockBackend, Node, Scheduler, SlotData, Subtype,
    ToolRegistry,
};
use std::collections::BTreeMap;

const SPAWN_SRC: &str = r#"
NODE plan: SPAWN [GENERATIVE]
  PLAN "разложить задачу на шаги"
  INTO sub
END

EDGES:
END
"#;

fn run_spawn() -> (IntentGraph, ExecutionResult) {
    let mut graph = luck_pilot::parse(SPAWN_SRC).expect("исходник SPAWN разбирается");
    let mut backend = MockBackend::default();
    let mut tools = ToolRegistry::default();
    let mut sched = Scheduler::new(&mut backend, &mut tools);
    let result = sched.run(&mut graph, "spawn-test").expect("граф исполним");
    (graph, result)
}

/// Сквозной прогон: SPAWN действительно порождает узлы, и на здоровом
/// подграфе валидатор молчит (проверка на ложные тревоги).
#[test]
fn spawned_healthy_subgraph_has_no_violations() {
    let (graph, result) = run_spawn();
    assert!(
        !result.spawned.is_empty(),
        "SPAWN обязан был породить узлы, иначе тест ничего не проверяет"
    );
    let v = validate_spawned(&graph, &result);
    assert!(v.is_empty(), "здоровый подграф не должен давать нарушений: {v:#?}");
}

/// Порождённый узел читает идентификатор, которого никто не пишет.
/// MockBackend такого не выдаёт — собираем случай вручную.
#[test]
fn spawned_dangling_reference_detected() {
    let (mut graph, mut result) = run_spawn();

    let mut slots: BTreeMap<&'static str, SlotData> = BTreeMap::new();
    slots.insert(
        "where",
        SlotData::Cond("нет_такого".into(), "=".into(), "\"да\"".into()),
    );
    slots.insert("into", SlotData::Ident("решение".into()));
    let node = Node::new("подделка".into(), Subtype::Generative, Kind::Filter, slots);
    let id = node.node_id.clone();
    graph.add_node(node).expect("узел добавляется");
    result.spawned.push(id.clone());

    let v = validate_spawned(&graph, &result);
    let hit = v
        .iter()
        .find(|x| x.rule == Rule::SpawnDanglingRef)
        .unwrap_or_else(|| panic!("висячая ссылка не найдена: {v:#?}"));
    assert_eq!(hit.block, id);
    assert_eq!(hit.arrow.as_deref(), Some("нет_такого"));
    assert!(has_errors(&v));
}

/// Ссылка на идентификатор, который в графе ЕСТЬ, — законна.
#[test]
fn spawned_reference_to_existing_ident_is_legal() {
    let (mut graph, mut result) = run_spawn();

    // Берём идентификатор, реально записанный одним из порождённых узлов.
    let existing = graph
        .nodes
        .values()
        .filter_map(|n| match n.slots.get("into") {
            Some(SlotData::Ident(id)) if !id.is_empty() => Some(id.clone()),
            _ => None,
        })
        .next()
        .expect("хоть один узел что-то пишет");

    let mut slots: BTreeMap<&'static str, SlotData> = BTreeMap::new();
    slots.insert(
        "where",
        SlotData::Cond(existing.clone(), "=".into(), "\"да\"".into()),
    );
    slots.insert("into", SlotData::Ident("решение".into()));
    let node = Node::new("честный".into(), Subtype::Generative, Kind::Filter, slots);
    let id = node.node_id.clone();
    graph.add_node(node).expect("узел добавляется");
    result.spawned.push(id);

    let v = validate_spawned(&graph, &result);
    assert!(
        !v.iter().any(|x| x.rule == Rule::SpawnDanglingRef),
        "ссылка на существующий «{existing}» не должна быть нарушением: {v:#?}"
    );
}

/// Подграф, не производящий ничего: только узлы с побочным эффектом.
#[test]
fn spawned_producing_nothing_warns() {
    let mut graph = IntentGraph::default();
    let mut slots: BTreeMap<&'static str, SlotData> = BTreeMap::new();
    slots.insert("tool", SlotData::Ident("notify".into()));
    slots.insert("args", SlotData::Args(Vec::new()));
    slots.insert("policy", SlotData::Args(Vec::new()));
    let node = Node::new("только_вызов".into(), Subtype::External, Kind::Tool, slots);
    let id = node.node_id.clone();
    graph.add_node(node).expect("узел добавляется");

    let mut result = ExecutionResult::default();
    result.spawned.push(id);

    let v = validate_spawned(&graph, &result);
    assert!(
        v.iter().any(|x| x.rule == Rule::SpawnProducesNothing),
        "подграф без единого производителя должен дать предупреждение: {v:#?}"
    );
    assert!(!has_errors(&v), "это предупреждение, а не ошибка");
}

/// Граф без SPAWN: проверять нечего, нарушений быть не должно.
#[test]
fn no_spawn_no_violations() {
    let graph = luck_pilot::parse(SPAWN_SRC).expect("разбирается");
    let result = ExecutionResult::default();
    assert!(validate_spawned(&graph, &result).is_empty());
}
