//! Luck — реальный инструмент web_search для TOOL-узлов
//! Ветка: Rust-проход (Маркер В.0 -> возврат), практическая демонстрация
//!
//! ЧТО ЭТО И ЧЕГО ОНО НЕ ГАРАНТИРУЕТ. До этого файла TOOL-узлы в
//! run_real.rs могли вызывать только заглушку "notify" — граф не мог
//! реально сходить в интернет. Это первый настоящий внешний инструмент.
//!
//! Реализация — наивный HTML-скрейпинг страницы результатов DuckDuckGo
//! (html.duckduckgo.com/html/, версия без JS, не требует API-ключа).
//! Это НЕ официальный API — разметка страницы может измениться в любой
//! момент и сломать парсинг молча (вернёт пустой список, не ошибку —
//! честно об этом здесь, не в комментарии к вызывающему коду). Для
//! прод-использования нужен настоящий поисковый API (Brave Search,
//! SerpAPI и т.п.) с ключом; здесь выбран путь без ключа сознательно —
//! цель этого файла показать, что TOOL-узел МОЖЕТ реально дотянуться до
//! внешнего мира, не поднять production-грейд интеграцию.

use crate::scheduler::ToolRegistry;
use std::collections::BTreeMap;

/// Регистрирует все "настоящие" инструменты (не заглушки) в одном месте.
/// До этой функции run_real.rs и (позже) luck_mcp.rs дублировали бы три
/// одинаковых `tools.register(...)` — единственный источник истины на
/// набор доступных TOOL-имён, а не копия в каждом бинарнике.
pub fn register_default_tools(tools: &mut ToolRegistry) {
    tools.register("notify", |args: &BTreeMap<String, String>| {
        format!("notified:{args:?}")
    });
    tools.register("web_search", |args: &BTreeMap<String, String>| {
        let query = args.get("query").map(String::as_str).unwrap_or("");
        web_search(query).unwrap_or_else(|e| format!("(ошибка поиска: {e})"))
    });
    tools.register("git_diff", |args: &BTreeMap<String, String>| {
        let path = args.get("path").map(String::as_str).unwrap_or(".");
        git_diff(path).unwrap_or_else(|e| format!("(ошибка git diff: {e})"))
    });
}

// ==================== git_diff ====================
//
// Второй настоящий внешний инструмент — оборачивает `git diff` (рабочее
// дерево против HEAD). Прототип для интеграции с pipegrab/uni-mcp-v2:
// та же команда, что `git_diff_content` в его mcp.rs, здесь — упрощённая
// версия без base/target refs (только рабочее дерево) для первой
// проверки дизайна графа `review_diff`. Расширение до произвольных
// ref-пар — механическая правка, не архитектурная, откладывается
// сознательно до подтверждения, что сам граф-дизайн работает.

const MAX_DIFF_BYTES: usize = 20_000;

pub fn git_diff(repo_path: &str) -> Result<String, String> {
    let path = if repo_path.is_empty() { "." } else { repo_path };
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(path)
        .arg("diff")
        .arg("-U3")
        .arg("HEAD")
        .output()
        .map_err(|e| format!("git_diff: не удалось запустить git: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "git_diff: git завершился с ошибкой: {}",
            stderr.trim()
        ));
    }

    let raw = String::from_utf8_lossy(&output.stdout).to_string();
    if raw.trim().is_empty() {
        return Ok("(рабочее дерево чисто — изменений нет)".to_string());
    }
    if raw.len() > MAX_DIFF_BYTES {
        let truncated: String = raw.chars().take(MAX_DIFF_BYTES).collect();
        Ok(format!(
            "{truncated}\n\n… (diff обрезан на {MAX_DIFF_BYTES} байт из {})",
            raw.len()
        ))
    } else {
        Ok(raw)
    }
}

const MAX_RESULTS: usize = 5;

pub fn web_search(query: &str) -> Result<String, String> {
    let url = format!(
        "https://html.duckduckgo.com/html/?q={}",
        percent_encode(query)
    );
    let body = ureq::get(&url)
        .set("user-agent", "Mozilla/5.0 (compatible; luck-rs-demo/0.1)")
        .timeout(std::time::Duration::from_secs(20))
        .call()
        .map_err(|e| format!("web_search: сетевая ошибка: {e}"))?
        .into_string()
        .map_err(|e| format!("web_search: не удалось прочитать тело ответа: {e}"))?;

    let results = extract_results(&body, MAX_RESULTS);
    if results.is_empty() {
        return Ok(
            "(поиск не вернул результатов — либо запрос ничего не нашёл, \
                    либо разметка DuckDuckGo изменилась и наивный парсер сломался)"
                .to_string(),
        );
    }

    let mut out = String::new();
    for (i, (title, snippet)) in results.iter().enumerate() {
        out.push_str(&format!("{}. {title} — {snippet}\n", i + 1));
    }
    Ok(out)
}

