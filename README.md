# horeca-pilot

Фабрика работающих бизнес-процессов с AI. Пилот: производство и обеспечение HoReCa
(отели, рестораны, кафе). Для инвестора — см. [MEMORANDUM.md](MEMORANDUM.md).

## Суть

Домен → обследование (1+6) → бизнес-модель → исполнимый план-граф → AI исполняет
с контролем (проверки на каждом шаге: остатки, сроки, деньги). Граф вместо промпта:
порядок и проверки детерминированы, AI наполняет содержанием.

## Статус: v0.1.0 — работает вживую

4 сценария исполняются на реальной модели (hermes3:8b, RTX 5060 Ti):
дневной цикл, возвраты, инвентаризация, деньги. 40 тестов зелёные.

## Быстрый старт

```bash
cd luck-pilot
cargo test                                                    # 40 тестов
cargo run --bin validate -- ../examples_luck/horeca-daily-cycle.luck  # валидация

# Живой прогон через Ollama на десктопе (GPU)
OLLAMA_HOST=http://100.64.0.1:11434 OLLAMA_MODEL=hermes3:8b \
OLLAMA_ONLY=1 cargo run --bin run -- ../examples_luck/horeca-daily-cycle.luck

# Или все 4 сценария разом:
../run_all.sh
```

## Структура

```
├── MEMORANDUM.md        # для бизнес-ангела (понятным языком)
├── examples_luck/       # 4 сценария HoReCa (.luck)
├── luck-pilot/          # движок: компилятор + планировщик + рантаймы (форк Luck)
├── luck-core/           # reference: канонический Rust-крейт Luck (для сверки)
├── run_all.sh           # единый живой прогон всех сценариев
└── docs/TECH.md         # технические детали: мост 1+6→Luck, паттерны, предикаты
```

## Политика

Нативные проекты (luck-репо Python, luck-repo Rust, ai-agent) НЕ модифицируются —
всё необходимое форкнуто в luck-pilot/.

## Ссылки

- Репо: https://github.com/tester-bcs/horeca-pilot (private)
- Технические детали: docs/TECH.md
- Скилл: luck-language (паттерны, питфоллы, живые рантаймы)
