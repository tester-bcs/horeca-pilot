//! Luck — Срез 6: планировщик обхода
//! Ветка: Rust-проход (Маркер В.0 -> возврат)
//!
//! Зеркалит Scheduler из luck/interpreter.py: инкрементальный обход
//! (граф может расти во время исполнения — ребро 6->1), активация рёбер
//! по фактическому выводу источника, кэш вычислений по cache_key
//! (НЕ по branch_id — Ядро п.3), отказ как узел графа (Этап Б, Фаза 5).
//!
//! ОТЛИЧИЕ ОТ PYTHON-ВЕТКИ (MARKER_V0_RUST п.5, фиксирую явно):
//! `ModelBackend::generate` в Rust принимает `kind`/`slots` параметрами,
//! а не через мутируемое поле `current_node`, которое планировщик в
//! Python выставляет перед вызовом (`self.backend.current_node = ...`).
//! Тот side-channel существовал в Python из-за фиксированной сигнатуры
//! протокола `generate(prompt, constraint)`; в Rust типаж можно было
//! спроектировать без скрытого состояния сразу — там, где Python решал
//! ограничение языка/протокола мутацией, Rust решает его сигнатурой.
//! Это НЕ вопрос владения branch_id при конкурентном порождении (вопрос
//! 2 из MARKER_V0_RUST §4) — тот остаётся открытым, см. ниже в run().

use crate::decoding::{self, Constraint};
use crate::parser::{Node, SlotData, parse_fragment};
use crate::registry::{Kind, Subtype};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

use crate::parser::{Edge, EdgeType, IntentGraph};

/// Зеркалит InvalidModelOutput — КОНТРАКТ РЕБРА 5->6: невалидный вывод
/// модели останавливает обход этой ветки, представлен узлом графа,
/// не исключением рантайма (это гарантирует Scheduler::run, ловя Err).
///
/// `fatal` — РАСШИРЕНИЕ этого контракта, а не часть исходного Ядра:
/// найдено на практике (реальный прогон с невалидным API-ключом дал
/// 401, свёрнутый в reject(SYNTAX) неотличимо от настоящего провала
/// грамматики — вводящий в заблуждение результат). Транспортный сбой,
/// который заведомо не исправится повтором ни на одном узле графа
/// (неверный ключ, битый ответ сервера), — не то же самое, что
/// "модель не осилила грамматику" на КОНКРЕТНОМ узле. `fatal=true`
/// останавливает ВЕСЬ обход (Scheduler::run возвращает Err), не
/// поглощается как reject одного узла. `fatal=false` (по умолчанию у
/// всех прежних источников ошибок — MockBackend, AdversarialBackend,
/// временные сетевые сбои) сохраняет исходное поведение без изменений.
#[derive(Debug, Clone)]
pub struct InvalidModelOutput {
    pub reason: String,
    pub fatal: bool,
}

impl InvalidModelOutput {
    pub fn new(reason: impl Into<String>) -> Self {
        InvalidModelOutput {
            reason: reason.into(),
            fatal: false,
        }
    }

    pub fn fatal(reason: impl Into<String>) -> Self {
        InvalidModelOutput {
            reason: reason.into(),
            fatal: true,
        }
    }
}

pub trait ModelBackend {
    fn generate(
        &mut self,
        prompt: &str,
        constraint: &Constraint,
        kind: Kind,
        slots: &BTreeMap<&'static str, SlotData>,
    ) -> Result<String, InvalidModelOutput>;

    fn call_count(&self) -> u64 {
        0
    }
}

/// Детерминированный бэкенд — функция от (prompt, constraint.ebnf).
/// Детерминизм принципиален: позволяет проверить, что совпадение
/// результатов между ветками вызвано кэшем (Срез 3), а не случайностью.
#[derive(Default)]
pub struct MockBackend {
    pub calls: u64,
}

impl MockBackend {
    pub fn new() -> Self {
        Self::default()
    }

    fn digest(prompt: &str, ebnf: &str) -> String {
        let mut h = Sha256::new();
        h.update(prompt.as_bytes());
        h.update(ebnf.as_bytes());
        let d = h.finalize();
        d.iter().map(|b| format!("{b:02x}")).collect::<String>()[..8].to_string()
    }
}

