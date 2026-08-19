//! Валидатор ICOM-баланса: полнота IDEF0-модели и целостность декомпозиции.
//!
//! Правило (классическое для IDEF0): граничные стрелки дочерней диаграммы
//! обязаны совпадать с ICOM родительского блока — ни одна не появляется
//! из ниоткуда, ни одна не исчезает бесследно. Проверка нужна ДО того, как
//! модель показана клиенту на подтверждение и отдана в `idef0::map_to_graph`.
//!
//! Валидатор ничего не чинит и не достраивает: он только сообщает, где
//! модель дырявая, с адресом (id блока + имя стрелки).
//!
//! Второе применение того же правила — проверка подграфа, порождённого
//! SPAWN (`validate_spawned`). По vendor/luck-engine constrained decoding
//! даёт СИНТАКСИЧЕСКУЮ валидность порождённых узлов, а max_depth/max_nodes
//! ограничивают их КОЛИЧЕСТВО; что подграф связен по данным — не проверяет
//! никто. Границы этой проверки честно описаны над `validate_spawned`.

use crate::idef0::{Block, Idef0Model};
use luck_engine::parser::{IntentGraph, SlotData};
use luck_engine::scheduler::ExecutionResult;
use std::collections::{BTreeMap, BTreeSet};

/// Ошибка делает модель неисполнимой; предупреждение — повод спросить
/// аналитика, но не повод останавливать пайп.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

/// Какое именно правило нарушено. Отдельный тип, а не строка: вызывающий
/// код (веб-вкладка «Обследование», интервью-агент) должен уметь принимать
/// решение по виду нарушения, не разбирая текст.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rule {
    /// §3.1 — выход родителя не производится ни одним ребёнком.
    MissingOutput,
    /// §3.2 — вход/управление ребёнка не приходит ни извне, ни от соседа.
    DanglingInput,
    /// §3.3 — вход/управление родителя не потребляется ни одним ребёнком.
    UnusedParentInput,
    /// §3.4 — пустой id или пустая формулировка функции.
    EmptyBlock,
    /// §3.4 — терминальный блок без объявленного выхода.
    NoOutput,
    /// §3.5 — ветка указывает на несуществующий блок.
    BranchTargetMissing,
    /// §3.5 — ветки объявлены, а kind не "branch" (или наоборот).
    BranchKindMismatch,
    /// Дубликат id: `map_to_graph` на нём паникует — ловим раньше.
    DuplicateId,
    /// Цикл по данным: граф не разрешим обходом, `Scheduler::run` вернёт Err.
    Cycle,
    /// Блок потребляет собственный выход — маппер петли пропускает, стрелка виснет.
    SelfLoop,
    /// Один поток производят несколько блоков — источник данных неоднозначен.
    DuplicateOutput,
    /// Потребитель выхода branch-блока не получит ребра: маппер строит для
    /// branch ТОЛЬКО Branch-рёбра по `branches`.
    BranchConsumerUnlinked,
    /// Блок вне потока: ни входящих, ни исходящих связей.
    OrphanBlock,
    /// Блок с декомпозицией исполняется наравне со своими детьми (`flatten`).
    ParentAlsoExecutes,
    /// Порождённый SPAWN-ом узел читает идентификатор, которого никто не пишет.
    SpawnDanglingRef,
    /// SPAWN породил узлы, ни один из которых ничего не производит.
    SpawnProducesNothing,
}

/// Нарушение с адресом. Без `block` результат бесполезен — «модель
/// невалидна» не даёт аналитику ничего.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub block: String,
    pub rule: Rule,
    pub severity: Severity,
    /// Имя стрелки (потока), если нарушение о конкретной стрелке.
    pub arrow: Option<String>,
    pub message: String,
}

impl Violation {
    fn err(block: &str, rule: Rule, arrow: Option<&str>, message: String) -> Self {
        Violation {
            block: block.to_string(),
            rule,
            severity: Severity::Error,
            arrow: arrow.map(str::to_string),
            message,
        }
    }
    fn warn(block: &str, rule: Rule, arrow: Option<&str>, message: String) -> Self {
        Violation {
            block: block.to_string(),
            rule,
            severity: Severity::Warning,
            arrow: arrow.map(str::to_string),
            message,
        }
    }
}

