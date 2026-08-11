//! Luck — Rust-проход (Маркер В.0 -> возврат)
//! Точка входа возврата: docs/MARKER_V0_RUST.md
//!
//! Этап В: Шаг 1 (Срез 1 синтаксис, Срез 2 типы, частично Срез 3
//! идентичность — см. parser.rs), Шаг 2 (Срез 5 constrained decoding —
//! см. decoding.rs), Шаг 3 (Срез 6 планировщик — см. scheduler.rs),
//! Шаг 4 (Срез 4 презентация — см. presentation.rs), Шаг 5 (вопрос §4.1
//! MARKER_V0_RUST.md — см. prefix_cache.rs), Шаг 6 (вопрос §4.2 — см.
//! concurrent_spawn.rs), практическая демонстрация на реальном Claude
//! (см. anthropic.rs, src/bin/run_real.rs).

pub mod anthropic;
pub mod concurrent_spawn;
pub mod decoding;
pub mod lexer;
pub mod parser;
pub mod prefix_cache;
pub mod presentation;
pub mod registry;
pub mod report;
pub mod scheduler;
pub mod tools;

pub use anthropic::{
    AnthropicTransport, ChatTransport, TransportError,
    ValidatingBackend as AnthropicValidatingBackend,
};
pub use concurrent_spawn::spawn_concurrent;
pub use decoding::{
    Constraint, build_constraint, example_node_source, node_surface_form, singleton_output,
    validate_output,
};
pub use lexer::escape_string_literal;
pub use parser::{Edge, EdgeType, IntentGraph, LuckParseError, Node, parse, parse_fragment};
pub use prefix_cache::PrefixTrie;
pub use presentation::{
    Element, PresentationError, Style, from_presentation, node_round_trip, to_presentation,
};
pub use registry::{Kind, Subtype};
pub use report::format_execution_report;
pub use scheduler::{
    ComputationCache, ExecutionResult, MockBackend, ModelBackend, Scheduler, ToolRegistry,
};
pub use tools::register_default_tools;

#[cfg(test)]
mod tests {
    use super::*;
    use parser::SlotData;
    use registry::Kind::*;
    use std::collections::BTreeMap;

    const SAMPLE: &str = r#"
    NODE n1: ROLE [GENERATIVE]
      AS "senior architect"
    END

    NODE n2: FILTER [GENERATIVE]
      WHERE status = "draft"
      INTO n3
    END

    NODE n3: TOOL [EXTERNAL]
      CALL search WITH query="Luck grammar", limit=5
    END

    EDGES:
      n1 -> n2
      n2 => n3 [on_match]
    END
    "#;

    #[test]
    fn parses_all_node_kinds() {
        let g = parse(SAMPLE).expect("парсится");
        assert_eq!(g.nodes.len(), 3);
        assert_eq!(g.nodes["n1"].kind, Role);
        assert_eq!(g.nodes["n2"].kind, Filter);
        assert_eq!(g.nodes["n3"].kind, Tool);
        assert_eq!(g.edges.len(), 2);
    }

    /// Ядро п.3 (аналог инварианта 1 из Python): cache_key НЕ зависит от
    /// branch_id — тот же узел под другим актором даёт тот же ключ.
    #[test]
    fn cache_key_independent_of_branch() {
        let g1 = parse(SAMPLE).unwrap();
        let mut g2 = parse(SAMPLE).unwrap();
        for n in g2.nodes.values_mut() {
            n.branch_id = "bob".to_string();
        }
        for id in g1.nodes.keys() {
            assert_eq!(g1.nodes[id].cache_key, g2.nodes[id].cache_key, "узел {id}");
        }
    }