impl ModelBackend for MockBackend {
    fn generate(
        &mut self,
        prompt: &str,
        constraint: &Constraint,
        kind: Kind,
        _slots: &BTreeMap<&'static str, SlotData>,
    ) -> Result<String, InvalidModelOutput> {
        self.calls += 1;
        let digest = Self::digest(prompt, &constraint.definition);
        Ok(match kind {
            Kind::Filter => {
                let n = u32::from_str_radix(&digest, 16).unwrap_or(0);
                if n.is_multiple_of(2) {
                    "MATCH".to_string()
                } else {
                    "NO_MATCH".to_string()
                }
            }
            Kind::Role => String::new(),
            Kind::Step => format!("RESULT:{digest}"),
            Kind::Spawn => format!(
                "NODE sp_{digest}_a: STEP [GENERATIVE]\n  DO \"generated substep\"\n  INTO out_{digest}\nEND\n\
                 NODE sp_{digest}_b: TOOL [EXTERNAL]\n  CALL notify WITH target=\"ops\"\nEND"
            ),
            _ => format!("generated<{digest}>"),
        })
    }

    fn call_count(&self) -> u64 {
        self.calls
    }
}

type ToolFn = Box<dyn Fn(&BTreeMap<String, String>) -> String>;

/// Реестр внешних инструментов (EXTERNAL-узлы). decoding неприменим
/// по определению Ядра п.4 — вывод приходит извне.
#[derive(Default)]
pub struct ToolRegistry {
    tools: BTreeMap<String, ToolFn>,
    pub call_count: u64,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(
        &mut self,
        name: &str,
        f: impl Fn(&BTreeMap<String, String>) -> String + 'static,
    ) {
        self.tools.insert(name.to_string(), Box::new(f));
    }

    fn call(&mut self, name: &str, args: &BTreeMap<String, String>) -> Option<String> {
        let f = self.tools.get(name)?;
        self.call_count += 1;
        Some(f(args))
    }
}

/// Кэш переиспользования вычислений — ключ ТОЛЬКО от содержимого
/// (composite_key = cache_key узла + промпт), НЕ от branch_id (Ядро п.3).
#[derive(Default)]
pub struct ComputationCache {
    store: BTreeMap<String, String>,
    pub hits: u64,
    pub misses: u64,
}

impl ComputationCache {
    pub fn new() -> Self {
        Self::default()
    }

    fn get(&mut self, key: &str) -> Option<String> {
        if let Some(v) = self.store.get(key) {
            self.hits += 1;
            Some(v.clone())
        } else {
            self.misses += 1;
            None
        }
    }

    fn put(&mut self, key: String, value: String) {
        self.store.insert(key, value);
    }
}

#[derive(Default, Debug)]
pub struct ExecutionResult {
    pub outputs: BTreeMap<String, String>,
    pub rejects: Vec<Node>,
    pub order: Vec<String>,
    pub skipped: Vec<String>,
    pub spawned: Vec<String>,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub model_calls: u64,
    pub tool_calls: u64,
}

/// Метка ребра -> ожидаемый вывод узла-источника.
/// '[on_match]' -> 'MATCH', '[on_no_match]' -> 'NO_MATCH'.
fn normalize_label(label: &str) -> String {
    let text = label.strip_prefix("on_").unwrap_or(label);
    text.to_uppercase()
}

pub struct Scheduler<'a> {
    backend: &'a mut dyn ModelBackend,
    tools: &'a mut ToolRegistry,
    cache: ComputationCache,
    max_depth: u32,
    max_nodes: usize,
    depth: BTreeMap<String, u32>,
    /// §4.1 MARKER_V0_RUST.md: инструментирование реальных промптов
    /// префиксным деревом — измерение, не замена ComputationCache
    /// (см. докстринг prefix_cache.rs). None по умолчанию: измерение —
    /// не часть обычного контракта планировщика, включается явно.
    prefix_trie: Option<crate::prefix_cache::PrefixTrie>,
}

impl<'a> Scheduler<'a> {
    pub fn new(backend: &'a mut dyn ModelBackend, tools: &'a mut ToolRegistry) -> Self {
        Scheduler {
            backend,
            tools,
            cache: ComputationCache::new(),
            max_depth: 3,
            max_nodes: 64,
            depth: BTreeMap::new(),
            prefix_trie: None,
        }
    }

