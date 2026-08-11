//! Luck — парсер (строит граф намерения из потока токенов) + Срез 3
//! (идентичность/cache_key живут на Node, как и в Python-ветке).
//! Ветка: Rust-проход (Маркер В.0 -> возврат)
//!
//! Зеркалит luck/ast_parser.py по назначению и структуре (Node/Edge/
//! IntentGraph, canonical_form -> cache_key, parse/parse_fragment).
//!
//! ОТЛИЧИЕ ОТ PYTHON-ВЕТКИ (MARKER_V0_RUST п.5, фиксирую явно):
//! canonical_form() здесь НЕ обязана байт-в-байт совпадать со строкой,
//! которую строит Python — это независимая реализация, а не порт.
//! Единственный инвариант, который обязан выполняться (и проверяется
//! тестами) — тот же, что в Python: один и тот же узел (тот же kind +
//! те же значения слотов) даёт тот же cache_key НЕЗАВИСИМО от branch_id,
//! а разные значения слотов дают разные cache_key (Ядро п.2, п.3).

use crate::lexer::{LuckLexError, Token, TokenType, tokenize};
use crate::registry::{self, Kind, Slot, SlotValue, Subtype};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub enum SlotData {
    Str(String),
    Ident(String),
    /// (field, op, value-как-в-исходнике — строки уже несут кавычки)
    Cond(String, String, String),
    Args(Vec<(String, String)>),
}