/// Есть ли среди нарушений хоть одна ошибка (предупреждения не считаются).
pub fn has_errors(violations: &[Violation]) -> bool {
    violations.iter().any(|v| v.severity == Severity::Error)
}

fn walk<'a>(b: &'a Block, out: &mut Vec<&'a Block>) {
    out.push(b);
    for c in &b.children {
        walk(c, out);
    }
}

/// Все блоки модели, включая корень A-0 (в отличие от `Idef0Model::flatten`,
/// который корень исключает, потому что тот — рамка, а не исполняемый шаг).
fn all_blocks(model: &Idef0Model) -> Vec<&Block> {
    let mut out = Vec::new();
    walk(&model.context, &mut out);
    out
}

/// Проверка одной декомпозиции: родитель против своих прямых детей.
/// Вынесена отдельно, потому что это и есть переносимое ядро правила —
/// то же самое проверяется для SPAWN-подграфа.
pub fn validate_decomposition(parent: &Block, children: &[Block]) -> Vec<Violation> {
    let mut out = Vec::new();
    if children.is_empty() {
        return out;
    }

    let produced: BTreeSet<&str> = children
        .iter()
        .flat_map(|c| c.outputs.iter().map(String::as_str))
        .collect();
    let external: BTreeSet<&str> = parent
        .inputs
        .iter()
        .chain(parent.controls.iter())
        .map(String::as_str)
        .collect();

    // §3.1 Полнота выхода: родитель обещает результат — декомпозиция обязана его дать.
    for o in &parent.outputs {
        if !produced.contains(o.as_str()) {
            out.push(Violation::err(
                &parent.id,
                Rule::MissingOutput,
                Some(o),
                format!(
                    "выход «{o}» блока {} не производится ни одним из его подблоков",
                    parent.id
                ),
            ));
        }
    }

    // §3.2 Обоснованность входа: данные не берутся ниоткуда.
    for c in children {
        for a in c.inputs.iter().chain(c.controls.iter()) {
            if !external.contains(a.as_str()) && !produced.contains(a.as_str()) {
                out.push(Violation::err(
                    &c.id,
                    Rule::DanglingInput,
                    Some(a),
                    format!(
                        "стрелка «{a}» блока {} висит: не приходит извне (ICOM блока {}) \
                         и не производится соседним подблоком",
                        c.id, parent.id
                    ),
                ));
            }
        }
    }

    // §3.3 Мёртвые входы родителя. Предупреждение, а не ошибка: модель
    // исполнима, но лишняя стрелка — почти всегда след незаданного вопроса
    // на обследовании, а не осознанное решение.
    let consumed: BTreeSet<&str> = children
        .iter()
        .flat_map(|c| c.inputs.iter().chain(c.controls.iter()).map(String::as_str))
        .collect();
    for a in parent.inputs.iter().chain(parent.controls.iter()) {
        if !consumed.contains(a.as_str()) {
            out.push(Violation::warn(
                &parent.id,
                Rule::UnusedParentInput,
                Some(a),
                format!(
                    "стрелка «{a}» объявлена у блока {}, но не потребляется ни одним подблоком",
                    parent.id
                ),
            ));
        }
    }

    out
}

/// Рёбра исполнения — ЗЕРКАЛО правила из `idef0::map_to_graph`. Держать в
/// соответствии с маппером обязательно: проверка, считающая связи иначе,
/// чем их строит маппер, бесполезна (и опаснее отсутствия проверки).
fn execution_edges(blocks: &[Block]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for from in blocks {
        if from.kind.as_deref() == Some("branch") && !from.branches.is_empty() {
            for (_, target) in &from.branches {
                if blocks.iter().any(|b| &b.id == target) {
                    out.push((from.id.clone(), target.clone()));
                }
            }
            continue;
        }
        for to in blocks {
            if from.id == to.id {
                continue;
            }
            let linked = from
                .outputs
                .iter()
                .any(|o| to.inputs.contains(o) || to.controls.contains(o));
            if linked {
                out.push((from.id.clone(), to.id.clone()));
            }
        }
    }
    out
}

