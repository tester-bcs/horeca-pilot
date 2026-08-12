//! Валидатор .luck-файла (CLI) — инструмент пайпа инкубатора.
//! Использование: cargo run --bin validate -- <file.luck>
use luck_engine::parser::parse;

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
    match parse(&src) {
        Ok(graph) => {
            println!("OK: {} nodes, {} edges", graph.nodes.len(), graph.edges.len());
            for (id, n) in &graph.nodes {
                println!("  node {id} kind={:?} subtype={:?}", n.kind, n.subtype);
            }
            for e in &graph.edges {
                println!(
                    "  edge {} -> {} type={:?} label={:?}",
                    e.source, e.target, e.edge_type, e.label
                );
            }
        }
        Err(e) => {
            println!("FAIL: {e}");
            std::process::exit(1);
        }
    }
}
