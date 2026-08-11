//! Luck — микро-стандартная библиотека типов узлов
//! Ветка: Rust-проход (Маркер В.0 -> возврат)
//!
//! Зеркалит luck/registry.py по назначению: тип узла объявляется ОДИН
//! раз как NodeKindSpec, и из этой единственной декларации выводятся
//! продукция EBNF, правило разбора тела, совместимость kind/subtype,
//! порядок слотов cache_key, грамматическое ограничение decoding.
//!
//! ОТЛИЧИЕ ОТ PYTHON-ВЕТКИ (фиксирую явно, не молчаливо, MARKER_V0_RUST
//! п.5): kind/subtype — не строки, а `Kind`/`Subtype`. Совместимость
//! kind<->subtype, которая в Python — таблица, выведенная из REGISTRY и
//! ПРОВЕРЯЕМАЯ во время исполнения (validate_kind_subtype), в Rust
//! наполовину доказана компилятором: `NodeKindSpec.subtype` — единственное
//! поле типа, разрешённые слоты и грамматика жёстко привязаны к Kind
//! через REGISTRY. Ошибка вида "kind объявлен с чужим subtype" в исходном
//! тексте Luck (не в Rust-коде) остаётся runtime-проверкой — она про
//! текст программы, который компилятор не читает.

use std::collections::BTreeMap;
use std::sync::LazyLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    Role,
    Task,
    Filter,
    Step,
    Classify,
    Spawn,
    Tool,
    Document,
    RejectMark,
}

impl Kind {
    pub const ALL: [Kind; 9] = [
        Kind::Role,
        Kind::Task,
        Kind::Filter,
        Kind::Step,
        Kind::Classify,
        Kind::Spawn,
        Kind::Tool,
        Kind::Document,
        Kind::RejectMark,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Role => "ROLE",
            Kind::Task => "TASK",
            Kind::Filter => "FILTER",
            Kind::Step => "STEP",
            Kind::Classify => "CLASSIFY",
            Kind::Spawn => "SPAWN",
            Kind::Tool => "TOOL",
            Kind::Document => "DOCUMENT",
            Kind::RejectMark => "REJECT_MARK",
        }
    }

    pub fn parse(s: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Subtype {
    Generative,
    External,
    Reject,
}

impl Subtype {
    pub fn as_str(self) -> &'static str {
        match self {
            Subtype::Generative => "GENERATIVE",
            Subtype::External => "EXTERNAL",
            Subtype::Reject => "REJECT",
        }
    }

    pub fn parse(s: &str) -> Option<Subtype> {
        match s {
            "GENERATIVE" => Some(Subtype::Generative),
            "EXTERNAL" => Some(Subtype::External),
            "REJECT" => Some(Subtype::Reject),
            _ => None,
        }
    }
}

/// Тип значения слота — value в Python-версии.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotValue {
    String,
    Ident,
    Condition,
    Args,
    /// Enum(options) — закрытый набор значений.
    Enum(&'static [&'static str]),
}

#[derive(Debug, Clone, Copy)]
pub struct Slot {
    pub keyword: &'static str,
    pub name: &'static str,
    pub value: SlotValue,
}

/// Грамматика вывода generative-узла. `__NODE_GRAMMAR__` и `__DYNAMIC__`
/// из Python — здесь варианты enum, не строки-маркеры: невозможно
/// случайно опечатать маркер, компилятор проверяет исчерпанность match.
#[derive(Debug, Clone, Copy)]
pub enum OutputEbnf {
    Literal(&'static str),
    /// SPAWN: грамматика УЗЛА LUCK, собранная из самого реестра.
    NodeGrammar,
    /// CLASSIFY: грамматика выводится из меток самого узла (слот `labels`).
    Dynamic,
}

#[derive(Debug, Clone, Copy)]
pub struct NodeKindSpec {
    pub kind: Kind,
    pub subtype: Subtype,
    pub slots: &'static [Slot],
    pub doc: &'static str,
    pub output_ebnf: Option<OutputEbnf>,
    pub stop_tokens: &'static [&'static str],
    pub normalized_slots: &'static [&'static str],
}

