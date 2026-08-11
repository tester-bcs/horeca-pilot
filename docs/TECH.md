# Технические детали horeca-pilot

Архитектура: форк ai-agent-порта Luck (luck-pilot) — автономный крейт
(компилятор .luck → план, валидатор, планировщик, рантаймы OpenRouter/Ollama).
Сценарии: examples_luck/*.luck (синтаксис .luck — см. скилл luck-language).

## Мост «1+6 → Luck» (карта соответствия)

| Срез 1+6 | Конструкт Luck | Как |
|---|---|---|
| 1. Инвариант | ЦЕЛЬ пайпа + VERIFY на критичных рёбрах | Инвариант = то, что пайп обязан не порвать; пороги-предупреждения (Срез 5) = VERIFY-предикаты ДО разрыва |
| 2. Декомпозиция | Модули A1..AN → подграфы (SPAWN-вложенность) | Каждый модуль = свой .luck-файл/подграф; сборка = план верхнего уровня |
| 3. Связи | EDGES с типами потоков | Материальный/денежный/информационный поток = ребро с меткой типа |
| 4. Форма | Слоты INTO/INPUT + VERIFY file_exists/not_empty | Документ = форма потока; честность = VERIFY «документ существует и заполнен» |
| 5. Динамика | CLASSIFY → BRANCHES, ON_FAIL, REJECT | Состояния = узлы-классификаторы; отказы = REJECT_MARK (не исключение); ритмы = планировщик (daily/weekly) |
| 6. Границы | Вход/выход графа | Узлы на границе = intake (вход) / report (выход); слабые места = ON_FAIL-обработчики |

## Маппинг VERIFY (Срез 4 + Срез 5 → предикаты)

| Проверка | Предикат (реестр) | Инвариант |
|---|---|---|
| Документ создан (заказ, ТТН, счёт) | file_exists | И1/И4 — форма честна |
| Документ заполнен | not_empty | И4 — форма честна |
| Остаток ≥ потребность | stock_level — {stock, need} | И1 — сырьё не кончилось |
| Комплектация = заказ | order_match — {ordered, picked} | И4 — полнота |
| Кэш-прогноз ≥ обязательства | cash_ok — {cash, obligations} | И2 — нет кассового разрыва |
| Срок годности > горизонт | shelf_life_ok — {expires, horizon}, ISO-даты | И3 — скоропорт не портится |
| Температурный лог в норме | temp_log_ok — {temp, max} | И3 — режим хранения |

РЕАЛИЗОВАНО (2026-08-11, в luck-pilot — нативные не тронуты): бизнес-предикаты
добавлены в luck_plan.rs (KNOWN_PREDICATES) и luck_scheduler.rs (чистые функции +
нормализация JSON-строк через ground_value). 34 теста зелёные, включая E2E:
TOOL-узел возвращает JSON-строку {"stock": N, "need": M} → VERIFY stock_level
распарсивает и проверяет. Формат: предикаты ждут JSON-объект с полями (см. таблицу).

## Параметры пилота (Срез 6 — данные предприятия, слоты плана)

- Страховой запас сырья: 2 дня (начальное значение)
- Порог перезаказа: остаток < потребность плана (CLASSIFY в stock_check)
- Окно доставки: утро, до 10:00 (И4 — зал клиента не ждёт)
- Отсрочка клиента: 7–30 дней (дебиторка — главный риск И2)
- OTIF-таргет: ≥ 95% (главный KPI A7)

## Сценарии (все валидны, проверено компилятором luck-pilot)

1. horeca-daily-cycle.luck — дневной цикл (10 узлов/10 рёбер): intake → plan →
   stock_check → fork {ok→produce | short→purchase→produce} → pick → verify_full →
   dispatch → bill → report. И1–И4, оба контура.
2. horeca-returns.luck — возвраты/рекламации (10/16): обратный поток (Срез 5),
   4 ветки по причине (брак/доставка/просрочка/пересорт) → settle → отчёт.
3. horeca-inventory.luck — инвентаризация (9/11): freeze → count → compare →
   fork {ok→write_off | deviation→investigate→resolve} → close → отчёт (ритм неделя).
4. horeca-cashflow.luck — cash-прогноз и кредитный контроль A6 (10/12):
   receivables → forecast → VERIFY cash_ok → fork {ok→pay | risk→hold} +
   credit_api → VERIFY credit_ok → settle_cash → отчёт (И2, дебиторка 7–30 дней).

ПАТТЕРН ВЕТВЛЕНИЯ (важно, обожжено на сценарии №1): ветвление исполняет только
ОТДЕЛЬНЫЙ узел `fork: BRANCH` (INPUT + BRANCHES label=target), а CLASSIFY сам
ветки не выбирает — он лишь пишет метку в INTO. Рёбра: fork -> target [label].
Без отдельного BRANCH-узла сценарий компилируется, но ветки не активируются.

ПАТТЕРН VERIFY vs BRANCH (обожжено на сценарии №4): VERIFY-предикаты ждут JSON
({cash, obligations}, {limit, outstanding}...), а BRANCH-узел ждёт МЕТКУ (ok/risk/...).
Нельзя писать JSON в input fork'а — он не найдёт метку среди веток. Решение:
источник JSON (для VERIFY) и источник метки (для BRANCH) — РАЗНЫЕ узлы:
STEP forecast -> INTO cash_forecast (JSON) -> VERIFY cash_ok; затем отдельный
CLASSIFY classify_risk -> INTO risk_state (метка) -> fork_cash: BRANCH INPUT risk_state.

РЕЕСТР VERIFY-ПРЕДИКАТОВ (6 бизнес-предикатов, 40 тестов зелёные: 35 юнит + 5 E2E):
stock_level {stock,need}, order_match {ordered,picked}, cash_ok {cash,obligations},
shelf_life_ok {expires,horizon} ISO-даты, temp_log_ok {temp,max}, credit_ok {limit,outstanding}.
E2E-харнесс (tests/e2e_horeca.rs): все 4 сценария исполняются через PlanExecutor
с HorecaRuntime (реалистичные ответы узлов); include_str! подхватывает правки
сценариев автоматически; негативный кейс: cash_ok Rejected при кассовом разрыве.

## Как запустить

```bash
cd luck-pilot
cargo test                                               # 35 тестов
cargo run --bin validate -- ../examples_luck/horeca-daily-cycle.luck  # валидация сценария
```

## Открытые вопросы

1. VERIFY subject — задаётся внутри слота: `VERIFY not_empty INTO picked_order`
   (одной строкой); отдельный слот INTO после VERIFY → MissingSubject (обожжено, исправлено).
2. Предикаты stock_level/order_match/cash_ok — расширение реестра VERIFY в luck-pilot.
3. SPAWN-вложенность: модуль A6 (деньги) как вложенный план — ждёт фичи SPAWN.
4. Синхронизация с нативными: luck-pilot — самостоятельный форк, НЕ подтягивает
   изменения из ai-agent (намеренно, чтобы нативные оставались в покое).
5. luck-core (reference) — оставить для сверки или удалить?