/// Наивное извлечение (title, snippet) пар из HTML DuckDuckGo. Держится
/// на классах `result__a` (заголовок-ссылка) и `result__snippet`
/// (описание) — если DDG переверстает страницу, это сломается молча
/// (см. докстринг файла). Не парсер HTML вообще, а точечный grep под
/// конкретную известную разметку — намеренно, чтобы не тащить крейт
/// HTML-парсера ради одной демонстрационной интеграции.
fn extract_results(html: &str, max: usize) -> Vec<(String, String)> {
    let mut results = Vec::new();
    let mut rest = html;
    while results.len() < max {
        let Some(title_start) = rest.find("result__a\"") else {
            break;
        };
        let after_class = &rest[title_start..];
        let Some(gt) = after_class.find('>') else {
            break;
        };
        let after_gt = &after_class[gt + 1..];
        let Some(close) = after_gt.find("</a>") else {
            break;
        };
        let title = clean_html_text(&after_gt[..close]);
        let tail = &after_gt[close + "</a>".len()..];

        let snippet = tail
            .find("result__snippet")
            .and_then(|snip_start| tail[snip_start..].find('>').map(|gt2| (snip_start, gt2)))
            .and_then(|(snip_start, gt2)| {
                let snip_html = &tail[snip_start + gt2 + 1..];
                snip_html
                    .find("</a>")
                    .map(|c| clean_html_text(&snip_html[..c]))
            })
            .unwrap_or_default();

        if !title.is_empty() {
            results.push((title, snippet));
        }
        rest = tail;
    }
    results
}

fn clean_html_text(s: &str) -> String {
    let no_tags = {
        let mut out = String::new();
        let mut in_tag = false;
        for c in s.chars() {
            match c {
                '<' => in_tag = true,
                '>' => in_tag = false,
                _ if !in_tag => out.push(c),
                _ => {}
            }
        }
        out
    };
    no_tags
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .trim()
        .to_string()
}

fn percent_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Настоящий временный git-репозиторий, не мок — тот же принцип, что
    /// у web_search-тестов ниже (проверяем реальное поведение внешнего
    /// инструмента, не наши предположения о нём).
    fn init_repo_with_diff(dir: &std::path::Path, has_diff: bool) {
        let run = |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .output()
                .expect("git доступен в тестовом окружении")
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "test@test"]);
        run(&["config", "user.name", "test"]);
        std::fs::write(dir.join("f.txt"), "line1\n").unwrap();
        run(&["add", "-A"]);
        run(&["commit", "-q", "-m", "init"]);
        if has_diff {
            std::fs::write(dir.join("f.txt"), "line1\nline2\n").unwrap();
        }
    }

    #[test]
    fn git_diff_clean_tree_reports_no_changes() {
        let dir = std::env::temp_dir().join(format!("luck_git_diff_clean_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        init_repo_with_diff(&dir, false);
        let out = git_diff(dir.to_str().unwrap()).unwrap();
        assert!(
            out.contains("нет"),
            "чистое дерево -> сообщение об отсутствии изменений: {out}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn git_diff_dirty_tree_shows_real_diff() {
        let dir = std::env::temp_dir().join(format!("luck_git_diff_dirty_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        init_repo_with_diff(&dir, true);
        let out = git_diff(dir.to_str().unwrap()).unwrap();
        assert!(
            out.contains("+line2"),
            "реальное изменение видно в выводе: {out}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn git_diff_nonexistent_path_yields_err_not_panic() {
        let result = git_diff("/nonexistent/path/luck_test_xyz");
        assert!(result.is_err());
    }

    #[test]
    fn percent_encode_spaces_and_specials() {
        assert_eq!(percent_encode("a b"), "a+b");
        assert_eq!(percent_encode("safe-name_1.2~x"), "safe-name_1.2~x");
        assert_eq!(percent_encode("SSRF & test"), "SSRF+%26+test");
    }

    /// Найдено на практике: разные вызовы одного и того же запроса могут
    /// вернуть разные топ-5 ссылок (живой поисковик, не детерминированный
    /// стаб) — парсер обязан извлекать title+snippet из ЛЮБОЙ страницы
    /// с этой структурой блоков, не только из конкретного снапшота HTML,
    /// снятого во время разработки.
    #[test]
    fn extract_results_from_minimal_ddg_like_html() {
        let html = r#"
        <div class="result">
          <a class="result__a" href="https://example.com/a">First &amp; Title</a>
          <a class="result__snippet">First snippet text.</a>
        </div>
        <div class="result">
          <a class="result__a" href="https://example.com/b">Second Title</a>
          <a class="result__snippet">Second snippet.</a>
        </div>
        "#;
        let results = extract_results(html, 5);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, "First & Title");
        assert_eq!(results[0].1, "First snippet text.");
        assert_eq!(results[1].0, "Second Title");
        assert_eq!(results[1].1, "Second snippet.");
    }

    #[test]
    fn extract_results_respects_max_limit() {
        let mut html = String::new();
        for i in 0..10 {
            html.push_str(&format!(
                r#"<a class="result__a" href="https://x">Title {i}</a><a class="result__snippet">Snippet {i}</a>"#
            ));
        }
        let results = extract_results(&html, 3);
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn extract_results_empty_html_yields_empty_not_panic() {
        assert!(extract_results("<html><body>no results here</body></html>", 5).is_empty());
        assert!(extract_results("", 5).is_empty());
    }

    #[test]
    fn clean_html_text_strips_tags_and_unescapes_entities() {
        assert_eq!(clean_html_text("<b>bold</b> &amp; plain"), "bold & plain");
        assert_eq!(clean_html_text("  padded  "), "padded");
    }
}