impl SlotData {
    /// Каноническое строковое представление ОДНОГО слота — основа
    /// префикс-стабильности cache_key (Ядро п.2): детерминировано по
    /// значению, не зависит от порядка объявления в исходнике (для ARGS
    /// нормализованных слотов — сортировкой; для остальных порядок и так
    /// фиксирован грамматикой).
    fn canonical(&self, normalize: bool) -> String {
        match self {
            SlotData::Str(v) => v.clone(),
            SlotData::Ident(v) => v.clone(),
            SlotData::Cond(f, o, v) => format!("({f},{o},{v})"),
            SlotData::Args(pairs) => {
                let mut pairs = pairs.clone();
                if normalize {
                    pairs.sort_by(|a, b| a.0.cmp(&b.0));
                }
                pairs
                    .iter()
                    .map(|(n, v)| format!("{n}={v}"))
                    .collect::<Vec<_>>()
                    .join(",")
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct Node {
    pub node_id: String,
    pub subtype: Subtype,
    pub kind: Kind,
    pub slots: BTreeMap<&'static str, SlotData>,
    pub branch_id: String,
    pub cache_key: String,
}

impl Node {
    pub fn new(
        node_id: String,
        subtype: Subtype,
        kind: Kind,
        slots: BTreeMap<&'static str, SlotData>,
    ) -> Self {
        let mut n = Node {
            node_id,
            subtype,
            kind,
            slots,
            branch_id: "root".to_string(),
            cache_key: String::new(),
        };
        n.cache_key = n.compute_cache_key();
        n
    }

    /// graph_node_id (Срез 3): адресация ветки. cache_key НЕ включает
    /// branch_id — два актора, независимо породившие идентичный узел,
    /// делят вычисление; branch_id нужен только для адресации в истории.
    pub fn graph_node_id(&self) -> (String, String) {
        (self.cache_key.clone(), self.branch_id.clone())
    }

    /// Каноническая сериализация узла: subtype|kind|slot=value|...
    /// Порядок слотов и нормализация — из реестра, не хардкод.
    pub fn canonical_form(&self) -> String {
        let spec = registry::spec(self.kind);
        let mut parts = vec![
            self.subtype.as_str().to_string(),
            self.kind.as_str().to_string(),
        ];
        for slot in spec.slots {
            let normalize = spec.normalized_slots.contains(&slot.name);
            let value = self.slots.get(slot.name).expect("слот заполнен парсером");
            parts.push(format!("{}={}", slot.name, value.canonical(normalize)));
        }
        parts.join("|")
    }

    fn compute_cache_key(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.canonical_form().as_bytes());
        let digest = hasher.finalize();
        digest
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()[..16]
            .to_string()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeType {
    Seq,
    Branch,
    Merge,
}

#[derive(Debug, Clone)]
pub struct Edge {
    pub source: String,
    pub target: String,
    pub edge_type: EdgeType,
    pub label: Option<String>,
}

#[derive(Debug, Default)]
pub struct IntentGraph {
    pub nodes: BTreeMap<String, Node>,
    pub edges: Vec<Edge>,
}

impl IntentGraph {
    pub fn add_node(&mut self, node: Node) -> Result<(), String> {
        if self.nodes.contains_key(&node.node_id) {
            return Err(format!("узел '{}' объявлен повторно", node.node_id));
        }
        self.nodes.insert(node.node_id.clone(), node);
        Ok(())
    }

    pub fn add_edge(&mut self, edge: Edge) {
        self.edges.push(edge);
    }

    /// Ссылки в EDGES обязаны указывать на объявленные узлы — ошибка
    /// авторства статического графа, поднимается как ошибка, не reject.
    pub fn validate_edge_endpoints(&self) -> Result<(), String> {
        for e in &self.edges {
            if !self.nodes.contains_key(&e.source) {
                return Err(format!(
                    "ребро ссылается на необъявленный узел-источник '{}'",
                    e.source
                ));
            }
            if !self.nodes.contains_key(&e.target) {
                return Err(format!(
                    "ребро ссылается на необъявленный узел-цель '{}'",
                    e.target
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct LuckParseError {
    pub message: String,
    pub line: usize,
    pub col: usize,
}

impl std::fmt::Display for LuckParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[парсер] {} ({}:{})", self.message, self.line, self.col)
    }
}
impl std::error::Error for LuckParseError {}
impl From<LuckLexError> for LuckParseError {
    fn from(e: LuckLexError) -> Self {
        LuckParseError {
            message: e.message,
            line: e.line,
            col: e.col,
        }
    }
}

pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Parser { tokens, pos: 0 }
    }

    fn peek(&self) -> &Token {
        &self.tokens[self.pos]
    }

    fn advance(&mut self) -> Token {
        let tok = self.tokens[self.pos].clone();
        if tok.ttype != TokenType::Eof {
            self.pos += 1;
        }
        tok
    }

    fn expect(&mut self, ttype: TokenType) -> Result<Token, LuckParseError> {
        let tok = self.peek().clone();
        if tok.ttype != ttype {
            return Err(LuckParseError {
                message: format!(
                    "ожидался {ttype:?}, получено {:?}={:?}",
                    tok.ttype, tok.value
                ),
                line: tok.line,
                col: tok.col,
            });
        }
        Ok(self.advance())
    }

    fn check(&self, ttype: TokenType) -> bool {
        self.peek().ttype == ttype
    }

    pub fn parse_program(&mut self) -> Result<IntentGraph, LuckParseError> {
        let mut graph = IntentGraph::default();
        while self.check(TokenType::Node) {
            let node = self.parse_node_decl()?;
            graph.add_node(node).map_err(|m| self.err_here(m))?;
        }
        self.parse_edge_block(&mut graph)?;
        self.expect(TokenType::Eof)?;
        graph
            .validate_edge_endpoints()
            .map_err(|m| self.err_here(m))?;
        Ok(graph)
    }

    /// Разбор фрагмента — последовательность объявлений узлов БЕЗ EDGES.
    /// Используется планировщиком (Срез 6) для разбора вывода SPAWN.
    pub fn parse_fragment(&mut self) -> Result<Vec<Node>, LuckParseError> {
        let mut nodes = Vec::new();
        while self.check(TokenType::Node) {
            nodes.push(self.parse_node_decl()?);
        }
        Ok(nodes)
    }

    fn err_here(&self, message: String) -> LuckParseError {
        let tok = self.peek();
        LuckParseError {
            message,
            line: tok.line,
            col: tok.col,
        }
    }

    fn parse_node_decl(&mut self) -> Result<Node, LuckParseError> {
        self.expect(TokenType::Node)?;
        let node_id_tok = self.expect(TokenType::Identifier)?;
        self.expect(TokenType::Colon)?;
        let kind_tok = self.expect(TokenType::Kind)?;
        let kind = Kind::parse(&kind_tok.value).ok_or_else(|| LuckParseError {
            message: format!("неизвестный kind '{}'", kind_tok.value),
            line: kind_tok.line,
            col: kind_tok.col,
        })?;

        self.expect(TokenType::LBracket)?;
        let subtype_tok = self.advance();
        let subtype = Subtype::parse(&subtype_tok.value).ok_or_else(|| LuckParseError {
            message: format!("неизвестный subtype '{}'", subtype_tok.value),
            line: subtype_tok.line,
            col: subtype_tok.col,
        })?;
        self.expect(TokenType::RBracket)?;

        registry::validate_kind_subtype(kind, subtype).map_err(|m| LuckParseError {
            message: m,
            line: kind_tok.line,
            col: kind_tok.col,
        })?;

        let slots = self.parse_body(kind)?;
        self.expect(TokenType::End)?;
        Ok(Node::new(node_id_tok.value, subtype, kind, slots))
    }

    /// Обобщённый разбор тела узла: последовательность слотов берётся
    /// из реестра. Добавление типа узла не требует правки парсера.
    fn parse_body(
        &mut self,
        kind: Kind,
    ) -> Result<BTreeMap<&'static str, SlotData>, LuckParseError> {
        let spec = registry::spec(kind);
        let mut slots = BTreeMap::new();
        for slot in spec.slots {
            let kw_tok = self.advance();
            if kw_tok.value != slot.keyword {
                return Err(LuckParseError {
                    message: format!(
                        "в узле {} ожидалось ключевое слово '{}'",
                        kind.as_str(),
                        slot.keyword
                    ),
                    line: kw_tok.line,
                    col: kw_tok.col,
                });
            }
            slots.insert(slot.name, self.parse_slot_value(slot)?);
        }
        Ok(slots)
    }

    fn parse_slot_value(&mut self, slot: &Slot) -> Result<SlotData, LuckParseError> {
        match slot.value {
            SlotValue::String => Ok(SlotData::Str(self.expect(TokenType::String)?.value)),
            SlotValue::Ident => Ok(SlotData::Ident(self.expect(TokenType::Identifier)?.value)),
            SlotValue::Condition => self.parse_condition(),
            SlotValue::Args => self.parse_arg_list(),
            SlotValue::Enum(options) => {
                let tok = self.advance();
                if !options.contains(&tok.value.as_str()) {
                    return Err(LuckParseError {
                        message: format!("ожидалось одно из {options:?}"),
                        line: tok.line,
                        col: tok.col,
                    });
                }
                Ok(SlotData::Ident(tok.value))
            }
        }
    }

    fn parse_condition(&mut self) -> Result<SlotData, LuckParseError> {
        let left = self.expect(TokenType::Identifier)?;
        let op_tok = self.advance();
        let op = match op_tok.ttype {
            TokenType::Eq => "=",
            TokenType::Neq => "!=",
            TokenType::Gt => ">",
            TokenType::Lt => "<",
            TokenType::Gte => ">=",
            TokenType::Lte => "<=",
            _ => {
                return Err(LuckParseError {
                    message: "ожидался оператор сравнения".into(),
                    line: op_tok.line,
                    col: op_tok.col,
                });
            }
        };
        let right = self.parse_value()?;
        Ok(SlotData::Cond(left.value, op.to_string(), right))
    }

    fn parse_value(&mut self) -> Result<String, LuckParseError> {
        let tok = self.peek().clone();
        match tok.ttype {
            TokenType::String => {
                self.advance();
                Ok(format!("\"{}\"", tok.value))
            }
            TokenType::Number => {
                self.advance();
                Ok(tok.value)
            }
            TokenType::At => {
                self.advance();
                let r = self.expect(TokenType::Identifier)?;
                Ok(format!("@{}", r.value))
            }
            _ => Err(LuckParseError {
                message: "ожидалось значение (строка/число/@ссылка)".into(),
                line: tok.line,
                col: tok.col,
            }),
        }
    }

    fn parse_arg_list(&mut self) -> Result<SlotData, LuckParseError> {
        let mut args = vec![self.parse_arg()?];
        while self.check(TokenType::Comma) {
            self.advance();
            args.push(self.parse_arg()?);
        }
        Ok(SlotData::Args(args))
    }

    fn parse_arg(&mut self) -> Result<(String, String), LuckParseError> {
        let name = self.expect(TokenType::Identifier)?;
        self.expect(TokenType::Eq)?;
        let value = self.parse_value()?;
        Ok((name.value, value))
    }

    fn parse_edge_block(&mut self, graph: &mut IntentGraph) -> Result<(), LuckParseError> {
        self.expect(TokenType::Edges)?;
        self.expect(TokenType::Colon)?;
        while self.check(TokenType::Identifier) {
            let edge = self.parse_edge_decl()?;
            graph.add_edge(edge);
        }
        self.expect(TokenType::End)?;
        Ok(())
    }

    fn parse_edge_decl(&mut self) -> Result<Edge, LuckParseError> {
        let src = self.expect(TokenType::Identifier)?;
        let op_tok = self.advance();
        let edge_type = match op_tok.ttype {
            TokenType::ArrowSeq => EdgeType::Seq,
            TokenType::ArrowBranch => EdgeType::Branch,
            TokenType::ArrowMerge => EdgeType::Merge,
            _ => {
                return Err(LuckParseError {
                    message: "ожидался оператор ребра (-> | => | ~>)".into(),
                    line: op_tok.line,
                    col: op_tok.col,
                });
            }
        };
        let tgt = self.expect(TokenType::Identifier)?;

        let mut label = None;
        if self.check(TokenType::LBracket) {
            self.advance();
            let label_tok = self.expect(TokenType::Identifier)?;
            label = Some(label_tok.value);
            self.expect(TokenType::RBracket)?;
        }

        Ok(Edge {
            source: src.value,
            target: tgt.value,
            edge_type,
            label,
        })
    }
}

pub fn parse(source: &str) -> Result<IntentGraph, LuckParseError> {
    let tokens = tokenize(source)?;
    Parser::new(tokens).parse_program()
}

pub fn parse_fragment(source: &str) -> Result<Vec<Node>, LuckParseError> {
    let tokens = tokenize(source)?;
    Parser::new(tokens).parse_fragment()
}
