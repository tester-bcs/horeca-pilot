// ---------------------------------------------------------------------------
// luck_compile — компилятор Luck-синтаксиса -> plan.json (v1 подмножество)
//
// Этап 2 интеграции Luck в ai-agent (см. docs/luck-integration.md).
// Нативный Rust-парсер подмножества Luck-графа:
//
//   NODE role: ROLE
//     AS "senior incident engineer"
//     INTO ctx
//   END
//
//   EDGES
//     role -> severity
//   END
//
// Компилятор строит Plan (serde-типы из luck_plan) и прогоняет через validate() —
// невалидный граф не компилируется (контракты проверяются на компиляции).
// ---------------------------------------------------------------------------

use std::collections::BTreeMap;

use crate::luck_plan::{Edge, EdgeType, Limits, Node, NodeKind, Plan, Policy, VerifySpec, validate};
use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CompileError {
    #[error("line {line}: {msg}")]
    Line { line: usize, msg: String },
    #[error("node block not closed (missing END) for node: {0}")]
    UnclosedNode(String),
    #[error("unknown node kind: {0}")]
    UnknownKind(String),
    #[error("duplicate slot: {0}")]
    DuplicateSlot(String),
    #[error("plan validation failed: {0}")]
    Validation(String),
}

type CResult<T> = Result<T, CompileError>;

fn err(line: usize, msg: impl Into<String>) -> CompileError {
    CompileError::Line { line, msg: msg.into() }
}

/// Разобрать значение в кавычках (с поддержкой \"-эскейпов).
fn parse_quoted(s: &str, line: usize) -> CResult<String> {
    let s = s.trim();
    if !s.starts_with('"') || !s.ends_with('"') {
        return Err(err(line, format!("expected quoted string, got: {s}")));
    }
    Ok(s[1..s.len() - 1].replace("\\\"", "\""))
}

/// Разобрать список пар k="v", разделённых запятыми.
fn parse_kv_list(s: &str, line: usize) -> CResult<Vec<(String, String)>> {
    let mut out = Vec::new();
    for item in s.split(',') {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        let (k, v) = item
            .split_once('=')
            .ok_or_else(|| err(line, format!("expected key=\"value\", got: {item}")))?;
        out.push((k.trim().to_string(), parse_quoted(v, line)?));
    }
    Ok(out)
}

/// Разобрать BRANCHES a=b, c=d (значения без кавычек).
fn parse_branches(s: &str, line: usize) -> CResult<Vec<(String, String)>> {
    let mut out = Vec::new();
    for item in s.split(',') {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        let (k, v) = item
            .split_once('=')
            .ok_or_else(|| err(line, format!("expected label=target, got: {item}")))?;
        out.push((k.trim().to_string(), v.trim().to_string()));
    }
    Ok(out)
}

fn parse_policy(s: &str, line: usize) -> CResult<Policy> {
    let mut require = None;
    let mut allow = Vec::new();
    for item in s.split(',') {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        let (k, v) = item
            .split_once('=')
            .ok_or_else(|| err(line, format!("expected key=value in POLICY, got: {item}")))?;
        match k.trim() {
            "require" => require = Some(v.trim().to_string()),
            "allow" => allow.push(v.trim().to_string()),
            other => return Err(err(line, format!("unknown POLICY key: {other}"))),
        }
    }
    Ok(Policy { require, allow })
}

struct NodeDraft {
    id: String,
    kind: NodeKind,
    into: Option<String>,
    input: Option<String>,
    labels: Vec<String>,
    branches: BTreeMap<String, Vec<String>>,
    tool: Option<String>,
    args: Value,
    policy: Option<Policy>,
    verify: Option<VerifySpec>,
    on_fail: Option<String>,
    do_: Option<String>,
    slots: Value,
}

impl NodeDraft {
    fn finish(self) -> Node {
        Node {
            id: self.id,
            kind: self.kind,
            into: self.into,
            input: self.input,
            labels: self.labels,
            branches: self.branches,
            tool: self.tool,
            args: self.args,
            policy: self.policy,
            verify: self.verify,
            on_fail: self.on_fail,
            do_: self.do_,
            slots: self.slots,
        }
    }
}

fn parse_kind(raw: &str, line: usize) -> CResult<NodeKind> {
    let k = raw.trim().to_uppercase();
    let kind = match k.as_str() {
        "ROLE" => NodeKind::Role,
        "CLASSIFY" => NodeKind::Classify,
        "FILTER" => NodeKind::Filter,
        "STEP" => NodeKind::Step,
        "TASK" => NodeKind::Task,
        "SPAWN" => NodeKind::Spawn,
        "TOOL" => NodeKind::Tool,
        "DOCUMENT" => NodeKind::Document,
        "BRANCH" => NodeKind::Branch,
        "MERGE" => NodeKind::Merge,
        "VERIFY" => NodeKind::Verify,
        "REJECT" => NodeKind::Reject,
        _ => return Err(CompileError::UnknownKind(raw.trim().to_string())),
    };
    let _ = line;
    Ok(kind)
}

