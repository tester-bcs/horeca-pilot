//! Интервью-агент (CLI): обследование предприятия разговором → IDEF0-модель.
//!
//! Использование:
//!   cargo run --bin interview -- [куда-сохранить.json]
//!
//! Транспорт как у `run`: OPENROUTER_API_KEY (+ OPENROUTER_MODEL) либо
//! OLLAMA_ONLY=1 с OLLAMA_HOST/OLLAMA_MODEL.
//!
//! Вопросы задаёт код (детерминированно, по нарушениям валидатора), LLM только
//! превращает свободный ответ клиента в структуру — см. `interview.rs`.
use luck_pilot::interview::Interview;
use luck_pilot::openrouter::FallbackTransport;
use std::io::{self, BufRead, Write};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let out_path = args.get(1).cloned().unwrap_or_else(|| "model.json".to_string());

    let mut transport = if std::env::var("OLLAMA_ONLY").is_ok() {
        eprintln!("== режим: только Ollama ==");
        FallbackTransport::ollama_only()
    } else {
        match FallbackTransport::from_env() {
            Ok(t) => t,
            Err(e) => {
                eprintln!("{e}");
                eprintln!("подсказка: OLLAMA_ONLY=1 для работы без OpenRouter");
                std::process::exit(2);
            }
        }
    };

    println!("Обследование. Отвечайте своими словами; пустая строка — выход.\n");
    let mut iv = Interview::new();
    let stdin = io::stdin();

    loop {
        let question = iv.question();
        println!("\n> {}", question.text());
        if question.is_final() {
            break;
        }
        print!("  ");
        let _ = io::stdout().flush();

        let mut line = String::new();
        if stdin.lock().read_line(&mut line).unwrap_or(0) == 0 {
            eprintln!("\nввод закончился — обследование прервано");
            break;
        }
        let answer = line.trim();
        if answer.is_empty() {
            eprintln!("\nобследование прервано");
            break;
        }

        // Ошибка разбора не роняет обследование: модель остаётся прежней,
        // вопрос задаётся снова (см. откат ухудшающей правки в interview.rs).
        if let Err(e) = iv.answer(&mut transport, answer) {
            eprintln!("  [не удалось учесть ответ] {e}");
        }
    }

    match iv.finished_model() {
        Some(model) => {
            let json = serde_json::to_string_pretty(model).expect("модель сериализуется");
            match std::fs::write(&out_path, &json) {
                Ok(()) => {
                    println!("\nМодель сохранена: {out_path}");
                    println!("Проверить:  cargo run --bin validate -- {out_path}");
                }
                Err(e) => {
                    eprintln!("не удалось записать {out_path}: {e}");
                    println!("{json}");
                    std::process::exit(1);
                }
            }
        }
        None => {
            eprintln!("\nМодель не достроена — сохранять нечего.");
            std::process::exit(1);
        }
    }
}