    /// Ядро п.2 (аналог инварианта 2): разные значения слота -> разный
    /// cache_key; идентичные узлы (даже с разным порядком ARGS для
    /// нормализуемых слотов) -> идентичный cache_key.
    #[test]
    fn prefix_stability_and_normalization() {
        let a = parse(r#"NODE t: TOOL [EXTERNAL] CALL s WITH q="x", n=5 END EDGES: END"#).unwrap();
        let b = parse(r#"NODE t: TOOL [EXTERNAL] CALL s WITH n=5, q="x" END EDGES: END"#).unwrap();
        assert_eq!(
            a.nodes["t"].cache_key, b.nodes["t"].cache_key,
            "порядок ARGS не влияет на cache_key"
        );

        let c = parse(r#"NODE t: TOOL [EXTERNAL] CALL s WITH q="y", n=5 END EDGES: END"#).unwrap();
        assert_ne!(
            a.nodes["t"].cache_key, c.nodes["t"].cache_key,
            "разное значение -> разный cache_key"
        );
    }

    /// Ядро п.4: decoding применим только к generative-подтипу — EXTERNAL/
    /// REJECT возвращают has_decoding()==false ПО ОПРЕДЕЛЕНИЮ реестра.
    #[test]
    fn decoding_only_for_generative() {
        assert!(registry::spec(Role).has_decoding());
        assert!(registry::spec(Filter).has_decoding());
        assert!(!registry::spec(Tool).has_decoding());
        assert!(!registry::spec(Document).has_decoding());
        assert!(!registry::spec(RejectMark).has_decoding());
    }

    /// Ошибка авторства статического графа (kind/subtype mismatch) —
    /// поднимается как Result::Err на этапе парсинга, не как reject-узел
    /// (графа ещё не существует). Зеркалит Python-инвариант 6.
    #[test]
    fn kind_subtype_mismatch_is_parse_error() {
        let src = r#"NODE n: ROLE [EXTERNAL] AS "x" END EDGES: END"#;
        assert!(parse(src).is_err());
    }

    /// Висячее ребро — тоже ошибка авторства статического графа.
    #[test]
    fn dangling_edge_is_parse_error() {
        let src = r#"NODE n: ROLE [GENERATIVE] AS "x" END EDGES: n -> ghost END"#;
        assert!(parse(src).is_err());
    }

    /// Реестр — единственный источник истины (Python-инвариант 7,
    /// расхождение 2.4): все 9 типов реестра лексируются, парсятся и
    /// компилируются без единой правки лексера/парсера сверх регистрации.
    #[test]
    fn registry_covers_all_kinds() {
        assert_eq!(Kind::ALL.len(), 9);
        for k in Kind::ALL {
            let spec = registry::spec(k);
            assert_eq!(spec.kind, k);
        }
    }

    /// parse_fragment — то, чем воспользуется планировщик (Срез 6) для
    /// разбора вывода SPAWN: узлы без блока EDGES.
    #[test]
    fn parse_fragment_without_edges() {
        let nodes = parse_fragment(r#"NODE x: STEP [GENERATIVE] DO "y" INTO out END"#).unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].kind, Step);
    }

    // ==================== Срез 5: constrained decoding ====================

    /// ROLE и STEP — вырожденная (одна строка) и обычная (закрытый набор)
    /// грамматики; singleton_output обязана вернуть Some только для ROLE.
    #[test]
    fn singleton_output_only_for_degenerate_grammar() {
        let role = parse(r#"NODE r: ROLE [GENERATIVE] AS "x" END EDGES: END"#).unwrap();
        assert_eq!(
            decoding::singleton_output(Role, &role.nodes["r"].slots),
            Some(String::new())
        );

        let filter =
            parse(r#"NODE f: FILTER [GENERATIVE] WHERE a = "b" INTO out END EDGES: END"#).unwrap();
        assert_eq!(
            decoding::singleton_output(Filter, &filter.nodes["f"].slots),
            None
        );
    }

    /// validate_output: закрытый набор альтернатив (FILTER) и обязательный
    /// префикс (STEP) — зеркалит val_cases из test_invariants.py.
    #[test]
    fn validate_output_forms() {
        let f =
            parse(r#"NODE f: FILTER [GENERATIVE] WHERE a = "b" INTO out END EDGES: END"#).unwrap();
        let slots = &f.nodes["f"].slots;
        assert!(decoding::validate_output(Filter, slots, "MATCH").is_ok());
        assert!(decoding::validate_output(Filter, slots, "maybe").is_err());

        let s = parse(r#"NODE s: STEP [GENERATIVE] DO "x" INTO out END EDGES: END"#).unwrap();
        let slots = &s.nodes["s"].slots;
        assert!(decoding::validate_output(Step, slots, "RESULT:done").is_ok());
        assert!(decoding::validate_output(Step, slots, "без префикса").is_err());
        assert!(decoding::validate_output(Step, slots, "RESULT:").is_err());

        let r = parse(r#"NODE r: ROLE [GENERATIVE] AS "x" END EDGES: END"#).unwrap();
        assert!(decoding::validate_output(Role, &r.nodes["r"].slots, "").is_ok());
        assert!(decoding::validate_output(Role, &r.nodes["r"].slots, "лишний текст").is_err());

        let sp = parse(r#"NODE p: SPAWN [GENERATIVE] PLAN "x" INTO out END EDGES: END"#).unwrap();
        let slots = &sp.nodes["p"].slots;
        assert!(decoding::validate_output(Spawn, slots, "Конечно! Вот шаги:").is_err());
        assert!(
            decoding::validate_output(Spawn, slots, r#"NODE n: ROLE [GENERATIVE] AS "a" END"#)
                .is_ok()
        );
    }

    /// CLASSIFY: грамматика зависит от содержимого узла (метки в LABELS) —
    /// расхождение 2.3 из MARKER_V0_RUST.md, закрытое реестром в Python,
    /// проверяется и в Rust-ветке той же логикой.
    #[test]
    fn classify_grammar_depends_on_node_content() {
        let c = parse(
            r#"NODE c: CLASSIFY [GENERATIVE] INPUT "x" LABELS a="urgent", b="normal" INTO out END EDGES: END"#,
        )
        .unwrap();
        let slots = &c.nodes["c"].slots;
        assert!(decoding::validate_output(Classify, slots, "urgent").is_ok());
        assert!(decoding::validate_output(Classify, slots, "normal").is_ok());
        assert!(decoding::validate_output(Classify, slots, "other").is_err());
    }

    /// node_surface_form -> parse_fragment сохраняет cache_key, для ВСЕХ
    /// типов реестра (не только двух) — то же, что инвариант 9 в Python,
    /// найденный эмпирикой (docs/EMPIRICAL_REAL_MODEL.md раздел 11): промпт
    /// обязан быть настоящим Luck, не внутренней pipe-формой.
    #[test]
    fn surface_form_round_trips_for_all_kinds() {
        let src = r#"
        NODE role: ROLE [GENERATIVE] AS "incident triage engineer" END
        NODE task1: TASK [GENERATIVE] GIVEN "raw" PRODUCE "summary" END
        NODE filter1: FILTER [GENERATIVE] WHERE level = "critical" INTO branch END
        NODE step1: STEP [GENERATIVE] DO "ack" INTO out END
        NODE classify1: CLASSIFY [GENERATIVE] INPUT "x" LABELS a="critical", b="warning" INTO level END
        NODE spawn1: SPAWN [GENERATIVE] PLAN "decompose" INTO subgraph END
        NODE tool1: TOOL [EXTERNAL] CALL notify WITH target="ops", level=2 END
        NODE doc1: DOCUMENT [EXTERNAL] FROM "spec.md" END
        NODE reject1: REJECT_MARK [REJECT] CAUSE plan REASON SYNTAX END
        EDGES: END
        "#;
        let g = parse(src).unwrap();
        assert_eq!(
            g.nodes.len(),
            Kind::ALL.len(),
            "тест покрывает все типы реестра"
        );

        for (id, node) in &g.nodes {
            let surface =
                decoding::node_surface_form(node.kind, node.subtype, &node.node_id, &node.slots);
            assert_ne!(
                surface,
                node.canonical_form(),
                "surface != внутренняя pipe-форма, узел {id}"
            );
            let reparsed = &parse_fragment(&surface).unwrap()[0];
            assert_eq!(
                reparsed.cache_key, node.cache_key,
                "round-trip cache_key, узел {id}"
            );
        }
    }

    /// example_node_source синтезирован из реестра — валиден для КАЖДОГО
    /// типа, не только тех, что участвуют в SPAWN few-shot.
    #[test]
    fn example_node_source_valid_for_all_kinds() {
        for k in Kind::ALL {
            let src = decoding::example_node_source(k, "example");
            let nodes = parse_fragment(&src).unwrap_or_else(|e| panic!("{k:?}: {e} \n{src}"));
            assert_eq!(nodes.len(), 1);
            assert_eq!(nodes[0].kind, k);
        }
    }

    // ==================== Срез 6: планировщик обхода ====================

    use scheduler::{InvalidModelOutput, Scheduler};

    const CHAIN_SRC: &str = r#"
    NODE role: ROLE [GENERATIVE] AS "analyst" END
    NODE work: STEP [GENERATIVE] DO "analyze" INTO result END
    EDGES:
      role -> work
    END
    "#;

    /// Ядро п.3 (Python-инвариант 1, уровень Scheduler): cache_key не
    /// включает branch_id — два прогона того же графа разными акторами
    /// делят кэш. Один и тот же Scheduler (общий cache) прогоняется дважды.
    #[test]
    fn two_branches_share_cache() {
        let mut backend = MockBackend::new();
        let mut tools = ToolRegistry::new();
        let mut sched = Scheduler::new(&mut backend, &mut tools);

        let mut g1 = parse(CHAIN_SRC).unwrap();
        let r1 = sched.run(&mut g1, "alice").unwrap();
        let calls_1 = r1.model_calls;

        let mut g2 = parse(CHAIN_SRC).unwrap();
        let r2 = sched.run(&mut g2, "bob").unwrap();
        let calls_2 = r2.model_calls;

        assert_eq!(
            calls_1, calls_2,
            "вторая ветка не должна добавлять вызовов модели"
        );
        assert_eq!(r1.outputs, r2.outputs, "выводы веток идентичны");
    }

    /// Активация BRANCH-рёбер по факту вывода источника: мёртвая ветка
    /// пропускается (skipped), а не отклоняется (reject) — расхождение
    /// 2.2 из MARKER_V0_RUST.md.
    const BRANCH_SRC: &str = r#"
    NODE gate: FILTER [GENERATIVE] WHERE a = "b" INTO out END
    NODE yes: STEP [GENERATIVE] DO "match path" INTO r1 END
    NODE no: STEP [GENERATIVE] DO "no-match path" INTO r2 END
    EDGES:
      gate => yes [on_match]
      gate => no [on_no_match]
    END
    "#;

    #[test]
    fn dead_branch_is_skipped_not_rejected() {
        let mut backend = MockBackend::new();
        let mut tools = ToolRegistry::new();
        let mut sched = Scheduler::new(&mut backend, &mut tools);
        let mut g = parse(BRANCH_SRC).unwrap();
        let r = sched.run(&mut g, "root").unwrap();

        assert!(r.rejects.is_empty(), "мёртвая ветка — не отказ");
        let gate_output = r.outputs["gate"].clone();
        let executed_branch = if gate_output == "MATCH" { "yes" } else { "no" };
        let dead_branch = if gate_output == "MATCH" { "no" } else { "yes" };
        assert!(r.outputs.contains_key(executed_branch));
        assert!(r.skipped.contains(&dead_branch.to_string()));
    }

    /// Ребро 6->1: SPAWN разбирает свой вывод как узлы Luck, вставляет их
    /// в граф с branch_id актора; они исполняются в том же прогоне.
    const SPAWN_SRC: &str = r#"
    NODE plan: SPAWN [GENERATIVE] PLAN "decompose" INTO subgraph END
    EDGES: END
    "#;

    #[test]
    fn spawn_grows_graph_and_children_execute() {
        let mut backend = MockBackend::new();
        let mut tools = ToolRegistry::new();
        tools.register("notify", |_args| "notified".to_string());
        let mut sched = Scheduler::new(&mut backend, &mut tools);
        let mut g = parse(SPAWN_SRC).unwrap();
        let r = sched.run(&mut g, "alice").unwrap();

        assert!(r.rejects.is_empty());
        assert_eq!(r.spawned.len(), 2, "MockBackend SPAWN выдаёт ровно 2 узла");
        for spawned_id in &r.spawned {
            assert!(
                r.outputs.contains_key(spawned_id),
                "порождённый узел исполнен: {spawned_id}"
            );
            assert_eq!(
                g.nodes[spawned_id].branch_id, "alice",
                "branch_id унаследован от актора"
            );
        }
    }

    /// Бюджет роста: max_depth=0 запрещает ЛЮБОЕ порождение — вместо
    /// детей SPAWN получает reject(BUDGET). Ядро: "что не помещается в
    /// граф как узел, не является частью Luck" — бюджет тоже узел.
    #[test]
    fn spawn_over_depth_budget_yields_reject() {
        let mut backend = MockBackend::new();
        let mut tools = ToolRegistry::new();
        let mut sched = Scheduler::new(&mut backend, &mut tools).with_limits(0, 64);
        let mut g = parse(SPAWN_SRC).unwrap();
        let r = sched.run(&mut g, "root").unwrap();

        assert!(r.spawned.is_empty());
        assert_eq!(r.rejects.len(), 1);
        assert_eq!(
            r.rejects[0].slots.get("reason"),
            Some(&SlotData::Ident("BUDGET".to_string()))
        );
    }

    /// КОНТРАКТ РЕБРА 5->6: невалидный вывод модели останавливает обход,
    /// представлен reject(SYNTAX)-узлом графа, не паникой/исключением.
    struct AlwaysInvalidBackend;
    impl ModelBackend for AlwaysInvalidBackend {
        fn generate(
            &mut self,
            _prompt: &str,
            _c: &Constraint,
            _k: Kind,
            _s: &BTreeMap<&'static str, SlotData>,
        ) -> Result<String, InvalidModelOutput> {
            Err(InvalidModelOutput::new("мусор"))
        }
    }

    #[test]
    fn invalid_model_output_becomes_reject_syntax_not_panic() {
        let mut backend = AlwaysInvalidBackend;
        let mut tools = ToolRegistry::new();
        let mut sched = Scheduler::new(&mut backend, &mut tools);
        let mut g = parse(CHAIN_SRC).unwrap();
        let r = sched.run(&mut g, "root").unwrap();

        assert_eq!(r.rejects.len(), 1);
        assert_eq!(
            r.rejects[0].slots.get("reason"),
            Some(&SlotData::Ident("SYNTAX".to_string()))
        );
        assert!(
            !r.outputs.contains_key("work"),
            "потомок не исполняется после отказа"
        );
    }

    /// Найдено на практике (docs — реальный прогон с плейсхолдер-ключом
    /// дал 401, до этой правки неотличимо свёрнутый в reject(SYNTAX) как
    /// настоящий провал грамматики). fatal=true у InvalidModelOutput
    /// обязан остановить ВЕСЬ обход (Err из run()), не поглощаться как
    /// reject одного узла — иначе транспортный сбой маскируется под
    /// содержательный результат эксперимента.
    struct AlwaysFatalBackend;
    impl ModelBackend for AlwaysFatalBackend {
        fn generate(
            &mut self,
            _prompt: &str,
            _c: &Constraint,
            _k: Kind,
            _s: &BTreeMap<&'static str, SlotData>,
        ) -> Result<String, InvalidModelOutput> {
            Err(InvalidModelOutput::fatal("HTTP 401: invalid x-api-key"))
        }
    }

    #[test]
    fn fatal_backend_error_aborts_whole_run_not_a_single_reject() {
        let mut backend = AlwaysFatalBackend;
        let mut tools = ToolRegistry::new();
        let mut sched = Scheduler::new(&mut backend, &mut tools);
        let mut g = parse(CHAIN_SRC).unwrap();

        let err = sched
            .run(&mut g, "root")
            .expect_err("fatal обязан прервать run() целиком");
        assert!(
            err.contains("401"),
            "причина фатального сбоя видна в сообщении: {err}"
        );
    }

    /// Неизвестный инструмент -> reject(TYPE), не паника.
    #[test]
    fn unknown_tool_yields_reject_type() {
        let mut backend = MockBackend::new();
        let mut tools = ToolRegistry::new(); // "save" не зарегистрирован
        let mut sched = Scheduler::new(&mut backend, &mut tools);
        let mut g =
            parse(r#"NODE t: TOOL [EXTERNAL] CALL save WITH path="x" END EDGES: END"#).unwrap();
        let r = sched.run(&mut g, "root").unwrap();

        assert_eq!(r.rejects.len(), 1);
        assert_eq!(
            r.rejects[0].slots.get("reason"),
            Some(&SlotData::Ident("TYPE".to_string()))
        );
    }

    // ==================== Срез 4: презентация ====================

    /// Контракт Среза 3 <-> Среза 4: canonical -> presentation -> canonical
    /// сохраняет и слоты, и cache_key — для ВСЕХ типов реестра, не для
    /// одного примера. Биекция структурна (обход одного spec.slots), но
    /// проверяется исполнимо, не только доказательством на бумаге.
    #[test]
    fn presentation_round_trip_preserves_cache_key_all_kinds() {
        let src = r#"
        NODE role: ROLE [GENERATIVE] AS "senior architect" END
        NODE task1: TASK [GENERATIVE] GIVEN "raw" PRODUCE "summary" END
        NODE filter1: FILTER [GENERATIVE] WHERE status = "draft" INTO n3 END
        NODE step1: STEP [GENERATIVE] DO "ack" INTO out END
        NODE classify1: CLASSIFY [GENERATIVE] INPUT "x" LABELS a="urgent", b="normal" INTO level END
        NODE spawn1: SPAWN [GENERATIVE] PLAN "decompose" INTO subgraph END
        NODE tool1: TOOL [EXTERNAL] CALL search WITH query="Luck grammar", limit=5 END
        NODE doc1: DOCUMENT [EXTERNAL] FROM "spec.md" END
        NODE reject1: REJECT_MARK [REJECT] CAUSE plan REASON SYNTAX END
        EDGES: END
        "#;
        let g = parse(src).unwrap();
        assert_eq!(g.nodes.len(), Kind::ALL.len());

        for (id, node) in &g.nodes {
            let rt = presentation::node_round_trip(node).unwrap_or_else(|e| panic!("{id}: {e}"));
            assert_eq!(
                rt.cache_key, node.cache_key,
                "cache_key сохранён, узел {id}"
            );
            assert_eq!(rt.slots, node.slots, "слоты сохранены, узел {id}");
        }
    }

    /// Стиль (4.4) в биекции не участвует: compact и expanded одного и
    /// того же графа разбираются в структурно идентичный граф.
    #[test]
    fn style_does_not_affect_parse() {
        use presentation::Style;
        let g = parse(SAMPLE).unwrap();
        let expanded = presentation::to_presentation(&g, Style::Expanded);
        let compact = presentation::to_presentation(&g, Style::Compact);
        assert_ne!(expanded, compact, "тексты разные (иначе тест бессмыслен)");

        let g1 = presentation::from_presentation(&expanded, "root").unwrap();
        let g2 = presentation::from_presentation(&compact, "root").unwrap();
        for id in g1.nodes.keys() {
            assert_eq!(g1.nodes[id].cache_key, g2.nodes[id].cache_key, "узел {id}");
        }
        assert_eq!(g1.edges.len(), g2.edges.len());
    }

    /// Полный граф (не один узел) через презентацию и обратно — включая
    /// рёбра с меткой (BRANCH). Мимо этого теста прошёл бы баг, видимый
    /// только на графах с более чем одним узлом (edges не проверяются
    /// node_round_trip).
    #[test]
    fn full_graph_round_trip_via_presentation() {
        use presentation::Style;
        let g = parse(SAMPLE).unwrap();
        let text = presentation::to_presentation(&g, Style::Expanded);
        let g2 = presentation::from_presentation(&text, "root").unwrap();

        assert_eq!(g.nodes.len(), g2.nodes.len());
        assert_eq!(g.edges.len(), g2.edges.len());
        for id in g.nodes.keys() {
            assert_eq!(g.nodes[id].cache_key, g2.nodes[id].cache_key, "узел {id}");
        }
        let branch_edge = g2
            .edges
            .iter()
            .find(|e| e.edge_type == EdgeType::Branch)
            .unwrap();
        assert_eq!(branch_edge.label.as_deref(), Some("on_match"));
    }

    /// Презентация текста, не порождённого этим генератором (неизвестный
    /// тип узла, отсутствующий обязательный слот) — PresentationError, не
    /// паника. Соответствует reject(SYNTAX) на ребре 4->1 в архитектуре:
    /// невосстановимая правка человека в презентационном синтаксисе.
    #[test]
    fn malformed_presentation_is_error_not_panic() {
        assert!(
            presentation::from_presentation(
                "<luck><nodes><ghost id=\"x\"/></nodes><edges/></luck>",
                "root"
            )
            .is_err()
        );
        assert!(
            presentation::from_presentation(
                "<luck><nodes><role id=\"x\"/></nodes><edges/></luck>",
                "root"
            )
            .is_err()
        );
    }

    // ==================== §4.1: PrefixTrie ====================

    /// Синтетический, но реалистичный для Luck-графов паттерн: один
    /// узел-предок (роль), от которого расходится N независимых детей —
    /// типичная форма "общий контекст, разные шаги" (см. пайплайн
    /// инцидент-реагирования в docs/EMPIRICAL_REAL_MODEL.md — там `role`
    /// был общим предком для всей цепочки). Каждый ребёнок получает
    /// промпт "surface_form(role) + \n + surface_form(себя)" — общий
    /// префикс должен быть измерим.
    #[test]
    fn wide_fanout_shares_prefix() {
        let mut src =
            String::from(r#"NODE role: ROLE [GENERATIVE] AS "incident triage engineer" END"#);
        const N: usize = 12;
        for i in 0..N {
            src.push_str(&format!(
                "
NODE step{i}: STEP [GENERATIVE] DO \"handle case {i}\" INTO out{i} END"
            ));
        }
        src.push_str("\nEDGES:");
        for i in 0..N {
            src.push_str(&format!("\n  role -> step{i}"));
        }
        src.push_str("\nEND");

        let mut backend = MockBackend::new();
        let mut tools = ToolRegistry::new();
        let mut sched = Scheduler::new(&mut backend, &mut tools).with_prefix_tracking();
        let mut g = parse(&src).unwrap();
        let r = sched.run(&mut g, "root").unwrap();
        assert!(r.rejects.is_empty());

        let stats = sched.prefix_stats().unwrap();
        // Измерено (docs/RUST_STEP_RESULTS.md §4.1): при N=12 экономия
        // ~49%, при N=100 — ~50%, сходится к 1/2 (одна общая строка —
        // AS-слот role — из двух строк типичного промпта ребёнка).
        assert!(
            stats.savings_ratio() > 0.0,
            "общий предок обязан дать измеримую экономию"
        );
        // N детей делят один и тот же первый узел префиксного дерева
        // (surface_form(role)) — минимум N-1 переиспользований из N+1
        // вставок (role исполняется один раз без предков, N детей — с ним).
        assert!(
            stats.shared_lines >= N - 1,
            "shared_lines={}, ожидалось >= {}",
            stats.shared_lines,
            N - 1
        );
    }

    /// Контрастный случай: узлы БЕЗ общего предка (независимые корни) —
    /// экономия обязана быть нулевой или близкой к нулю. Подтверждает,
    /// что PrefixTrie измеряет именно структуру графа, а не артефакт
    /// реализации (совпадение по случайности).
    #[test]
    fn independent_roots_share_nothing() {
        let mut src = String::new();
        const N: usize = 8;
        for i in 0..N {
            src.push_str(&format!(
                "NODE r{i}: ROLE [GENERATIVE] AS \"distinct role {i}\" END\n"
            ));
        }
        src.push_str("EDGES:\nEND");

        let mut backend = MockBackend::new();
        let mut tools = ToolRegistry::new();
        let mut sched = Scheduler::new(&mut backend, &mut tools).with_prefix_tracking();
        let mut g = parse(&src).unwrap();
        let r = sched.run(&mut g, "root").unwrap();
        assert!(r.rejects.is_empty());

        let stats = sched.prefix_stats().unwrap();
        assert_eq!(stats.shared_lines, 0, "независимые корни не делят префикс");
        assert_eq!(stats.savings_ratio(), 0.0);
    }

    // ==================== §4.4: стоимость canonical_form()/cache_key ====================

    /// Измеряет реальную стоимость `Node::new` (canonical_form() +
    /// sha256) на представительном объёме узлов — не гадание, факт.
    /// Таймингово ничего не ASSERT'ится (машины разные, флейки в CI) —
    /// печатается диагностика (`--nocapture`), закреплённые в
    /// docs/RUST_STEP_RESULTS.md числа сняты этим же тестом.
    /// Корректность (не тайминг) проверяется: N узлов с разным `given`
    /// -> N разных cache_key, ни одной коллизии.
    #[test]
    fn canonical_form_cost_is_measured_not_guessed() {
        use std::collections::BTreeSet;
        use std::time::Instant;

        const N: usize = 50_000;
        let mut keys = BTreeSet::new();
        let start = Instant::now();
        for i in 0..N {
            let mut slots = BTreeMap::new();
            slots.insert(
                "given",
                SlotData::Str(format!("raw incident payload number {i}")),
            );
            slots.insert("produce", SlotData::Str("structured summary".to_string()));
            let node = Node::new(format!("n{i}"), Subtype::Generative, Task, slots);
            keys.insert(node.cache_key);
        }
        let elapsed = start.elapsed();

        assert_eq!(
            keys.len(),
            N,
            "N узлов с разным содержимым -> N разных cache_key, коллизий нет"
        );

        let per_node_ns = elapsed.as_nanos() as f64 / N as f64;
        eprintln!(
            "MEASURED canonical_form()+sha256: N={N} total={elapsed:?} per_node={per_node_ns:.0}ns"
        );
        // Ориентир для чтения диагностики (не assert): по
        // docs/EMPIRICAL_REAL_MODEL.md реальный вызов модели — от сотен
        // мс до нескольких секунд. Если per_node_ns на 6+ порядков
        // меньше миллисекунды (микросекунды или меньше), вычисление
        // cache_key не может быть узким местом рядом с сетевым вызовом
        // ни при каком правдоподобном размере графа.
    }

    // ==================== §4.2: конкурентное порождение ====================

    use std::sync::Mutex;

    /// Настоящая многопоточность (не симуляция): N потоков независимо
    /// парсят ОДИНАКОВЫЙ SPAWN-вывод (значит, одинаковый node_id — см.
    /// докстринг concurrent_spawn.rs) и одновременно пытаются вставить
    /// его в ОДИН граф под разными branch_id. Крупная блокировка
    /// (Mutex на весь граф, критическая секция на весь батч вставки)
    /// обязана дать: ровно один узел в графе (не паника, не дубликат,
    /// не повреждённое состояние), и ровно один поток из N реально
    /// вставил его (остальные нашли узел уже существующим).
    #[test]
    fn concurrent_identical_spawn_yields_single_node_no_corruption() {
        let graph = Mutex::new(IntentGraph::default());
        {
            let mut g = graph.lock().unwrap();
            g.add_node(Node::new(
                "source".to_string(),
                Subtype::Generative,
                Spawn,
                BTreeMap::from([
                    ("plan", SlotData::Str("decompose".to_string())),
                    ("into", SlotData::Ident("subgraph".to_string())),
                ]),
            ))
            .unwrap();
        }

        const ACTORS: usize = 16;
        let spawn_output = r#"NODE shared_child: STEP [GENERATIVE] DO "same content" INTO out END"#;

        let inserted_counts: Vec<usize> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..ACTORS)
                .map(|i| {
                    let graph = &graph;
                    scope.spawn(move || {
                        let nodes = parse_fragment(spawn_output).unwrap();
                        let branch_id = format!("actor_{i}");
                        concurrent_spawn::spawn_concurrent(graph, "source", nodes, &branch_id).len()
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });

        let g = graph.lock().unwrap();
        assert_eq!(
            g.nodes.len(),
            2,
            "source + ровно один shared_child, не {ACTORS}"
        );
        assert_eq!(
            inserted_counts.iter().sum::<usize>(),
            1,
            "ровно один из {ACTORS} акторов реально вставил узел — остальные нашли уже существующим"
        );
        let winner_branch = g.nodes["shared_child"].branch_id.clone();
        assert!(
            winner_branch.starts_with("actor_"),
            "branch_id победившего актора корректен, не повреждён гонкой: {winner_branch}"
        );
    }

    /// Контраст: разное содержимое -> разные node_id -> все N узлов
    /// сосуществуют, каждый под своим branch_id. Подтверждает, что в
    /// предыдущем тесте единственность — следствие ОДИНАКОВОГО
    /// содержимого (Ядро п.3: одинаковый узел = общее вычисление), а не
    /// артефакт протокола, теряющего узлы.
    #[test]
    fn concurrent_distinct_spawn_all_survive() {
        let graph = Mutex::new(IntentGraph::default());
        {
            let mut g = graph.lock().unwrap();
            g.add_node(Node::new(
                "source".to_string(),
                Subtype::Generative,
                Spawn,
                BTreeMap::from([
                    ("plan", SlotData::Str("decompose".to_string())),
                    ("into", SlotData::Ident("subgraph".to_string())),
                ]),
            ))
            .unwrap();
        }

        const ACTORS: usize = 16;
        let inserted_counts: Vec<usize> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..ACTORS)
                .map(|i| {
                    let graph = &graph;
                    scope.spawn(move || {
                        let src = format!(
                            "NODE child_{i}: STEP [GENERATIVE] DO \"distinct content {i}\" INTO out{i} END"
                        );
                        let nodes = parse_fragment(&src).unwrap();
                        let branch_id = format!("actor_{i}");
                        concurrent_spawn::spawn_concurrent(graph, "source", nodes, &branch_id).len()
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });

        let g = graph.lock().unwrap();
        assert_eq!(
            g.nodes.len(),
            1 + ACTORS,
            "source + все {ACTORS} различных детей"
        );
        assert_eq!(
            inserted_counts.iter().sum::<usize>(),
            ACTORS,
            "каждый актор вставил ровно свой узел"
        );
        for i in 0..ACTORS {
            assert_eq!(
                g.nodes[&format!("child_{i}")].branch_id,
                format!("actor_{i}")
            );
        }
    }
}