/// Проверки уровня всего графа исполнения — те, что не видны при взгляде на
/// одну декомпозицию: цикл, петля, неоднозначный источник, сирота, ветвление.
/// Найдены атакующими тестами (`tests/icom_attack.rs`), каждая соответствует
/// реальному режиму отказа маппера или планировщика.
fn validate_execution_graph(model: &Idef0Model) -> Vec<Violation> {
    let mut out = Vec::new();
    let blocks = model.flatten();
    if blocks.is_empty() {
        return out;
    }

    // Петля: блок читает то, что сам производит. Маппер такие рёбра
    // пропускает (from_id == to_id), значит вход остаётся неудовлетворённым.
    for b in &blocks {
        for a in b.inputs.iter().chain(b.controls.iter()) {
            if b.outputs.contains(a) {
                out.push(Violation::err(
                    &b.id,
                    Rule::SelfLoop,
                    Some(a),
                    format!(
                        "блок {} потребляет «{a}», который сам же производит — \
                         связь не будет построена, вход останется без источника",
                        b.id
                    ),
                ));
            }
        }
    }

    // Неоднозначный источник: один поток пишут несколько блоков. Потребитель
    // получит Seq-рёбра от ВСЕХ, а Seq требует исполнения всех входящих —
    // модель начинает означать не то, что читается на диаграмме.
    let mut producers: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for b in &blocks {
        for o in &b.outputs {
            producers.entry(o.as_str()).or_default().push(b.id.as_str());
        }
    }
    for (flow, srcs) in producers.iter().filter(|(_, s)| s.len() > 1) {
        out.push(Violation::warn(
            srcs[0],
            Rule::DuplicateOutput,
            Some(flow),
            format!("поток «{flow}» производится несколькими блоками: {srcs:?}"),
        ));
    }

    // Потребитель выхода branch-блока вне его веток: маппер для branch строит
    // только Branch-рёбра, поэтому такой потребитель остаётся без входящего
    // ребра и исполняется как корневой — раньше своего источника данных.
    for from in &blocks {
        if from.kind.as_deref() != Some("branch") || from.branches.is_empty() {
            continue;
        }
        let targets: BTreeSet<&str> = from.branches.iter().map(|(_, t)| t.as_str()).collect();
        for to in &blocks {
            if to.id == from.id || targets.contains(to.id.as_str()) {
                continue;
            }
            for o in &from.outputs {
                if to.inputs.contains(o) || to.controls.contains(o) {
                    out.push(Violation::err(
                        &to.id,
                        Rule::BranchConsumerUnlinked,
                        Some(o),
                        format!(
                            "блок {} читает «{o}» от branch-блока {}, но не указан \
                             в его ветках — ребро построено не будет",
                            to.id, from.id
                        ),
                    ));
                }
            }
        }
    }

    // Цикл по данным: Scheduler вернёт Err («граф содержит цикл, не разрешимый
    // обходом»). Правила уровня одной декомпозиции цикл не видят — каждый вход
    // формально «производится соседом».
    let edges = execution_edges(&blocks);
    let mut indeg: BTreeMap<&str, usize> = blocks.iter().map(|b| (b.id.as_str(), 0)).collect();
    for (_, to) in &edges {
        if let Some(d) = indeg.get_mut(to.as_str()) {
            *d += 1;
        }
    }
    let mut queue: Vec<&str> = indeg
        .iter()
        .filter(|(_, d)| **d == 0)
        .map(|(id, _)| *id)
        .collect();
    let mut settled = 0usize;
    while let Some(id) = queue.pop() {
        settled += 1;
        for (from, to) in &edges {
            if from == id {
                if let Some(d) = indeg.get_mut(to.as_str()) {
                    *d -= 1;
                    if *d == 0 {
                        queue.push(to.as_str());
                    }
                }
            }
        }
    }
    if settled < blocks.len() {
        let stuck: Vec<&str> = indeg
            .iter()
            .filter(|(_, d)| **d > 0)
            .map(|(id, _)| *id)
            .collect();
        out.push(Violation::err(
            stuck.first().copied().unwrap_or("<граф>"),
            Rule::Cycle,
            None,
            format!("цикл по данным между блоками {stuck:?} — планировщик такой граф не исполнит"),
        ));
    }

    // Сирота: блок вне потока целиком.
    if blocks.len() > 1 {
        for b in &blocks {
            let has_in = edges.iter().any(|(_, to)| to == &b.id);
            let has_out = edges.iter().any(|(from, _)| from == &b.id);
            if !has_in && !has_out {
                out.push(Violation::warn(
                    &b.id,
                    Rule::OrphanBlock,
                    None,
                    format!(
                        "блок {} не связан ни с одним другим — он исполнится, \
                         но вне потока модели",
                        b.id
                    ),
                ));
            }
        }
    }

    // Родитель с детьми: `flatten` кладёт в исполнение и его, и потомков,
    // поэтому родитель дублирует работу своей же декомпозиции.
    for b in &blocks {
        if !b.children.is_empty() {
            out.push(Violation::warn(
                &b.id,
                Rule::ParentAlsoExecutes,
                None,
                format!(
                    "блок {} имеет декомпозицию и при этом исполняется сам — \
                     его работа дублирует работу подблоков",
                    b.id
                ),
            ));
        }
    }

    out
}

