//! Веб-интерфейс пайпа: список сценариев -> запуск -> итог -> документы.
//! Использование:
//!   OLLAMA_HOST=http://100.64.0.1:11434 OLLAMA_MODEL=hermes3:8b OLLAMA_ONLY=1 \
//!     cargo run --bin web -- [порт]
//! Открыть http://localhost:8080
//!
//! Отличие от старой версии: канонический Scheduler (vendor/luck-engine) —
//! однопоточный синхронный обход без потоковых событий по узлам (нет
//! аналога старого PlanEvent-канала — Scheduler не эмитит прогресс по
//! ходу, только итоговый ExecutionResult). Прогресс UI здесь по этой
//! причине огрублён до "running -> completed/rejected" без покадровой
//! трансляции узлов; `ExecutionResult::order` после завершения всё же даёт
//! порядок исполнения для отображения постфактум.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Html,
    routing::{get, post},
    Json, Router,
};
use luck_engine::anthropic::ValidatingBackend;
use luck_engine::parser::parse;
use luck_engine::scheduler::{Scheduler, ToolRegistry};
use luck_pilot::openrouter::{register_demo_tools, FallbackTransport};
use luck_pilot::verify::{run_verified, VerifiedOutcome};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Instant,
};

const SCENARIOS_DIR: &str = "../examples_luck";
const SCENARIOS: &[&str] = &[
    "horeca-daily-cycle",
    "horeca-returns",
    "horeca-inventory",
    "horeca-cashflow",
];

#[derive(Clone)]
struct RunState {
    status: String, // idle | running | completed | rejected
    lines: Vec<String>,
    result: String,
    started: Option<Instant>,
}

type SharedState = Arc<Mutex<HashMap<String, RunState>>>;

#[tokio::main]
async fn main() {
    let port: u16 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(8080);
    let shared: SharedState = Arc::new(Mutex::new(HashMap::new()));

    let app = Router::new()
        .route("/", get(index))
        .route("/api/scenarios", get(list_scenarios))
        .route("/api/run/:name", post(run_scenario))
        .route("/api/status/:name", get(status_scenario))
        .fallback(|req: axum::extract::Request| async move {
            eprintln!("[404] {} {}", req.method(), req.uri());
            (StatusCode::NOT_FOUND, "nf")
        })
        .with_state(shared);

    let addr = format!("0.0.0.0:{port}");
    println!("== веб-пайп HoReCa: http://{addr} ==");
    let listener = tokio::net::TcpListener::bind(&addr).await.expect("bind");
    axum::serve(listener, app).await.expect("serve");
}

// ---------------------------------------------------------------------------
// Страница
// ---------------------------------------------------------------------------

async fn index() -> Html<String> {
    let scenarios = SCENARIOS
        .iter()
        .map(|s| format!(r#"<button onclick="run('{s}')">{s}</button>"#))
        .collect::<Vec<_>>()
        .join("\n");
    Html(format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><title>HoReCa пайп</title>
<style>
body{{font-family:monospace;background:#0a0a0a;color:#ddd;padding:20px}}
button{{background:#1a1a2e;color:#7aa2f7;border:1px solid #7aa2f7;padding:8px 14px;margin:4px;cursor:pointer;border-radius:4px}}
button:hover{{background:#2a2a4e}}
#log{{white-space:pre-wrap;margin-top:16px;font-size:13px}}
.done{{color:#9ece6a}} .rejected{{color:#f7768e}} .running{{color:#7aa2f7}}
h3{{color:#7aa2f7}}
</style></head><body>
<h2>HoReCa пайп — исполнение сценариев</h2>
<div>{scenarios}</div>
<pre id="log"></pre>
<script>
function run(name){{fetch('/api/run/'+name,{{method:'POST'}}).then(r=>r.json()).then(d=>{{log('▶ '+name+': '+d.status);poll(name)}})}}
function log(s){{document.getElementById('log').textContent=s}}
function poll(name){{
  fetch('/api/status/'+name).then(r=>r.json()).then(d=>{{
    let l=document.getElementById('log');
    l.textContent='=== '+name+' ['+d.status+'] ===\n'+d.lines.join('\n');
    if(d.status==='running')setTimeout(()=>poll(name),1500);
  }})}}
</script></body></html>"#
    ))
}

// ---------------------------------------------------------------------------
// API
// ---------------------------------------------------------------------------

async fn list_scenarios() -> Json<Value> {
    Json(json!({"scenarios": SCENARIOS}))
}

async fn run_scenario(
    State(shared): State<SharedState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    if !SCENARIOS.contains(&name.as_str()) {
        return Err(StatusCode::NOT_FOUND);
    }
    {
        let m = shared.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(rs) = m.get(&name) {
            if rs.status == "running" {
                return Ok(Json(json!({"status": "already running"})));
            }
        }
    }
    let src = std::fs::read_to_string(format!("{SCENARIOS_DIR}/{name}.luck"))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let graph = parse(&src).map_err(|e| {
        eprintln!("compile {name}: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    shared.lock().unwrap_or_else(|e| e.into_inner()).insert(
        name.clone(),
        RunState {
            status: "running".into(),
            lines: vec![],
            result: String::new(),
            started: Some(Instant::now()),
        },
    );
    let shared2 = shared.clone();
    tokio::task::spawn_blocking(move || {
        let mut graph = graph;
        let transport = FallbackTransport::from_env().unwrap_or_else(|e| {
            eprintln!("runtime err: {e}");
            FallbackTransport::ollama_only()
        });
        let mut backend = ValidatingBackend::new(transport, 3);
        let mut tools = ToolRegistry::new();
        register_demo_tools(&mut tools);
        let mut sched = Scheduler::new(&mut backend, &mut tools);
        let outcome = run_verified(&mut sched, &mut graph, "root");

        let mut m = shared2.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(rs) = m.get_mut(&name) {
            match outcome {
                Ok(VerifiedOutcome::Completed(result)) => {
                    rs.status = "completed".into();
                    rs.lines = result
                        .order
                        .iter()
                        .map(|id| format!("✓ {id}: {}", result.outputs.get(id).cloned().unwrap_or_default()))
                        .collect();
                    rs.result = "ok".into();
                }
                Ok(VerifiedOutcome::Rejected { reason, partial }) => {
                    rs.status = "rejected".into();
                    rs.lines = partial
                        .order
                        .iter()
                        .map(|id| format!("✓ {id}: {}", partial.outputs.get(id).cloned().unwrap_or_default()))
                        .collect();
                    rs.lines.push(format!("ОТКАЗ: {reason}"));
                    rs.result = reason;
                }
                Err(fatal) => {
                    rs.status = "rejected".into();
                    rs.lines.push(format!("ФАТАЛЬНЫЙ СБОЙ: {fatal}"));
                    rs.result = fatal;
                }
            }
        }
    });
    Ok(Json(json!({"status": "started"})))
}

async fn status_scenario(
    State(shared): State<SharedState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let m = shared.lock().unwrap_or_else(|e| e.into_inner());
    let rs = m.get(&name).ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(json!({
        "status": rs.status,
        "lines": rs.lines,
        "elapsed_s": rs.started.map(|t| t.elapsed().as_secs()).unwrap_or(0),
    })))
}
