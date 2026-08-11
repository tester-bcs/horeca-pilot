//! Веб-интерфейс пайпа: список сценариев -> запуск -> прогресс узлов -> документы.
//! Использование:
//!   OLLAMA_HOST=http://100.64.0.1:11434 OLLAMA_MODEL=hermes3:8b OLLAMA_ONLY=1 \
//!     cargo run --bin web -- [порт]
//! Открыть http://localhost:8080

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Html,
    routing::{get, post},
    Json, Router,
};
use luck_pilot::{
    compile,
    luck_scheduler::{PlanEvent, PlanExecutor, PlanOutcome},
    openrouter::FallbackRuntime,
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
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
    events: Vec<(String, String, bool)>, // (node_id, kind, ok)
    store: HashMap<String, Value>,
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
.node-ok{{color:#9ece6a}} .node-fail{{color:#f7768e}} .node-run{{color:#7aa2f7}}
.done{{color:#9ece6a}} .rejected{{color:#f7768e}} .running{{color:#7aa2f7}}
h3{{color:#7aa2f7}}
</style></head><body>
<h2>HoReCa пайп — исполнение сценариев</h2>
<div>{scenarios}</div>
<pre id="log"></pre>
<script>
function run(name){{fetch('/api/run/'+name,{{method:'POST'}}).then(r=>r.json()).then(d=>{{log('▶ '+name+': '+d.status);poll(name)}})}}
function poll(name){{
  fetch('/api/status/'+name).then(r=>r.json()).then(d=>{{
    let l=document.getElementById('log');
    l.textContent='=== '+name+' ['+d.status+'] ===\n'+d.lines.join('\n')+'\n\n'+d.store;
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
    // Уже идёт?
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
    let plan = compile(&src).map_err(|e| {
        eprintln!("compile {name}: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    shared.lock().unwrap_or_else(|e| e.into_inner()).insert(
        name.clone(),
        RunState {
            status: "running".into(),
            events: vec![],
            store: HashMap::new(),
            result: String::new(),
            started: Some(Instant::now()),
        },
    );
    let shared2 = shared.clone();
    tokio::spawn(async move {
        let rt = Arc::new(FallbackRuntime::from_env().unwrap_or_else(|e| {
            eprintln!("runtime err: {e}");
            FallbackRuntime::ollama_only()
        }));
        let (tx, mut rx) = tokio::sync::mpsc::channel::<PlanEvent>(32);
        let mut ex = PlanExecutor::with_events(rt, tx);
        // приём событий
        let ev_shared = shared2.clone();
        let ev_name = name.clone();
        let ev_task = tokio::spawn(async move {
            while let Some(ev) = rx.recv().await {
                let mut m = ev_shared.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(rs) = m.get_mut(&ev_name) {
                    match ev {
                        PlanEvent::NodeStart { id } => {
                            rs.events.push((id, "start".into(), true));
                        }
                        PlanEvent::NodeDone { id, ok } => {
                            rs.events.push((id, if ok { "done" } else { "fail" }.into(), ok));
                        }
                        PlanEvent::Rejected { reason } => {
                            rs.status = "rejected".into();
                            rs.result = reason;
                        }
                        PlanEvent::Completed => {
                            rs.status = "completed".into();
                        }
                    }
                }
            }
        });
        let outcome = ex.run(&plan).await;
        // store после завершения
        {
            let mut m = shared2.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(rs) = m.get_mut(&name) {
                for (k, v) in ex.store() {
                    rs.store.insert(k.clone(), v.clone());
                }
                match outcome {
                    PlanOutcome::Completed { result } => {
                        rs.status = "completed".into();
                        rs.result = result.to_string();
                    }
                    PlanOutcome::Rejected { reason } => {
                        rs.status = "rejected".into();
                        rs.result = reason;
                    }
                }
            }
        }
        ev_task.await.ok();
    });
    Ok(Json(json!({"status": "started"})))
}

async fn status_scenario(
    State(shared): State<SharedState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let m = shared.lock().unwrap_or_else(|e| e.into_inner());
    let rs = m.get(&name).ok_or(StatusCode::NOT_FOUND)?;
    let mut lines: Vec<String> = Vec::new();
    let mut store_lines: Vec<String> = Vec::new();
    for (id, kind, ok) in &rs.events {
        let icon = match kind.as_str() {
            "start" => "▶",
            "done" => "✓",
            "fail" => "✗",
            _ => "•",
        };
        lines.push(format!("{icon} {id} ({kind})"));
    }
    if !rs.result.is_empty() {
        let r = &rs.result;
        let r = if r.chars().count() > 200 {
            let t: String = r.chars().take(200).collect();
            format!("{t}…")
        } else {
            r.clone()
        };
        lines.push(format!("result: {r}"));
    }
    for (k, v) in &rs.store {
        let s = v.to_string();
        let s = if s.chars().count() > 150 {
            let t: String = s.chars().take(150).collect();
            format!("{t}…")
        } else {
            s
        };
        store_lines.push(format!("{k}: {s}"));
    }
    Ok(Json(json!({
        "status": rs.status,
        "lines": lines,
        "store": store_lines.join("\n"),
        "elapsed_s": rs.started.map(|t| t.elapsed().as_secs()).unwrap_or(0),
    })))
}