/// Полная проверка модели: правила §3.1–§3.5 промпта плюс проверки уровня
/// графа исполнения (`validate_execution_graph`).
pub fn validate_model(model: &Idef0Model) -> Vec<Violation> {
    let mut out = Vec::new();
    let blocks = all_blocks(model);
    let ids: BTreeSet<&str> = blocks.iter().map(|b| b.id.as_str()).collect();

    // Дубликаты id — ловим до map_to_graph, который на них паникует.
    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    for b in &blocks {
        *seen.entry(b.id.as_str()).or_insert(0) += 1;
    }
    for (id, n) in seen.iter().filter(|(_, n)| **n > 1) {
        out.push(Violation::err(
            id,
            Rule::DuplicateId,
            None,
            format!("id «{id}» встречается {n} раза — id блока должен быть уникален"),
        ));
    }

    for b in &blocks {
        // §3.4 непустые блоки
        if b.id.trim().is_empty() {
            out.push(Violation::err(
                "<без id>",
                Rule::EmptyBlock,
                None,
                format!("блок с функцией «{}» не имеет id", b.function),
            ));
        }
        if b.function.trim().is_empty() {
            out.push(Violation::err(
                &b.id,
                Rule::EmptyBlock,
                None,
                format!("блок {} не имеет формулировки функции", b.id),
            ));
        }
        // Терминальный блок без выхода: предупреждение — маппер синтезирует
        // имя `{id}_out`, модель остаётся исполнимой, но такой выход
        // невозможно связать ребром, т.е. блок выпадает из потока.
        if b.outputs.is_empty() && b.children.is_empty() {
            out.push(Violation::warn(
                &b.id,
                Rule::NoOutput,
                None,
                format!(
                    "блок {} не объявляет выхода — его результат не связывается с другими блоками",
                    b.id
                ),
            ));
        }

        // §3.5 ветвления
        let is_branch = b.kind.as_deref() == Some("branch");
        if !b.branches.is_empty() && !is_branch {
            out.push(Violation::err(
                &b.id,
                Rule::BranchKindMismatch,
                None,
                format!(
                    "блок {} объявляет ветки, но его kind = {:?}, а не \"branch\" — \
                     Branch-рёбра построены не будут",
                    b.id, b.kind
                ),
            ));
        }
        if is_branch && b.branches.is_empty() {
            out.push(Violation::err(
                &b.id,
                Rule::BranchKindMismatch,
                None,
                format!("блок {} объявлен как \"branch\", но не задаёт ни одной ветки", b.id),
            ));
        }
        for (label, target) in &b.branches {
            if !ids.contains(target.as_str()) {
                out.push(Violation::err(
                    &b.id,
                    Rule::BranchTargetMissing,
                    Some(label),
                    format!(
                        "ветка «{label}» блока {} указывает на несуществующий блок «{target}»",
                        b.id
                    ),
                ));
            }
        }

        // §3.1–§3.3 — на каждом уровне декомпозиции
        out.extend(validate_decomposition(b, &b.children));
    }

    out.extend(validate_execution_graph(model));
    out
}

