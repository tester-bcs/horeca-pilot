//! Luck — Срез 4: презентационный синтаксис
//! Ветка: Rust-проход (Маркер В.0 -> возврат)
//!
//! Зеркалит luck/presentation.py: второй поверхностный синтаксис —
//! тегированный, ближе к POML. Допустим по скорректированному Ядру:
//! одна каноническая форма плюс любое число представлений, находящихся
//! с ней в БИЕКЦИИ (запрет касался потерь, не трансляции как таковой).
//!
//! БИЕКЦИЯ СТРУКТУРНА, А НЕ ДОКАЗЫВАЕМА: генератор (`node_to_element`) и
//! разбор (`element_to_node`) обходят один и тот же `spec.slots` в одном
//! порядке — так же, как в Python. Новый тип узла в реестре автоматически
//! получает корректную биекцию.
//!
//! ОТЛИЧИЕ ОТ PYTHON-ВЕТКИ (MARKER_V0_RUST п.5): Python использует
//! `xml.etree.ElementTree` из стандартной библиотеки. Rust не имеет XML
//! в std, а тянуть внешний крейт ради подмножества, которое сам же и
//! генерируешь и сам же разбираешь, избыточно — весь проект до сих пор
//! использовал ровно один внешний крейт (`sha2`, для того, чего в std
//! нет вообще). Здесь написан свой минимальный `Element` + парсер под
//! этот конкретный формат (без namespaces, CDATA, DOCTYPE — они и
//! Python-веткой не использовались). Это осознанное сужение: чужой XML
//! (не порождённый этим генератором) может не распарситься.

use crate::parser::{Edge, EdgeType, IntentGraph, Node, SlotData};
use crate::registry::{self, Kind, SlotValue};

#[derive(Debug, Clone, Default)]
pub struct Element {
    pub tag: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<Element>,
    pub text: Option<String>,
}

impl Element {
    fn new(tag: impl Into<String>) -> Self {
        Element {
            tag: tag.into(),
            attrs: Vec::new(),
            children: Vec::new(),
            text: None,
        }
    }

    fn attr(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.attrs.push((k.into(), v.into()));
        self
    }

    fn get_attr(&self, k: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.as_str())
    }

    fn find(&self, tag: &str) -> Option<&Element> {
        self.children.iter().find(|c| c.tag == tag)
    }

    fn find_all(&self, tag: &str) -> Vec<&Element> {
        self.children.iter().filter(|c| c.tag == tag).collect()
    }
}

#[derive(Debug)]
pub struct PresentationError(pub String);

impl std::fmt::Display for PresentationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl std::error::Error for PresentationError {}

// ==================== 4.1 — генератор презентации ====================

/// Каноническая форма -> элемент презентации. Обход слотов идёт строго
/// по `spec.slots` — тому же кортежу, что использует парсер канонического
/// синтаксиса. Подтип узла НЕ записывается: он однозначно определяется
/// kind через реестр (опускание выводимого не нарушает биекцию).
pub fn node_to_element(node: &Node) -> Element {
    let spec = registry::spec(node.kind);
    let mut el = Element::new(node.kind.as_str().to_lowercase()).attr("id", &node.node_id);

    for slot in spec.slots {
        let value = node.slots.get(slot.name).expect("слот заполнен");
        let mut child = Element::new(slot.name);
        match value {
            SlotData::Str(v) | SlotData::Ident(v) => child.text = Some(v.clone()),
            SlotData::Cond(field, op, val) => {
                child = child
                    .attr("field", field.clone())
                    .attr("op", op.clone())
                    .attr("value", val.clone());
            }
            SlotData::Args(pairs) => {
                for (name, val) in pairs {
                    child.children.push(
                        Element::new("arg")
                            .attr("name", name.clone())
                            .attr("value", val.clone()),
                    );
                }
            }
        }
        el.children.push(child);
    }
    el
}

pub fn graph_to_element(graph: &IntentGraph) -> Element {
    let mut root = Element::new("luck");
    let mut nodes_el = Element::new("nodes");
    for node in graph.nodes.values() {
        nodes_el.children.push(node_to_element(node));
    }
    root.children.push(nodes_el);

    let mut edges_el = Element::new("edges");
    for e in &graph.edges {
        let etype = match e.edge_type {
            EdgeType::Seq => "seq",
            EdgeType::Branch => "branch",
            EdgeType::Merge => "merge",
        };
        let mut edge_el = Element::new("edge")
            .attr("from", &e.source)
            .attr("to", &e.target)
            .attr("type", etype);
        if let Some(l) = &e.label {
            edge_el = edge_el.attr("label", l.clone());
        }
        edges_el.children.push(edge_el);
    }
    root.children.push(edges_el);
    root
}

// ==================== 4.2 — обратный разбор ====================