    pub fn with_limits(mut self, max_depth: u32, max_nodes: usize) -> Self {
        self.max_depth = max_depth;
        self.max_nodes = max_nodes;
        self
    }

    pub fn with_prefix_tracking(mut self) -> Self {
        self.prefix_trie = Some(crate::prefix_cache::PrefixTrie::new());
        self
    }

    pub fn prefix_stats(&self) -> Option<&crate::prefix_cache::PrefixTrie> {
        self.prefix_trie.as_ref()
    }

    fn make_reject(cause_node_id: &str, reason: &str, branch_id: &str) -> Node {
        let mut slots: BTreeMap<&'static str, SlotData> = BTreeMap::new();
        slots.insert("cause", SlotData::Ident(cause_node_id.to_string()));
        slots.insert("reason", SlotData::Ident(reason.to_string()));
        let mut n = Node::new(
            format!("__reject__{cause_node_id}"),
            Subtype::Reject,
            Kind::RejectMark,
            slots,
        );
        n.branch_id = branch_id.to_string();
        n
    }

    fn edge_is_active(
        edge: &Edge,
        outputs: &BTreeMap<String, String>,
        executed: &BTreeSet<String>,
    ) -> bool {
        if !executed.contains(&edge.source) {
            return false;
        }
        if edge.edge_type == EdgeType::Branch
            && let Some(label) = &edge.label
        {
            let expected = normalize_label(label);
            let actual = outputs
                .get(&edge.source)
                .map(|s| s.trim().to_uppercase())
                .unwrap_or_default();
            return actual == expected;
        }
        true
    }

    /// Узел исполняется, если: все входящие SEQ активны (конъюнкция),
    /// хотя бы одно входящее BRANCH активно (дизъюнкция — выбор ветки),
    /// хотя бы одно входящее MERGE активно. Без входящих рёбер — активен
    /// всегда (корень графа).
    fn node_is_active(
        graph: &IntentGraph,
        node_id: &str,
        outputs: &BTreeMap<String, String>,
        executed: &BTreeSet<String>,
    ) -> bool {
        let incoming: Vec<&Edge> = graph.edges.iter().filter(|e| e.target == node_id).collect();
        if incoming.is_empty() {
            return true;
        }
        let seq: Vec<&&Edge> = incoming
            .iter()
            .filter(|e| e.edge_type == EdgeType::Seq)
            .collect();
        let branch: Vec<&&Edge> = incoming
            .iter()
            .filter(|e| e.edge_type == EdgeType::Branch)
            .collect();
        let merge: Vec<&&Edge> = incoming
            .iter()
            .filter(|e| e.edge_type == EdgeType::Merge)
            .collect();

        if !seq.is_empty()
            && !seq
                .iter()
                .all(|e| Self::edge_is_active(e, outputs, executed))
        {
            return false;
        }
        if !branch.is_empty()
            && !branch
                .iter()
                .any(|e| Self::edge_is_active(e, outputs, executed))
        {
            return false;
        }
        if !merge.is_empty()
            && !merge
                .iter()
                .any(|e| Self::edge_is_active(e, outputs, executed))
        {
            return false;
        }
        true
    }

    /// Промпт узла = его поверхностный синтаксис, предварённый
    /// поверхностным синтаксисом предков (docs/EMPIRICAL_REAL_MODEL.md,
    /// раздел 11 в Python-ветке — перенесено сюда как решение, не
    /// переоткрыто).
    fn build_prompt(
        graph: &IntentGraph,
        node: &Node,
        outputs: &BTreeMap<String, String>,
    ) -> String {
        let parents: Vec<&String> = graph
            .edges
            .iter()
            .filter(|e| e.target == node.node_id)
            .map(|e| &e.source)
            .collect();
        let mut sorted_parents: Vec<&String> = parents;
        sorted_parents.sort();
        sorted_parents.dedup();

        let mut parts = Vec::new();
        for p in sorted_parents {
            if let Some(pnode) = graph.nodes.get(p) {
                parts.push(decoding::node_surface_form(
                    pnode.kind,
                    pnode.subtype,
                    &pnode.node_id,
                    &pnode.slots,
                ));
                if let Some(out) = outputs.get(p) {
                    parts.push(format!("=> {out}"));
                }
            }
        }
        parts.push(decoding::node_surface_form(
            node.kind,
            node.subtype,
            &node.node_id,
            &node.slots,
        ));
        parts.join("\n")
    }