/// Проверка подграфа, порождённого SPAWN, ПОСТФАКТУМ — после `Scheduler::run`,
/// по финальному графу и `ExecutionResult::spawned`.
///
/// ГРАНИЦА ПРИМЕНИМОСТИ (важно, не недоделка). Полный ICOM-баланс здесь
/// недостижим, и не из-за реализации: `Scheduler::spawn` — приватная функция
/// внутри `run()` в vendor/luck-engine, вклиниться в момент порождения нельзя
/// без правки вендора (запрещена проектной политикой). Но главное — глубже:
/// **узел SPAWN не объявляет ICOM ожидаемого подграфа**. Его слот `PLAN` —
/// свободный текст, а `INTO` принимает текст самого плана, а не результат
/// подграфа. Сверять «выход подграфа = обещанный выход родителя» (§3.1) не с
/// чем: обещание не выражено машинно-проверяемо.
///
/// Поэтому здесь проверяется только то, что проверяемо честно, — аналог §3.2
/// (обоснованность входа): порождённый узел не должен читать идентификатор,
/// которого никто в графе не пишет. Что нужно, чтобы закрыть §3.1, описано
/// в отчёте `docs/ICOM-VALIDATOR.md`.
pub fn validate_spawned(graph: &IntentGraph, result: &ExecutionResult) -> Vec<Violation> {
    let mut out = Vec::new();
    if result.spawned.is_empty() {
        return out;
    }

    // Что вообще пишется в графе: все идентификаторы из слотов `into`.
    let mut written: BTreeSet<String> = BTreeSet::new();
    for node in graph.nodes.values() {
        if let Some(SlotData::Ident(id)) = node.slots.get("into") {
            if !id.is_empty() {
                written.insert(id.clone());
            }
        }
    }

    let mut produces_anything = false;
    for id in &result.spawned {
        let Some(node) = graph.nodes.get(id) else {
            continue;
        };
        if let Some(SlotData::Ident(into)) = node.slots.get("into") {
            if !into.is_empty() {
                produces_anything = true;
            }
        }
        // Чтения: условие FILTER ссылается на идентификатор, записанный ранее.
        if let Some(SlotData::Cond(field, _, _)) = node.slots.get("where") {
            if !written.contains(field) {
                out.push(Violation::err(
                    id,
                    Rule::SpawnDanglingRef,
                    Some(field),
                    format!(
                        "порождённый узел {id} читает «{field}», который не производит \
                         ни один узел графа"
                    ),
                ));
            }
        }
    }

    if !produces_anything {
        out.push(Violation::warn(
            "<spawn>",
            Rule::SpawnProducesNothing,
            None,
            format!(
                "SPAWN породил {} узлов, ни один из которых ничего не производит",
                result.spawned.len()
            ),
        ));
    }

    out
}

