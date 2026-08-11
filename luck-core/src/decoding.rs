//! Luck — Срез 5: constrained decoding (грамматики + постфактум-валидатор)
//! Ветка: Rust-проход (Маркер В.0 -> возврат)
//!
//! Зеркалит соответствующие функции luck/registry.py (build_constraint,
//! singleton_output, node_surface_form, example_node_source,
//! validate_output). Живёт в отдельном модуле, не в registry.rs, потому
//! что validate_output зависит от parser (разбор SPAWN-вывода как узлов
//! Luck) — тот же выбор, что в Python: registry.py импортирует
//! ast_parser.parse_fragment лениво внутри функции, а не на верхнем
//! уровне, чтобы избежать цикла registry<->parser. В Rust явный отдельный
//! модуль честнее скрытого ленивого импорта: зависимость видна в графе
//! модулей, а не спрятана внутри тела функции.
//!
//! Второй режим гарантии (backends.py в Python) — реальный вызов модели
//! через сеть — здесь НЕ реализован. Это осознанная граница Шага, не
//! пропуск: Rust-ветка отвечает на вопросы Маркера В.0 (раздел 4
//! MARKER_V0_RUST.md), ни один из них не требует повторного прогона на
//! реальной модели — та эмпирика уже собрана Python-веткой
//! (docs/EMPIRICAL_REAL_MODEL.md) и не специфична для языка рантайма.

use crate::parser::{SlotData, parse_fragment};
use crate::registry::{self, Kind, OutputEbnf, SlotValue, Subtype};
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub struct Constraint {
    pub definition: String,
    pub stop: Vec<&'static str>,
}

/// Строит ограничение decoding для узла. `None` для EXTERNAL/REJECT —
/// не отсутствие реализации, а прямое следствие Ядра п.4.
pub fn build_constraint(
    kind: Kind,
    slots: &BTreeMap<&'static str, SlotData>,
) -> Option<Constraint> {
    let spec = registry::spec(kind);
    if !spec.has_decoding() {
        return None;
    }
    let output_ebnf = spec.output_ebnf.as_ref()?;

    let definition = match output_ebnf {
        OutputEbnf::Literal(s) => s.to_string(),
        OutputEbnf::NodeGrammar => node_grammar_ebnf(),
        OutputEbnf::Dynamic => {
            // CLASSIFY: грамматика выводится из меток самого узла.
            let mut labels: Vec<(&str, String)> = match slots.get("labels") {
                Some(SlotData::Args(pairs)) => pairs
                    .iter()
                    .map(|(n, v)| (n.as_str(), strip_quotes(v)))
                    .collect(),
                _ => Vec::new(),
            };
            labels.sort_by(|a, b| a.0.cmp(b.0));
            let alts: Vec<String> = labels.iter().map(|(_, v)| format!("\"{v}\"")).collect();
            format!("output = {} ;", alts.join(" | "))
        }
    };

    Some(Constraint {
        definition,
        stop: spec.stop_tokens.to_vec(),
    })
}

fn strip_quotes(v: &str) -> String {
    v.trim_matches('"').to_string()
}

fn node_grammar_ebnf() -> String {
    let kinds: Vec<String> = Kind::ALL
        .iter()
        .map(|k| format!("\"{}\"", k.as_str()))
        .collect();
    let subtypes: Vec<&'static str> = vec![
        Subtype::External.as_str(),
        Subtype::Generative.as_str(),
        Subtype::Reject.as_str(),
    ];
    let mut subtypes = subtypes;
    subtypes.sort();
    let subtypes: Vec<String> = subtypes.iter().map(|s| format!("\"{s}\"")).collect();
    let mut sorted_kinds = Kind::ALL.to_vec();
    sorted_kinds.sort_by_key(|k| k.as_str());
    let bodies: Vec<String> = sorted_kinds
        .iter()
        .map(|k| format!("{}_body", k.as_str().to_lowercase()))
        .collect();

    format!(
        "output = {{ node_decl }} ;\n\
         node_decl = \"NODE\" , identifier , \":\" , kind , \"[\" , subtype , \"]\" , node_body , \"END\" ;\n\
         kind = {} ;\n\
         subtype = {} ;\n\
         node_body = {} ;",
        kinds.join(" | "),
        subtypes.join(" | "),
        bodies.join(" | ")
    )
}

/// Если язык грамматики узла состоит из РОВНО ОДНОЙ строки — возвращает
/// её; иначе None. Найдено эмпирикой (docs/EMPIRICAL_REAL_MODEL.md):
/// «выведи пустую строку» — единственная инструкция, которую chat-модели
/// трёх семейств стабильно не выполняли, описывая пустоту вместо
/// молчания. Их и не нужно было спрашивать: движок с нативной грамматикой
/// физически выдал бы эту единственную строку.
pub fn singleton_output(kind: Kind, slots: &BTreeMap<&'static str, SlotData>) -> Option<String> {
    let c = build_constraint(kind, slots)?;
    if c.definition == "output = \"\" ;" {
        return Some(String::new());
    }
    // "output = \"X\" ;" без " | " внутри — единственная альтернатива.
    let body = c
        .definition
        .strip_prefix("output = \"")?
        .strip_suffix("\" ;")?;
    if body.contains('|') || body.contains('"') {
        return None;
    }
    Some(body.to_string())
}