/// Элемент презентации -> узел канонической формы. Обход по `spec.slots`,
/// не по детям элемента: порядок и состав диктует реестр — инверсия
/// генератора по построению.
pub fn element_to_node(el: &Element, branch_id: &str) -> Result<Node, PresentationError> {
    let kind = Kind::parse(&el.tag.to_uppercase())
        .ok_or_else(|| PresentationError(format!("неизвестный тип узла '{}'", el.tag)))?;
    let spec = registry::spec(kind);

    let node_id = el
        .get_attr("id")
        .ok_or_else(|| PresentationError(format!("у элемента <{}> отсутствует id", el.tag)))?
        .to_string();

    let mut slots = std::collections::BTreeMap::new();
    for slot in spec.slots {
        let child = el.find(slot.name).ok_or_else(|| {
            PresentationError(format!(
                "в <{} id={node_id}> отсутствует слот <{}>",
                el.tag, slot.name
            ))
        })?;
        let value = match slot.value {
            SlotValue::String => SlotData::Str(child.text.clone().unwrap_or_default()),
            SlotValue::Ident => SlotData::Ident(child.text.clone().unwrap_or_default()),
            SlotValue::Enum(options) => {
                let val = child.text.clone().unwrap_or_default();
                if !options.contains(&val.as_str()) {
                    return Err(PresentationError(format!(
                        "слот <{}>: ожидалось одно из {options:?}",
                        slot.name
                    )));
                }
                SlotData::Ident(val)
            }
            SlotValue::Condition => {
                let field = child.get_attr("field").unwrap_or_default().to_string();
                let op = child.get_attr("op").unwrap_or_default().to_string();
                let val = child.get_attr("value").unwrap_or_default().to_string();
                SlotData::Cond(field, op, val)
            }
            SlotValue::Args => {
                let pairs = child
                    .find_all("arg")
                    .into_iter()
                    .map(|a| {
                        (
                            a.get_attr("name").unwrap_or_default().to_string(),
                            a.get_attr("value").unwrap_or_default().to_string(),
                        )
                    })
                    .collect();
                SlotData::Args(pairs)
            }
        };
        slots.insert(slot.name, value);
    }

    let mut node = Node::new(node_id, spec.subtype, kind, slots);
    node.branch_id = branch_id.to_string();
    Ok(node)
}

fn edge_type_from_str(s: &str) -> Option<EdgeType> {
    match s {
        "seq" => Some(EdgeType::Seq),
        "branch" => Some(EdgeType::Branch),
        "merge" => Some(EdgeType::Merge),
        _ => None,
    }
}

pub fn element_to_graph(root: &Element, branch_id: &str) -> Result<IntentGraph, PresentationError> {
    let mut graph = IntentGraph::default();
    if let Some(nodes_el) = root.find("nodes") {
        for el in &nodes_el.children {
            let node = element_to_node(el, branch_id)?;
            graph.add_node(node).map_err(PresentationError)?;
        }
    }
    if let Some(edges_el) = root.find("edges") {
        for e in edges_el.find_all("edge") {
            let etype_str = e.get_attr("type").unwrap_or("seq");
            let edge_type = edge_type_from_str(etype_str)
                .ok_or_else(|| PresentationError(format!("неизвестный тип ребра '{etype_str}'")))?;
            graph.add_edge(Edge {
                source: e.get_attr("from").unwrap_or_default().to_string(),
                target: e.get_attr("to").unwrap_or_default().to_string(),
                edge_type,
                label: e.get_attr("label").map(String::from),
            });
        }
    }
    graph.validate_edge_endpoints().map_err(PresentationError)?;
    Ok(graph)
}

// ==================== 4.4 — стилевой слой (в биекции НЕ участвует) ====================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    /// С отступами, по строке на элемент.
    Expanded,
    /// Одной строкой.
    Compact,
}

fn esc(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn render_compact(el: &Element, out: &mut String) {
    out.push('<');
    out.push_str(&el.tag);
    for (k, v) in &el.attrs {
        out.push_str(&format!(" {k}=\"{}\"", esc(v)));
    }
    if el.children.is_empty() && el.text.is_none() {
        out.push_str(" />");
        return;
    }
    out.push('>');
    if let Some(t) = &el.text {
        out.push_str(&esc(t));
    }
    for c in &el.children {
        render_compact(c, out);
    }
    out.push_str(&format!("</{}>", el.tag));
}

fn render_expanded(el: &Element, level: usize, out: &mut String) {
    let pad = "  ".repeat(level);
    let attrs: String = el
        .attrs
        .iter()
        .map(|(k, v)| format!(" {k}=\"{}\"", esc(v)))
        .collect();

    if el.children.is_empty() {
        if let Some(t) = &el.text {
            out.push_str(&format!("{pad}<{}{attrs}>{}</{}>", el.tag, esc(t), el.tag));
        } else {
            out.push_str(&format!("{pad}<{}{attrs}/>", el.tag));
        }
        return;
    }

    out.push_str(&format!("{pad}<{}{attrs}>\n", el.tag));
    for (i, c) in el.children.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        render_expanded(c, level + 1, out);
    }
    out.push_str(&format!("\n{pad}</{}>", el.tag));
}

