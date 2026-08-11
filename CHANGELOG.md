# Changelog

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
