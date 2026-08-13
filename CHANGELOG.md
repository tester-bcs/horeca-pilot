# Changelog

## Unreleased
### Добавлено
- Политика синка с апстримом `tester-bcs/luck`: пин коммита в
  `luck-pilot/vendor/VENDOR.md`, проверка `scripts/check-luck-upstream.sh`
  (сверяет пин с последним коммитом апстрима, затрагивающим `rust/`).
- SPAWN-вложенность опробована на A6 (`horeca-cashflow.luck`, `settle_plan`) —
  структурно работает, но качество ответов страдает от эхо-бага вендора;
  задокументировано как известное ограничение (docs/TECH.md).

### Удалено
- `luck-core/` (старый reference-форк канонического крейта) — дублировал
  `luck-pilot/vendor/luck-engine/`, теперь единственный источник правды.

## v0.2.0 — 2026-08-12
Переход `luck-pilot` на канонический движок Luck ([tester-bcs/luck](https://github.com/tester-bcs/luck) —
консолидированный апстрим: Python `luck-lang` архивирован и слит в него вместе
с Rust-рантаймом).

### Изменено
- Движок вендорен в `luck-pilot/vendor/luck-engine/` (read-only, path-зависимость).
- Удалены `luck_plan.rs`/`luck_compile.rs`/`luck_scheduler.rs` — их роль (парсинг,
  валидация, планировщик) теперь у вендора.
- `idef0.rs`, `openrouter.rs`, `src/bin/{run,validate,web}.rs`, `tests/e2e_horeca.rs`
  переписаны под канонические типы (`IntentGraph`/`Node`, `Scheduler`, `ModelBackend`).
- Все 4 `.luck`-сценария переведены на канонический синтаксис: `BRANCH` →
  branch-рёбра `=>` с меткой, `MERGE` → merge-рёбра `~>`, `VERIFY`/`POLICY`/`BUDGET`
  → слоты вместо узлов.
- Бизнес-VERIFY-предикаты HoReCa перенесены в новый `src/verify.rs`
  (`run_verified` — пост-хок enforcement поверх вендорского `Scheduler`).

### Исправлено
- Во всех 4 сценариях branch-рёбра ошибочно ссылались на алиасы `LABELS`
  (`[ok]`) вместо фактического текстового вывода модели (`[хватает]`) — графы
  молча обрывались на 2–3 узлах, не доходя до конца, при этом печатая `ГОТОВО`.
  Обнаружено только на live-прогоне (не ловится тестами на моках). Значения
  `LABELS` сделаны однословными токенами (лексер не допускает пробелы в метке
  ребра), метки рёбер синхронизированы.
- Таймаут OpenRouter-транспорта 30с → 180с, `max_tokens` 512 → 2000 — не
  хватало на медленные/reasoning-модели (nemotron-3-super).

### Тесты
- 18 зелёных: 13 unit + 5 e2e.
- Live-прогон (`google/gemini-2.5-flash` через OpenRouter): все 4 сценария
  проходят граф целиком, включая ветвление и слияние.

### Известные ограничения
- `web.rs` временно без live-стриминга прогресса по узлам (вендор не даёт event-хуков).
- VERIFY — пост-хок, не прерывает граф посреди прогона.

## v0.1.0 — 2026-08-11
Первый стабильный прототип пайпа бизнес-инкубатора (пилот HoReCa).

### Добавлено
- horeca-daily-cycle.luck — сценарий №1: дневной цикл (10 узлов, 10 рёбер), И1–И4.
- horeca-returns.luck — сценарий №2: возвраты/рекламации, 4 ветки по причине.
- horeca-inventory.luck — сценарий №3: инвентаризация, ветки ok/deviation.
- horeca-cashflow.luck — сценарий №4: cash-прогноз + кредитный контроль (A6).
- luck-pilot/ — автономный форк ai-agent-порта Luck (luck_plan + luck_compile + luck_scheduler).
- luck-core/ — reference-форк канонического Rust-крейта (для сверки расхождений).
- VERIFY-предикаты HoReCa: stock_level, order_match, cash_ok, shelf_life_ok,
  temp_log_ok, credit_ok (реестр в luck_plan.rs + функции в luck_scheduler.rs).
- bin/validate.rs — CLI-валидатор .luck.
- tests/e2e_horeca.rs — E2E-исполнение всех сценариев (HorecaRuntime, include_str!).
- README.md — мост «1+6 → Luck», политика (нативные проекты не трогаем), паттерны.

### Исправлено
- Сценарий №1: ветвление через отдельный BRANCH-узел fork (CLASSIFY ветки не выбирает).
- Сценарии №3–4: JSON (для VERIFY) и метка (для BRANCH) — разные узлы.

### Тесты
- 40 зелёных: 35 юнит + 5 E2E (включая негатив: cash_ok → Rejected).