/// Скомпилировать Luck-текст в Plan (с валидацией).
pub fn compile(text: &str) -> CResult<Plan> {
    let mut nodes: Vec<Node> = Vec::new();
    let mut edges: Vec<Edge> = Vec::new();
    let mut gen_edges: Vec<(usize, Edge)> = Vec::new(); // автогенерированные branch-рёбра

    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0usize;
    while i < lines.len() {
        let line_no = i + 1;
        let raw = lines[i];
        let line = raw.split('#').next().unwrap_or("").trim();
        i += 1;
        if line.is_empty() {
            continue;
        }

        if line.eq_ignore_ascii_case("EDGES") {
            // Секция рёбер до END.
            while i < lines.len() {
                let el = lines[i];
                let el_line = i + 1;
                i += 1;
                let el = el.split('#').next().unwrap_or("").trim();
                if el.is_empty() {
                    continue;
                }
                if el.eq_ignore_ascii_case("END") {
                    break;
                }
                // Формат: a -> b  или  a -> b [label]
                let parts: Vec<&str> = el.split("->").collect();
                if parts.len() != 2 {
                    return Err(err(el_line, format!("expected 'a -> b', got: {el}")));
                }
                let from = parts[0].trim();
                let mut to_part = parts[1].trim();
                let mut label = None;
                if let Some(rest) = to_part.strip_suffix(']') {
                    if let Some(bracket) = rest.rfind('[') {
                        label = Some(rest[bracket + 1..].trim().to_string());
                        to_part = rest[..bracket].trim();
                    }
                }
                if from.is_empty() || to_part.is_empty() {
                    return Err(err(el_line, "empty node id in edge"));
                }
                let edge_type = if label.is_some() {
                    EdgeType::Branch
                } else {
                    EdgeType::Seq
                };
                gen_edges.push((
                    el_line,
                    Edge {
                        from: from.to_string(),
                        to: to_part.to_string(),
                        edge_type,
                        label,
                    },
                ));
            }
            continue;
        }

        if line.starts_with("NODE ") || line.starts_with("node ") {
            // Заголовок: NODE id: KIND
            let header = line[4..].trim();
            let (id, kind_raw) = header
                .split_once(':')
                .ok_or_else(|| err(line_no, format!("expected 'NODE id: KIND', got: {header}")))?;
            let id = id.trim();
            let kind = parse_kind(kind_raw, line_no)?;
            if id.is_empty() {
                return Err(err(line_no, "empty node id"));
            }

            let mut d = NodeDraft {
                id: id.to_string(),
                kind,
                into: None,
                input: None,
                labels: Vec::new(),
                branches: BTreeMap::new(),
                tool: None,
                args: Value::Null,
                policy: None,
                verify: None,
                on_fail: None,
                do_: None,
                slots: json!({}),
            };
            let mut closed = false;
            while i < lines.len() {
                let sl = lines[i];
                let sl_no = i + 1;
                i += 1;
                let sl = sl.split('#').next().unwrap_or("").trim();
                if sl.is_empty() {
                    continue;
                }
                if sl.eq_ignore_ascii_case("END") {
                    closed = true;
                    break;
                }
                let (key, rest) = sl
                    .split_once(char::is_whitespace)
                    .unwrap_or((sl, ""));
                let rest = rest.trim();
                match key.to_uppercase().as_str() {
                    "INTO" => d.into = Some(rest.to_string()),
                    "INPUT" => d.input = Some(rest.to_string()),
                    "AS" => {
                        let v = parse_quoted(rest, sl_no)?;
                        if let Value::Object(m) = &mut d.slots {
                            m.insert("as".to_string(), Value::String(v));
                        }
                    }
                    "DO" => d.do_ = Some(parse_quoted(rest, sl_no)?),
                    "TOOL" => d.tool = Some(rest.to_string()),
                    "LABELS" => {
                        for (k, v) in parse_kv_list(rest, sl_no)? {
                            d.labels.push(v);
                            let _ = k;
                        }
                    }
                    "BRANCHES" => {
                        for (k, v) in parse_branches(rest, sl_no)? {
                            d.branches.entry(k).or_default().push(v);
                        }
                    }
                    "ARGS" => {
                        d.args = serde_json::from_str(rest)
                            .map_err(|e| err(sl_no, format!("invalid ARGS JSON: {e}")))?;
                    }
                    "POLICY" => d.policy = Some(parse_policy(rest, sl_no)?),
                    "VERIFY" => {
                        // VERIFY <pred> [INTO <subject>]
                        let mut it = rest.split_whitespace();
                        let pred = it.next().unwrap_or("").to_string();
                        let mut subject = None;
                        let mut toks = it;
                        let mut expect_subj = false;
                        for t in toks.by_ref() {
                            if expect_subj {
                                subject = Some(t.to_string());
                                expect_subj = false;
                            } else if t.eq_ignore_ascii_case("INTO") {
                                expect_subj = true;
                            }
                        }
                        d.verify = Some(VerifySpec { predicate: pred, subject });
                    }
                    "ON_FAIL" => d.on_fail = Some(rest.to_string()),
                    other => {
                        return Err(err(sl_no, format!("unknown slot: {other}")));
                    }
                }
            }
            if !closed {
                return Err(CompileError::UnclosedNode(id.to_string()));
            }
            nodes.push(d.finish());
        }
    }

    for (el_line, e) in gen_edges {
        let from_known = nodes.iter().any(|n| n.id == e.from);
        let to_known = nodes.iter().any(|n| n.id == e.to);
        if !from_known || !to_known {
            return Err(err(
                el_line,
                format!("edge {}->{} references unknown node", e.from, e.to),
            ));
        }
        edges.push(e);
    }

    // Автогенерация branch-рёбер из слотов BRANCHES узлов (label=target).
    let branch_targets: Vec<(String, String, String)> = nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Branch)
        .flat_map(move |n| {
            n.branches
                .iter()
                .flat_map(move |(label, targets)| targets.iter().map(move |t| (n.id.clone(), label.clone(), t.clone())))
        })
        .collect();
    for (from, label, to) in branch_targets {
        edges.push(Edge {
            from,
            to,
            edge_type: EdgeType::Branch,
            label: Some(label),
        });
    }

    let plan = Plan {
        plan_version: 1,
        nodes,
        edges,
        limits: Limits::default(),
    };
    validate(&plan).map_err(|e| CompileError::Validation(e.to_string()))?;
    Ok(plan)
}