pub fn render(el: &Element, style: Style) -> String {
    let mut out = String::new();
    match style {
        Style::Compact => render_compact(el, &mut out),
        Style::Expanded => render_expanded(el, 0, &mut out),
    }
    out
}

// ==================== Минимальный XML-парсер под этот формат ====================
// Осознанно узкий (см. докстринг модуля): элементы, атрибуты, текст,
// самозакрывающиеся теги. Без namespaces/CDATA/DOCTYPE/комментариев.

struct XmlParser<'a> {
    chars: &'a [char],
    pos: usize,
}

impl<'a> XmlParser<'a> {
    fn skip_ws(&mut self) {
        while self.pos < self.chars.len() && self.chars[self.pos].is_whitespace() {
            self.pos += 1;
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn expect_char(&mut self, c: char) -> Result<(), PresentationError> {
        if self.peek() != Some(c) {
            return Err(PresentationError(format!(
                "ожидался '{c}' в позиции {}",
                self.pos
            )));
        }
        self.pos += 1;
        Ok(())
    }

    fn read_name(&mut self) -> String {
        let start = self.pos;
        while self.pos < self.chars.len() {
            let c = self.chars[self.pos];
            if c.is_alphanumeric() || c == '_' || c == '-' || c == ':' {
                self.pos += 1;
            } else {
                break;
            }
        }
        self.chars[start..self.pos].iter().collect()
    }

    fn unescape(s: &str) -> String {
        s.replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&amp;", "&")
    }

    fn read_attrs(&mut self) -> Result<Vec<(String, String)>, PresentationError> {
        let mut attrs = Vec::new();
        loop {
            self.skip_ws();
            match self.peek() {
                Some('/') | Some('>') | None => break,
                _ => {}
            }
            let name = self.read_name();
            self.skip_ws();
            self.expect_char('=')?;
            self.skip_ws();
            self.expect_char('"')?;
            let start = self.pos;
            while self.peek().is_some() && self.peek() != Some('"') {
                self.pos += 1;
            }
            let raw: String = self.chars[start..self.pos].iter().collect();
            self.expect_char('"')?;
            attrs.push((name, Self::unescape(&raw)));
        }
        Ok(attrs)
    }

    fn parse_element(&mut self) -> Result<Element, PresentationError> {
        self.skip_ws();
        self.expect_char('<')?;
        let tag = self.read_name();
        let attrs = self.read_attrs()?;
        self.skip_ws();

        if self.peek() == Some('/') {
            self.pos += 1;
            self.expect_char('>')?;
            return Ok(Element {
                tag,
                attrs,
                children: Vec::new(),
                text: None,
            });
        }
        self.expect_char('>')?;

        self.skip_ws();
        if self.peek() == Some('<') && self.chars.get(self.pos + 1) != Some(&'/') {
            // дочерние элементы
            let mut children = Vec::new();
            loop {
                self.skip_ws();
                if self.peek() == Some('<') && self.chars.get(self.pos + 1) == Some(&'/') {
                    break;
                }
                children.push(self.parse_element()?);
            }
            self.skip_ws();
            self.expect_close(&tag)?;
            return Ok(Element {
                tag,
                attrs,
                children,
                text: None,
            });
        }

        // текстовое содержимое
        let start = self.pos;
        while self.peek().is_some() && self.peek() != Some('<') {
            self.pos += 1;
        }
        let text = Self::unescape(&self.chars[start..self.pos].iter().collect::<String>());
        self.expect_close(&tag)?;
        Ok(Element {
            tag,
            attrs,
            children: Vec::new(),
            text: Some(text),
        })
    }

    fn expect_close(&mut self, tag: &str) -> Result<(), PresentationError> {
        self.expect_char('<')?;
        self.expect_char('/')?;
        let closing = self.read_name();
        if closing != tag {
            return Err(PresentationError(format!(
                "несовпадение закрывающего тега: <{tag}> .. </{closing}>"
            )));
        }
        self.skip_ws();
        self.expect_char('>')?;
        Ok(())
    }
}

pub fn parse_element(text: &str) -> Result<Element, PresentationError> {
    let chars: Vec<char> = text.chars().collect();
    let mut p = XmlParser {
        chars: &chars,
        pos: 0,
    };
    let el = p.parse_element()?;
    p.skip_ws();
    Ok(el)
}

// ==================== Публичный интерфейс ====================

pub fn to_presentation(graph: &IntentGraph, style: Style) -> String {
    render(&graph_to_element(graph), style)
}

pub fn from_presentation(text: &str, branch_id: &str) -> Result<IntentGraph, PresentationError> {
    let root = parse_element(text)?;
    element_to_graph(&root, branch_id)
}

/// canonical -> presentation -> canonical для одного узла — контракт
/// Среза 3 <-> Среза 4: сохраняет и слоты, и cache_key.
pub fn node_round_trip(node: &Node) -> Result<Node, PresentationError> {
    let text = render(&node_to_element(node), Style::Expanded);
    let el = parse_element(&text)?;
    element_to_node(&el, &node.branch_id)
}