    fn ready_nodes(graph: &IntentGraph, processed: &BTreeSet<String>) -> Vec<String> {
        let mut ready = Vec::new();
        for nid in graph.nodes.keys() {
            if processed.contains(nid) {
                continue;
            }
            let parents_done = graph
                .edges
                .iter()
                .filter(|e| &e.target == nid)
                .all(|e| processed.contains(&e.source));
            if parents_done {
                ready.push(nid.clone());
            }
        }
        ready
    }

    /// Ребро 6->1 (вариант A): разбирает вывод SPAWN как объявления узлов
    /// Luck, вставляет их, соединяя SEQ-ребром от породившего узла.
    /// Каноничность порождённого гарантирована грамматикой SPAWN (Срез 5,
    /// validate_output), не проверяется здесь постфактум повторно.
    ///
    /// ОТКРЫТЫЙ ВОПРОС MARKER_V0_RUST §4.2 (владение branch_id при
    /// конкурентном порождении): здесь порождение — часть однопоточного
    /// `run()`, конкурентности нет, поэтому вопрос "кто первым запишет
    /// branch_id при одновременной записи" НЕ возникает и НЕ решён —
    /// формула "branch_id наследуется от актора" (Фаза 5) применена, но
    /// протокол присвоения при параллельном исполнении остаётся
    /// непроверенным этим Шагом. Однопоточность здесь — область
    /// применимости, не ответ на вопрос.
    fn spawn(
        graph: &mut IntentGraph,
        node: &Node,
        output: &str,
        result: &mut ExecutionResult,
        branch_id: &str,
        depth: u32,
        depth_map: &mut BTreeMap<String, u32>,
    ) {
        let Ok(new_nodes) = parse_fragment(output) else {
            return;
        };
        for mut new_node in new_nodes {
            new_node.branch_id = branch_id.to_string();
            if graph.nodes.contains_key(&new_node.node_id) {
                continue;
            }
            let new_id = new_node.node_id.clone();
            if graph.add_node(new_node).is_err() {
                continue;
            }
            graph.add_edge(Edge {
                source: node.node_id.clone(),
                target: new_id.clone(),
                edge_type: EdgeType::Seq,
                label: None,
            });
            depth_map.insert(new_id.clone(), depth + 1);
            result.spawned.push(new_id);
        }
    }

    /// `Err(String)` — фатальный сбой транспорта (`InvalidModelOutput::
    /// fatal`, см. anthropic.rs), останавливает ВЕСЬ обход, не только
    /// этот узел. Расширение контракта ребра 5->6, не пересмотр: обычный
    /// невалидный вывод по-прежнему становится reject(SYNTAX) ОДНОГО
    /// узла через `Ok(None)`, как и раньше.
    fn execute(
        &mut self,
        node: &Node,
        prompt: &str,
        result: &mut ExecutionResult,
        branch_id: &str,
    ) -> Result<Option<String>, String> {
        match node.subtype {
            Subtype::Generative => {
                let Some(constraint) = decoding::build_constraint(node.kind, &node.slots) else {
                    result
                        .rejects
                        .push(Self::make_reject(&node.node_id, "TYPE", branch_id));
                    return Ok(None);
                };
                match self
                    .backend
                    .generate(prompt, &constraint, node.kind, &node.slots)
                {
                    Ok(out) => Ok(Some(out)),
                    Err(e) if e.fatal => Err(format!(
                        "фатальный сбой транспорта на узле '{}': {}",
                        node.node_id, e.reason
                    )),
                    // КОНТРАКТ РЕБРА 5->6: невалидный вывод останавливает
                    // обход этой ветки, представлен reject-узлом.
                    Err(_) => {
                        result
                            .rejects
                            .push(Self::make_reject(&node.node_id, "SYNTAX", branch_id));
                        Ok(None)
                    }
                }
            }
            Subtype::External => {
                if node.kind == Kind::Tool {
                    let tool_name = match node.slots.get("tool") {
                        Some(SlotData::Ident(v)) => v.clone(),
                        _ => return Ok(None),
                    };
                    let args = match node.slots.get("args") {
                        Some(SlotData::Args(pairs)) => {
                            pairs.iter().map(|(n, v)| (n.clone(), unquote(v))).collect()
                        }
                        _ => BTreeMap::new(),
                    };
                    return Ok(match self.tools.call(&tool_name, &args) {
                        Some(v) => Some(v),
                        None => {
                            result.rejects.push(Self::make_reject(
                                &node.node_id,
                                "TYPE",
                                branch_id,
                            ));
                            None
                        }
                    });
                }
                Ok(match node.slots.get("from") {
                    Some(SlotData::Str(v)) => Some(format!("<document:{v}>")),
                    _ => None,
                })
            }
            Subtype::Reject => Ok(None),
        }
    }

