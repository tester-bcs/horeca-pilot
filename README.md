# horeca-pilot

> СТАТУС: ИССЛЕДОВАНИЕ (research). Проект, ветки и клоны — исследовательская
> разработка, не продукт. Репо: https://github.com/tester-bcs/horeca-pilot

Фабрика работающих бизнес-процессов с AI. Пилот: производство и обеспечение HoReCa
(отели, рестораны, кафе). Для инвестора — см. [MEMORANDUM.md](MEMORANDUM.md).

## Суть

Домен → обследование (1+6) → бизнес-модель → исполнимый план-граф → AI исполняет
с контролем (проверки на каждом шаге: остатки, сроки, деньги). Граф вместо промпта:
порядок и проверки детерминированы, AI наполняет содержанием.

## Статус: v0.2.0 — на каноническом движке Luck

Апстрим Luck консолидировался в единый канонический репозиторий
[tester-bcs/luck](https://github.com/tester-bcs/luck) (язык + Rust-рантайм,
POLICY/VERIFY/BUDGET). `luck-pilot/` больше не самодельный форк с собственным
`plan.json`-IR — движок вендорен в `luck-pilot/vendor/luck-engine/` (read-only,
не модифицируется) и подключён как path-зависимость.

4 сценария исполняются на реальных моделях и полностью проходят граф от входа
до отчёта (не только формально валидны — проверено live-прогоном с ветвлением).
18 тестов зелёные (13 unit + 5 e2e).

## Быстрый старт

```bash
cd luck-pilot
cargo test                                                    # 18 тестов
cargo run --bin validate -- ../examples_luck/horeca-daily-cycle.luck  # валидация

# Живой прогон через OpenRouter (рекомендуется — быстрая недорогая модель,
# без склонности к reasoning-«раздумьям», съедающим token-бюджет)
OPENROUTER_API_KEY=*** OPENROUTER_MODEL=google/gemini-2.5-flash \
cargo run --bin run -- ../examples_luck/horeca-daily-cycle.luck

# Или через локальную Ollama (десктоп, GPU)
OLLAMA_HOST=http://localhost:11434 OLLAMA_MODEL=hermes3:8b \
OLLAMA_ONLY=1 cargo run --bin run -- ../examples_luck/horeca-daily-cycle.luck

# Все 4 сценария разом:
../run_all.sh

# Веб-интерфейс (http://localhost:8080 — список сценариев, запуск, финальный результат;
# live-стриминг по узлам временно недоступен — см. docs/TECH.md «Известные ограничения»)
OLLAMA_HOST=http://localhost:11434 OLLAMA_MODEL=hermes3:8b \
OLLAMA_ONLY=1 cargo run --bin web -- 8080
```

## Структура

```
├── MEMORANDUM.md        # для бизнес-ангела (понятным языком)
├── examples_luck/       # 4 сценария HoReCa (.luck, канонический синтаксис Luck)
├── luck-pilot/          # обвязка пилота: IDEF0-маппер, OpenRouter/Ollama-рантайм,
│                         # бизнес-VERIFY-предикаты HoReCa (verify.rs)
│   └── vendor/luck-engine/  # вендор канонического движка tester-bcs/luck (не трогаем)
├── luck-core/           # устарело — оставлено для сверки, кандидат на удаление
│                         # (см. TODO.md: теперь есть vendor/luck-engine)
├── run_all.sh           # единый живой прогон всех сценариев
└── docs/TECH.md         # технические детали: мост 1+6→Luck, паттерны, предикаты
```

## Политика

Нативные/апстрим-проекты (canonical `tester-bcs/luck`) НЕ модифицируются —
только вендорятся как read-only зависимость в `luck-pilot/vendor/`. Вся
HoReCa-специфика (IDEF0-маппер, бизнес-предикаты, рантаймы) — в `luck-pilot/src/`.

## Ссылки

- Репо: https://github.com/tester-bcs/horeca-pilot (public, open source)
- Апстрим Luck: https://github.com/tester-bcs/luck (канонический язык + Rust-рантайм)
- Технические детали: docs/TECH.md
