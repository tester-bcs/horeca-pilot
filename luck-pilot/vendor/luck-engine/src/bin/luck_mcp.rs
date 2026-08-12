//! Luck (Rust) — MCP stdio-сервер, по образцу uni-mcp-v2 (`uni-mcp mcp`).
//!
//! Один инструмент — run_luck_pipeline(source: string) -> текстовый
//! отчёт об исполнении. Переиспользует ту же логику parse->Scheduler->
//! AnthropicTransport, что run_real.rs — не дублирует её (см. report.rs,
//! tools.rs::register_default_tools).
//!
//!     cargo run --release --bin luck_mcp
//!
//! Протокол — JSON-RPC 2.0 по строке на сообщение (тот же формат, что
//! в README uni-mcp-v2): initialize -> notifications/initialized ->
//! tools/list -> tools/call. stdout — ТОЛЬКО протокол, ничего больше;
//! все логи и диагностика — в stderr (иначе один println! в неверном
//! месте молча ломает протокол для клиента).
//!
//! ОТЛИЧИЕ ОТ run_real.rs, ОСОЗНАННОЕ: там ANTHROPIC_API_KEY можно
//! ввести интерактивно (rpassword), потому что это интерактивный CLI.
//! Здесь — НЕЛЬЗЯ: stdin занят протоколом (JSON-RPC сообщения клиента),
//! попытка прочитать оттуда пароль исказила бы протокол. Ключ — только
//! из окружения; отсутствие ключа не валит сервер целиком (initialize/
//! tools/list по-прежнему отвечают, как requires_llm-гейт в uni-mcp-v2),
//! а даёт понятную JSON-RPC ошибку конкретно на tools/call.
//!
//! ЧЕГО НЕТ (сознательно, не забыто): source — ГОТОВЫЙ Luck-текст, без
//! скрытого механизма шаблонной подстановки (в отличие от run_batch.rs
//! с его {{ISSUE}}). Если вызывающей стороне нужна подстановка данных в
//! граф — это её работа до вызова, не часть протокола MCP-инструмента:
//! добавлять сюда магический плейсхолдер-синтаксис значило бы завести
//! диалект Luck, не описанный в docs/SPEC.md.

use luck::anthropic::{AnthropicTransport, ValidatingBackend};
use luck::{Scheduler, ToolRegistry, format_execution_report, parse, register_default_tools};
use serde_json::{Value, json};
use std::io::{BufRead, Write};

const PROTOCOL_VERSION: &str = "2025-06-18";
const SERVER_NAME: &str = "luck-mcp";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

fn tool_schema() -> Value {
    json!({
        "name": "run_luck_pipeline",
        "description": "Parse and execute a Luck (.luck) intent-graph program \
            against a real Claude model, returning the full execution trace \
            (traversal order, per-node outputs, retries, rejects). Requires \
            ANTHROPIC_API_KEY configured in the server environment.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "source": {
                    "type": "string",
                    "description": "Full Luck source: NODE ... END blocks \
                        followed by an EDGES: ... END block. See docs/SPEC.md \
                        in the luck-repo for the grammar."
                }
            },
            "required": ["source"]
        }
    })
}

fn rpc_error(code: i64, message: impl Into<String>) -> Value {
    json!({ "code": code, "message": message.into() })
}

fn handle_run_pipeline(args: &Value) -> Result<Value, Value> {
    let source = args
        .get("source")
        .and_then(Value::as_str)
        .ok_or_else(|| rpc_error(-32602, "Missing required argument 'source'"))?;

    let api_key = std::env::var("ANTHROPIC_API_KEY").map_err(|_| {
        rpc_error(
            -32603,
            "Server not configured: ANTHROPIC_API_KEY is not set in the environment. \
             Set it before starting luck_mcp — this server never prompts for it \
             interactively (stdin carries the JSON-RPC protocol, not a password).",
        )
    })?;

    let mut graph =
        parse(source).map_err(|e| rpc_error(-32602, format!("Luck syntax error: {e}")))?;

    let model = std::env::var("LUCK_ANTHROPIC_MODEL")
        .unwrap_or_else(|_| "claude-haiku-4-5-20251001".to_string());
    let base_url = std::env::var("LUCK_ANTHROPIC_BASE_URL")
        .unwrap_or_else(|_| "https://api.anthropic.com".to_string());
    let max_tokens: u32 = std::env::var("LUCK_MAX_TOKENS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(4096);
    let max_attempts: u32 = std::env::var("LUCK_MAX_ATTEMPTS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3);

    let transport = AnthropicTransport::new(api_key, model.clone())
        .with_base_url(base_url)
        .with_max_tokens(max_tokens);
    let mut backend = ValidatingBackend::new(transport, max_attempts);
    let mut tools = ToolRegistry::new();
    register_default_tools(&mut tools);

    let mut sched = Scheduler::new(&mut backend, &mut tools);
    let result = sched
        .run(&mut graph, "mcp_run")
        .map_err(|e| rpc_error(-32603, format!("Execution aborted: {e}")))?;

    let report = format_execution_report(&graph, &result, &model, backend.retry_count, "mcp");
    Ok(json!({ "content": [{ "type": "text", "text": report }] }))
}

fn dispatch(method: &str, params: &Value) -> Result<Value, Value> {
    match method {
        "initialize" => Ok(json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": { "tools": {} },
            "serverInfo": { "name": SERVER_NAME, "version": SERVER_VERSION },
        })),
        "tools/list" => Ok(json!({ "tools": [tool_schema()] })),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let empty = json!({});
            let arguments = params.get("arguments").unwrap_or(&empty);
            match name {
                "run_luck_pipeline" => handle_run_pipeline(arguments),
                other => Err(rpc_error(-32601, format!("Tool not found: {other}"))),
            }
        }
        other => Err(rpc_error(-32601, format!("Method not found: {other}"))),
    }
}

fn main() {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("luck_mcp: ошибка чтения stdin: {e}");
                break;
            }
        };
        if line.trim().is_empty() {
            continue;
        }

        let req: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("luck_mcp: невалидный JSON во входящей строке: {e}");
                continue;
            }
        };

        let method = req.get("method").and_then(Value::as_str).unwrap_or("");
        let id = req.get("id").cloned();

        // Уведомления (без id) не получают ответа — только логируем и
        // молча продолжаем, как notifications/initialized в примере
        // README uni-mcp-v2.
        if id.is_none() {
            eprintln!("luck_mcp: уведомление '{method}', ответ не требуется");
            continue;
        }

        let empty_params = json!({});
        let params = req.get("params").unwrap_or(&empty_params);
        let response = match dispatch(method, params) {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(error) => json!({ "jsonrpc": "2.0", "id": id, "error": error }),
        };

        if let Err(e) = writeln!(stdout, "{response}") {
            eprintln!("luck_mcp: не удалось записать ответ в stdout: {e}");
            break;
        }
        if let Err(e) = stdout.flush() {
            eprintln!("luck_mcp: не удалось сбросить буфер stdout: {e}");
            break;
        }
    }
}