    /// Главный цикл: инкрементальный обход, граф может расти во время
    /// исполнения (ребро 6->1), поэтому готовые узлы пересчитываются
    /// каждую итерацию — предвычисленный порядок непригоден.
    pub fn run(
        &mut self,
        graph: &mut IntentGraph,
        branch_id: &str,
    ) -> Result<ExecutionResult, String> {
        let mut result = ExecutionResult::default();
        let mut executed: BTreeSet<String> = BTreeSet::new();
        let mut processed: BTreeSet<String> = BTreeSet::new();
        self.depth = graph.nodes.keys().map(|k| (k.clone(), 0)).collect();

        loop {
            let ready = Self::ready_nodes(graph, &processed);
            if ready.is_empty() {
                break;
            }

            for node_id in ready {
                if processed.contains(&node_id) {
                    continue;
                }
                processed.insert(node_id.clone());
                let node = graph.nodes[&node_id].clone();

                if node.subtype == Subtype::Reject {
                    result.order.push(node_id.clone());
                    result.rejects.push(node);
                    continue;
                }

                if !Self::node_is_active(graph, &node_id, &result.outputs, &executed) {
                    result.skipped.push(node_id);
                    continue;
                }

                result.order.push(node_id.clone());
                let prompt = Self::build_prompt(graph, &node, &result.outputs);
                if let Some(trie) = &mut self.prefix_trie {
                    trie.insert(&prompt);
                }
                let mut h = Sha256::new();
                h.update(node.cache_key.as_bytes());
                h.update(b"|");
                h.update(prompt.as_bytes());
                let composite_key = h
                    .finalize()
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>()[..16]
                    .to_string();

                let output = if let Some(cached) = self.cache.get(&composite_key) {
                    cached
                } else {
                    // `?` здесь пробрасывает ТОЛЬКО fatal-сбой транспорта
                    // (Err(String)) из execute наружу из run() целиком;
                    // обычный невалидный вывод отражён как Ok(None) и
                    // обрабатывается веткой ниже как раньше.
                    let Some(out) = self.execute(&node, &prompt, &mut result, branch_id)? else {
                        continue;
                    };
                    self.cache.put(composite_key, out.clone());
                    out
                };

                result.outputs.insert(node_id.clone(), output.clone());
                executed.insert(node_id.clone());

                // --- ребро 6->1: порождение новых узлов ---
                if node.kind == Kind::Spawn {
                    let depth = *self.depth.get(&node_id).unwrap_or(&0);
                    let over_depth = depth + 1 > self.max_depth;
                    let over_count = graph.nodes.len() >= self.max_nodes;
                    if over_depth || over_count {
                        // Исчерпание бюджета — не синтаксис, не тип; по
                        // Ядру ('что не помещается в граф как узел, не
                        // является частью Luck') тоже reject-узел.
                        result
                            .rejects
                            .push(Self::make_reject(&node_id, "BUDGET", branch_id));
                    } else {
                        Self::spawn(
                            graph,
                            &node,
                            &output,
                            &mut result,
                            branch_id,
                            depth,
                            &mut self.depth,
                        );
                    }
                }
            }
        }

        let unprocessed: Vec<&String> = graph
            .nodes
            .keys()
            .filter(|k| !processed.contains(*k))
            .collect();
        if !unprocessed.is_empty() {
            return Err(format!(
                "граф содержит цикл, не разрешимый обходом: {unprocessed:?}"
            ));
        }

        result.cache_hits = self.cache.hits;
        result.cache_misses = self.cache.misses;
        result.model_calls = self.backend.call_count();
        result.tool_calls = self.tools.call_count;
        Ok(result)
    }
}

fn unquote(v: &str) -> String {
    if v.starts_with('"') && v.ends_with('"') && v.len() >= 2 {
        v[1..v.len() - 1].to_string()
    } else {
        v.to_string()
    }
}