impl NodeKindSpec {
    /// Ядро п.4: decoding применим только к generative-подтипу.
    pub fn has_decoding(&self) -> bool {
        self.subtype == Subtype::Generative && self.output_ebnf.is_some()
    }

    fn value_symbol(slot: &Slot) -> String {
        match slot.value {
            SlotValue::String => "string_literal".to_string(),
            SlotValue::Ident => "identifier".to_string(),
            SlotValue::Condition => "condition".to_string(),
            SlotValue::Args => "arg_list".to_string(),
            SlotValue::Enum(opts) => {
                let alts: Vec<String> = opts.iter().map(|o| format!("\"{o}\"")).collect();
                format!("( {} )", alts.join(" | "))
            }
        }
    }

    /// Продукция EBNF для тела узла — Срез 1.
    pub fn ebnf_production(&self) -> String {
        let parts: Vec<String> = self
            .slots
            .iter()
            .map(|s| format!("\"{}\" , {}", s.keyword, Self::value_symbol(s)))
            .collect();
        format!(
            "{}_body = {} ;",
            self.kind.as_str().to_lowercase(),
            parts.join(" , ")
        )
    }
}

macro_rules! slots {
    ($(($kw:expr, $name:expr, $val:expr)),* $(,)?) => {
        &[$(Slot { keyword: $kw, name: $name, value: $val }),*]
    };
}

pub static REGISTRY: LazyLock<BTreeMap<Kind, NodeKindSpec>> = LazyLock::new(|| {
    let mut m = BTreeMap::new();
    let mut put = |spec: NodeKindSpec| {
        m.insert(spec.kind, spec);
    };

    // ---------- GENERATIVE ----------
    put(NodeKindSpec {
        kind: Kind::Role,
        subtype: Subtype::Generative,
        slots: slots![("AS", "as", SlotValue::String)],
        doc: "Задаёт роль/позицию, из которой ведётся рассуждение. Сам вывода не порождает.",
        output_ebnf: Some(OutputEbnf::Literal("output = \"\" ;")),
        stop_tokens: &["\n"],
        normalized_slots: &[],
    });

    put(NodeKindSpec {
        kind: Kind::Task,
        subtype: Subtype::Generative,
        slots: slots![
            ("GIVEN", "given", SlotValue::String),
            ("PRODUCE", "produce", SlotValue::String),
        ],
        doc: "Задача: из данного входа произвести описанный результат.",
        output_ebnf: Some(OutputEbnf::Literal(
            "output = text ; text = char , { char } ;",
        )),
        stop_tokens: &["\n\n"],
        normalized_slots: &[],
    });

    put(NodeKindSpec {
        kind: Kind::Filter,
        subtype: Subtype::Generative,
        slots: slots![
            ("WHERE", "where", SlotValue::Condition),
            ("INTO", "into", SlotValue::Ident),
        ],
        doc: "Проверка условия. Вывод строго бинарен — основа ветвления через '=>'.",
        output_ebnf: Some(OutputEbnf::Literal("output = \"MATCH\" | \"NO_MATCH\" ;")),
        stop_tokens: &["\n"],
        normalized_slots: &[],
    });

    put(NodeKindSpec {
        kind: Kind::Step,
        subtype: Subtype::Generative,
        slots: slots![
            ("DO", "do", SlotValue::String),
            ("INTO", "into", SlotValue::Ident),
        ],
        doc: "Шаг пайплайна с именованным результатом.",
        output_ebnf: Some(OutputEbnf::Literal(
            "output = \"RESULT\" , \":\" , text ; text = char , { char } ;",
        )),
        stop_tokens: &["\n\n"],
        normalized_slots: &[],
    });

    put(NodeKindSpec {
        kind: Kind::Classify,
        subtype: Subtype::Generative,
        slots: slots![
            ("INPUT", "input", SlotValue::String),
            ("LABELS", "labels", SlotValue::Args),
            ("INTO", "into", SlotValue::Ident),
        ],
        doc: "Классификация в закрытый набор меток. Ограничение decoding строится \
              из самих меток — редкий случай, когда grammar зависит от содержимого узла.",
        output_ebnf: Some(OutputEbnf::Dynamic),
        stop_tokens: &["\n"],
        normalized_slots: &["labels"],
    });

    // Грамматика вывода SPAWN — это грамматика УЗЛА LUCK. Именно поэтому
    // каноничность порождённого узла гарантируется самим constrained
    // decoding, а не проверкой постфактум.
    put(NodeKindSpec {
        kind: Kind::Spawn,
        subtype: Subtype::Generative,
        slots: slots![
            ("PLAN", "plan", SlotValue::String),
            ("INTO", "into", SlotValue::Ident),
        ],
        doc: "Порождающий узел: его вывод разбирается планировщиком как объявления \
              узлов Luck и вставляется в граф. Замыкает цикл 6->1.",
        output_ebnf: Some(OutputEbnf::NodeGrammar),
        stop_tokens: &["\n\n\n"],
        normalized_slots: &[],
    });

    // ---------- EXTERNAL ----------
    put(NodeKindSpec {
        kind: Kind::Tool,
        subtype: Subtype::External,
        slots: slots![
            ("CALL", "tool", SlotValue::Ident),
            ("WITH", "args", SlotValue::Args),
        ],
        doc: "Вызов внешнего инструмента. Аргументы нормализуются сортировкой \
              по имени — иначе нарушается префикс-стабильность.",
        output_ebnf: None,
        stop_tokens: &[],
        normalized_slots: &["args"],
    });

    put(NodeKindSpec {
        kind: Kind::Document,
        subtype: Subtype::External,
        slots: slots![("FROM", "from", SlotValue::String)],
        doc: "Загрузка внешнего документа как данных графа.",
        output_ebnf: None,
        stop_tokens: &[],
        normalized_slots: &[],
    });

    // ---------- REJECT ----------
    put(NodeKindSpec {
        kind: Kind::RejectMark,
        subtype: Subtype::Reject,
        slots: slots![
            ("CAUSE", "cause", SlotValue::Ident),
            (
                "REASON",
                "reason",
                SlotValue::Enum(&["SYNTAX", "TYPE", "BUDGET"])
            ),
        ],
        doc: "Отказ, представленный как полноценный узел, а не исключение вне \
              графа. SYNTAX/TYPE — отказы валидации; BUDGET — исчерпание бюджета \
              роста при динамическом порождении (ребро 6->1).",
        output_ebnf: None,
        stop_tokens: &[],
        normalized_slots: &[],
    });

    m
});