/// Обёртка над `idef0::map_to_graph` с проверкой ДО построения графа.
/// Предупреждения не блокируют — возвращаются вместе с графом.
pub fn map_to_graph_checked(
    model: &Idef0Model,
) -> Result<(IntentGraph, Vec<Violation>), Vec<Violation>> {
    let violations = validate_model(model);
    if has_errors(&violations) {
        return Err(violations);
    }
    Ok((crate::idef0::map_to_graph(model), violations))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blk(id: &str, f: &str, inputs: &[&str], outputs: &[&str]) -> Block {
        Block {
            id: id.into(),
            function: f.into(),
            kind: None,
            inputs: inputs.iter().map(|s| s.to_string()).collect(),
            controls: vec![],
            outputs: outputs.iter().map(|s| s.to_string()).collect(),
            mechanisms: vec![],
            children: vec![],
            branches: vec![],
        }
    }

    /// Демо-модель проекта обязана проходить проверку — она эталон того,
    /// как выглядит корректная модель обследования.
    #[test]
    fn demo_model_passes() {
        let json = include_str!("../../examples_luck/horeca-model.json");
        let model: Idef0Model = serde_json::from_str(json).expect("демо-модель парсится");
        let v = validate_model(&model);
        let errors: Vec<_> = v.iter().filter(|x| x.severity == Severity::Error).collect();
        assert!(errors.is_empty(), "демо-модель должна быть без ошибок: {errors:#?}");
    }

    #[test]
    fn rule1_missing_output_detected() {
        let model = Idef0Model {
            context: Block {
                outputs: vec!["отчёт".into()],
                children: vec![blk("A1", "шаг", &["заказы"], &["нечто_другое"])],
                ..blk("A0", "рамка", &["заказы"], &[])
            },
        };
        let v = validate_model(&model);
        let hit = v.iter().find(|x| x.rule == Rule::MissingOutput).expect("найдено");
        assert_eq!(hit.block, "A0");
        assert_eq!(hit.arrow.as_deref(), Some("отчёт"));
        assert_eq!(hit.severity, Severity::Error);
    }

    #[test]
    fn rule2_dangling_input_detected() {
        let model = Idef0Model {
            context: Block {
                outputs: vec!["out".into()],
                children: vec![blk("A1", "шаг", &["ниоткуда"], &["out"])],
                ..blk("A0", "рамка", &["заказы"], &[])
            },
        };
        let v = validate_model(&model);
        let hit = v.iter().find(|x| x.rule == Rule::DanglingInput).expect("найдено");
        assert_eq!(hit.block, "A1");
        assert_eq!(hit.arrow.as_deref(), Some("ниоткуда"));
    }

    #[test]
    fn rule3_unused_parent_input_is_warning() {
        let model = Idef0Model {
            context: Block {
                outputs: vec!["out".into()],
                children: vec![blk("A1", "шаг", &[], &["out"])],
                ..blk("A0", "рамка", &["никому_не_нужен"], &[])
            },
        };
        let v = validate_model(&model);
        let hit = v.iter().find(|x| x.rule == Rule::UnusedParentInput).expect("найдено");
        assert_eq!(hit.severity, Severity::Warning);
        assert_eq!(hit.arrow.as_deref(), Some("никому_не_нужен"));
        assert!(!has_errors(&v), "мёртвый вход не должен блокировать модель");
    }

    #[test]
    fn rule4_empty_block_detected() {
        let model = Idef0Model {
            context: Block {
                outputs: vec!["out".into()],
                children: vec![blk("A1", "   ", &[], &["out"])],
                ..blk("A0", "рамка", &[], &[])
            },
        };
        let v = validate_model(&model);
        let hit = v.iter().find(|x| x.rule == Rule::EmptyBlock).expect("найдено");
        assert_eq!(hit.block, "A1");
    }

    #[test]
    fn rule5_branch_target_missing_detected() {
        let mut b = blk("A1", "решить", &[], &["out"]);
        b.kind = Some("branch".into());
        b.branches = vec![("ok".into(), "A9".into())];
        let model = Idef0Model {
            context: Block {
                outputs: vec!["out".into()],
                children: vec![b],
                ..blk("A0", "рамка", &[], &[])
            },
        };
        let v = validate_model(&model);
        let hit = v.iter().find(|x| x.rule == Rule::BranchTargetMissing).expect("найдено");
        assert_eq!(hit.block, "A1");
        assert_eq!(hit.arrow.as_deref(), Some("ok"));
    }

    #[test]
    fn rule5_branches_without_branch_kind_detected() {
        let mut b = blk("A1", "решить", &[], &["out"]);
        b.branches = vec![("ok".into(), "A1".into())];
        let model = Idef0Model {
            context: Block {
                outputs: vec!["out".into()],
                children: vec![b],
                ..blk("A0", "рамка", &[], &[])
            },
        };
        let v = validate_model(&model);
        assert!(v.iter().any(|x| x.rule == Rule::BranchKindMismatch));
    }

    #[test]
    fn duplicate_id_detected_before_mapper_panics() {
        let model = Idef0Model {
            context: Block {
                outputs: vec!["out".into()],
                children: vec![
                    blk("A1", "шаг", &[], &["mid"]),
                    blk("A1", "дубль", &["mid"], &["out"]),
                ],
                ..blk("A0", "рамка", &[], &[])
            },
        };
        let v = validate_model(&model);
        assert!(v.iter().any(|x| x.rule == Rule::DuplicateId));
        assert!(map_to_graph_checked(&model).is_err(), "дубль не должен дойти до маппера");
    }

    #[test]
    fn checked_mapper_passes_valid_model_through() {
        let json = include_str!("../../examples_luck/horeca-model.json");
        let model: Idef0Model = serde_json::from_str(json).expect("демо-модель парсится");
        let (graph, warnings) = map_to_graph_checked(&model).expect("валидная модель");
        assert!(!graph.nodes.is_empty());
        assert!(!has_errors(&warnings));
    }
}