// ---------------------------------------------------------------------------
// Тесты
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
# Инцидент-триаж
NODE role: ROLE
  AS "senior incident engineer"
  INTO ctx
END

NODE severity: CLASSIFY
  INPUT ctx
  LABELS a="critical", b="warning"
  INTO level
END

NODE fork: BRANCH
  BRANCHES critical=probe, warning=probe
END

NODE probe: TOOL
  TOOL shell
  ARGS {"cmd": "kubectl get pods"}
  INTO out
END

NODE verify_out: VERIFY
  VERIFY not_empty INTO out
END

NODE merge: MERGE
  INTO final
END

NODE report: STEP
  DO "synthesize report"
  VERIFY grep INTO final
  INTO result
END

EDGES
  role -> severity
  severity -> fork
  probe -> merge
  verify_out -> merge
  merge -> report
END
"#;

    #[test]
    fn compiles_sample() {
        let plan = compile(SAMPLE).expect("sample should compile");
        assert_eq!(plan.nodes.len(), 7);
        assert_eq!(plan.edges.len(), 7); // 5 seq + 2 branch (auto из BRANCHES)
    }

    #[test]
    fn compiles_into_valid_plan() {
        let plan = compile(SAMPLE).unwrap();
        validate(&plan).expect("compiled plan must pass validation");
    }

    #[test]
    fn branch_edges_generated_with_labels() {
        let plan = compile(SAMPLE).unwrap();
        let branches: Vec<_> = plan
            .edges
            .iter()
            .filter(|e| e.edge_type == EdgeType::Branch)
            .collect();
        assert_eq!(branches.len(), 2);
        assert!(branches.iter().all(|e| e.label.is_some()));
    }

    #[test]
    fn unknown_kind_rejected() {
        let src = "NODE x: WIZARD\nEND\n";
        assert!(matches!(compile(src), Err(CompileError::UnknownKind(_))));
    }

    #[test]
    fn unclosed_node_rejected() {
        let src = "NODE x: ROLE\n";
        assert!(matches!(compile(src), Err(CompileError::UnclosedNode(_))));
    }

    #[test]
    fn edge_to_unknown_rejected() {
        let src = r#"
NODE a: STEP
  DO "x"
END
EDGES
  a -> ghost
END
"#;
        assert!(matches!(compile(src), Err(CompileError::Line { .. })));
    }

    #[test]
    fn bad_args_json_rejected() {
        let src = r#"
NODE t: TOOL
  TOOL shell
  ARGS {"cmd": }
END
"#;
        assert!(matches!(compile(src), Err(CompileError::Line { .. })));
    }

    #[test]
    fn verify_contract_enforced_at_compile() {
        // VERIFY с неизвестным предикатом — план не компилируется (валидатор).
        let src = r#"
NODE v: VERIFY
  VERIFY llm_judge INTO out
END
"#;
        assert!(matches!(compile(src), Err(CompileError::Validation(_))));
    }
}