pub fn spec(kind: Kind) -> &'static NodeKindSpec {
    REGISTRY
        .get(&kind)
        .expect("REGISTRY заполнен для всех Kind::ALL")
}

/// Таблица совместимости kind/subtype — Срез 2. Выводится, не задаётся.
pub fn subtype_kinds() -> BTreeMap<Subtype, Vec<Kind>> {
    let mut table: BTreeMap<Subtype, Vec<Kind>> = BTreeMap::new();
    for spec in REGISTRY.values() {
        table.entry(spec.subtype).or_default().push(spec.kind);
    }
    for kinds in table.values_mut() {
        kinds.sort();
    }
    table
}

/// Все ключевые слова слотов — для лексера. Выводятся из реестра.
pub fn keywords() -> std::collections::BTreeSet<&'static str> {
    let mut result = std::collections::BTreeSet::new();
    for spec in REGISTRY.values() {
        for slot in spec.slots {
            result.insert(slot.keyword);
            if let SlotValue::Enum(opts) = slot.value {
                result.extend(opts.iter().copied());
            }
        }
    }
    result
}

/// Порядок слотов канонической формы — основа cache_key (Срез 3).
pub fn canonical_slot_order(kind: Kind) -> Vec<&'static str> {
    spec(kind).slots.iter().map(|s| s.name).collect()
}

/// Проверка совместимости kind/subtype в исходном тексте Luck (не в
/// Rust-типах — это runtime-факт о тексте программы, см. заголовок модуля).
pub fn validate_kind_subtype(kind: Kind, subtype: Subtype) -> Result<(), String> {
    let s = spec(kind);
    if s.subtype != subtype {
        return Err(format!(
            "kind '{}' объявлен с подтипом [{}], но зарегистрирован как [{}]",
            kind.as_str(),
            subtype.as_str(),
            s.subtype.as_str()
        ));
    }
    Ok(())
}
