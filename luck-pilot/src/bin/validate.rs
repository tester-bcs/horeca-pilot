//! Валидатор .luck-файла (CLI) — инструмент пайпа инкубатора.
//! Использование: cargo run --bin validate -- <file.luck>
fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: validate <file.luck>");
        std::process::exit(2);
    }
    let src = match std::fs::read_to_string(&args[1]) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("read error: {e}");
            std::process::exit(2);
        }
    };
    match luck_pilot::compile(&src) {
        Ok(plan) => {
            println!("OK: {} nodes, {} edges", plan.nodes.len(), plan.edges.len());
            for n in &plan.nodes {
                println!("  node {} kind={:?}", n.id, n.kind);
            }
            for e in &plan.edges {
                println!("  edge {} -> {} type={:?} label={:?}", e.from, e.to, e.edge_type, e.label);
            }
        }
        Err(e) => {
            println!("FAIL: {e}");
            std::process::exit(1);
        }
    }
}