/// Узел -> поверхностный синтаксис Luck (не то же самое, что
/// Node::canonical_form — та внутренняя pipe-форма для cache_key).
/// Найдено эмпирикой: показывать модели внутреннее представление,
/// одновременно требуя от неё внешний синтаксис — рассинхрон промпта.
pub fn node_surface_form(
    kind: Kind,
    subtype: Subtype,
    node_id: &str,
    slots: &BTreeMap<&'static str, SlotData>,
) -> String {
    let spec = registry::spec(kind);
    let mut lines = vec![format!(
        "NODE {node_id}: {} [{}]",
        kind.as_str(),
        subtype.as_str()
    )];
    for slot in spec.slots {
        let value = slots.get(slot.name).expect("слот заполнен");
        let line = match value {
            SlotData::Str(v) => format!("  {} \"{v}\"", slot.keyword),
            SlotData::Ident(v) => format!("  {} {v}", slot.keyword),
            SlotData::Cond(f, o, v) => format!("  {} {f} {o} {v}", slot.keyword),
            SlotData::Args(pairs) => {
                let args: Vec<String> = pairs.iter().map(|(n, v)| format!("{n}={v}")).collect();
                format!("  {} {}", slot.keyword, args.join(", "))
            }
        };
        lines.push(line);
    }
    lines.push("END".to_string());
    lines.join("\n")
}

/// Синтезирует минимальный валидный исходник Luck для одного узла типа
/// kind — обходом spec.slots, инверсия parse_body. Нужен как few-shot
/// пример для SPAWN (эмпирика: словесного правила недостаточно).
pub fn example_node_source(kind: Kind, node_id: &str) -> String {
    let spec = registry::spec(kind);
    let mut lines = vec![format!(
        "NODE {node_id}: {} [{}]",
        kind.as_str(),
        spec.subtype.as_str()
    )];
    for slot in spec.slots {
        let placeholder = match slot.value {
            SlotValue::String => "\"example value\"".to_string(),
            SlotValue::Ident => "out".to_string(),
            SlotValue::Condition => "field = \"value\"".to_string(),
            SlotValue::Args => "name=\"value\"".to_string(),
            SlotValue::Enum(opts) => opts[0].to_string(),
        };
        lines.push(format!("  {} {placeholder}", slot.keyword));
    }
    lines.push("END".to_string());
    lines.join("\n")
}

/// Проверяет вывод модели на соответствие грамматике узла. `Ok(())`,
/// если валиден, иначе `Err(причина)`. Живёт здесь (не в транспортном
/// коде), потому что знание о допустимом выводе принадлежит реестру —
/// тому же источнику, из которого выведена сама грамматика.
pub fn validate_output(
    kind: Kind,
    slots: &BTreeMap<&'static str, SlotData>,
    text: &str,
) -> Result<(), String> {
    let Some(constraint) = build_constraint(kind, slots) else {
        return Ok(());
    };
    let value = text.trim();
    let ebnf = &constraint.definition;

    // Форма 1: пустой вывод
    if ebnf == "output = \"\" ;" {
        return if value.is_empty() {
            Ok(())
        } else {
            Err("ожидался пустой вывод".into())
        };
    }

    // Форма 2: грамматика узла Luck (SPAWN)
    if ebnf.starts_with("output = { node_decl }") {
        return match parse_fragment(text) {
            Err(e) => Err(format!("вывод не разбирается как узлы Luck: {e}")),
            Ok(nodes) if nodes.is_empty() => Err("вывод не содержит ни одного узла".into()),
            Ok(_) => Ok(()),
        };
    }

    // Форма 3: закрытый набор альтернатив ("A" | "B" | ...), без node_decl
    if let Some(rest) = ebnf.strip_prefix("output = ")
        && let Some(rest) = rest.strip_suffix(" ;")
        && rest.starts_with('"')
        && !rest.starts_with("\"RESULT\"")
    {
        let allowed: Vec<&str> = rest.split(" | ").map(|s| s.trim_matches('"')).collect();
        if !allowed.is_empty() {
            return if allowed.contains(&value) {
                Ok(())
            } else {
                Err(format!("ожидалось одно из {allowed:?}"))
            };
        }
    }

    // Форма 4: обязательный префикс (STEP: RESULT:...)
    if ebnf.starts_with("output = \"RESULT\"") {
        let head = "RESULT:";
        if !value.starts_with(head) {
            return Err(format!("ожидался префикс '{head}'"));
        }
        return if value[head.len()..].trim().is_empty() {
            Err("пустое тело после префикса".into())
        } else {
            Ok(())
        };
    }

    // Форма 5: свободный текст
    if value.is_empty() {
        Err("ожидался непустой текст".into())
    } else {
        Ok(())
    }
}
