//! Luck — реальный транспорт Anthropic Messages API + ValidatingBackend
//! Ветка: Rust-проход (Маркер В.0 -> возврат)
//!
//! Зеркалит luck/backends.py (anthropic_chat + ValidatingBackend) —
//! перенесено сюда полностью, включая находки эмпирики
//! (docs/EMPIRICAL_REAL_MODEL.md), не переоткрыто заново:
//!   - max_tokens=4096, не 1024 (раздел 16: reasoning-модели тратят
//!     бюджет на thinking-блок раньше содержательного ответа);
//!   - инструкция явно разделяет "правило для проверки" и "текст для
//!     выдачи" (раздел 7);
//!   - два few-shot примера для SPAWN, не один (раздел 9-10 — один
//!     пример заякоривал ключевые слова, не только форму);
//!   - вырожденные грамматики вычисляются без вызова модели
//!     (registry::singleton_output, уже перенесён в decoding.rs).
//!
//! Ранее (Срез 5, decoding.rs) этот транспорт был осознанно вне объёма
//! Rust-прохода — вопросы MARKER_V0_RUST.md не требовали повторного
//! прогона на реальной модели. Здесь он добавлен не для ответа на
//! архитектурный вопрос, а для практической демонстрации: пользователь
//! хочет реально прогнать пайплайн на своей машине через настоящий API.

use crate::decoding::{self, Constraint};
use crate::parser::SlotData;
use crate::registry::Kind;
use crate::scheduler::{InvalidModelOutput, ModelBackend};
use std::collections::BTreeMap;
use std::time::Duration;

/// Абстракция транспорта, которую ValidatingBackend умеет обернуть.
/// Выделена не для AnthropicTransport конкретно, а для ЛЮБОГО чат-
/// клиента, который умеет получить текст на текст (плюс собственные
/// ретраи transport-уровня — они НЕ часть контракта этого трейта, у
/// каждой реализации свои). Найдено практической нуждой: у стороннего
/// приложения (pipegrab/uni-mcp-v2) уже есть свой настроенный LLM-клиент
/// (crate::llm::LlmClient, Anthropic или OpenAI-совместимый) — заставлять
/// его дополнительно настраивать ОТДЕЛЬНЫЙ ключ специально под Luck
/// значило бы дублировать конфигурацию ради того, что уже есть. Реализуй
/// этот трейт для своего транспорта — вся логика ValidatingBackend
/// (грамматика, few-shot, повторы по содержимому) переиспользуется без
/// единой правки.
pub trait ChatTransport {
    fn call(&mut self, prompt: &str) -> Result<String, TransportError>;
    fn call_count(&self) -> u64;
}

/// Транспорт — тонкая обёртка HTTP-вызова. Отделён от ValidatingBackend
/// той же причиной, что в Python anthropic_chat() отделена от
/// ValidatingBackend: транспорт не знает о грамматиках/повторах-по-
/// содержимому, только шлёт текст и получает текст (плюс свои,
/// транспортные, сетевые ретраи — другой уровень, см. ValidatingBackend
/// ниже про два РАЗНЫХ смысла "повтора").
pub struct AnthropicTransport {
    api_key: String,
    model: String,
    base_url: String,
    max_tokens: u32,
    timeout: Duration,
    network_retries: u32,
    agent: ureq::Agent,
    pub calls: u64,
}

impl AnthropicTransport {
    pub fn new(api_key: String, model: String) -> Self {
        AnthropicTransport {
            api_key,
            model,
            base_url: "https://api.anthropic.com".to_string(),
            max_tokens: 4096,
            timeout: Duration::from_secs(60),
            network_retries: 3,
            agent: ureq::AgentBuilder::new().build(),
            calls: 0,
        }
    }

    pub fn with_base_url(mut self, url: String) -> Self {
        self.base_url = url;
        self
    }

    pub fn with_max_tokens(mut self, n: u32) -> Self {
        self.max_tokens = n;
        self
    }
}

impl ChatTransport for AnthropicTransport {
    fn call_count(&self) -> u64 {
        self.calls
    }

    fn call(&mut self, prompt: &str) -> Result<String, TransportError> {
        let url = format!("{}/v1/messages", self.base_url.trim_end_matches('/'));
        let body = serde_json::json!({
            "model": self.model,
            "max_tokens": self.max_tokens,
            "messages": [{"role": "user", "content": prompt}],
        });

        let mut last_err = TransportError::Transient("нет попыток".to_string());
        for attempt in 1..=self.network_retries {
            self.calls += 1;
            let result = self
                .agent
                .post(&url)
                .set("content-type", "application/json")
                .set("x-api-key", &self.api_key)
                .set("anthropic-version", "2023-06-01")
                .timeout(self.timeout)
                .send_json(body.clone());

            match result {
                Ok(resp) => {
                    let data: serde_json::Value = resp.into_json().map_err(|e| {
                        TransportError::Fatal(format!("не удалось разобрать JSON-ответ: {e}"))
                    })?;
                    let text = data["content"]
                        .as_array()
                        .map(|blocks| {
                            blocks
                                .iter()
                                .filter_map(|b| b["text"].as_str())
                                .collect::<String>()
                        })
                        .unwrap_or_default();
                    return Ok(text);
                }
                Err(ureq::Error::Status(code, resp)) => {
                    let body_text = resp.into_string().unwrap_or_default();
                    // 4xx (кроме 429 — rate limit, тот СПАСАЕТ повтор) —
                    // клиентская ошибка запроса (неверный ключ, невалидное
                    // тело, устаревшая модель). Повтор того же запроса
                    // получит тот же код: ретраить бессмысленно, это не
                    // сбой сети и не провал грамматики модели — категория,
                    // которую нельзя маскировать под reject(SYNTAX), не
                    // обманывая читающего результат (см. докстринг модуля).
                    if (400..500).contains(&code) && code != 429 {
                        return Err(TransportError::Fatal(format!(
                            "HTTP {code} от {url}: {body_text}\n\
                             Частые причины: неверный/пустой ANTHROPIC_API_KEY \
                             (401), недостаточно прав или лимит исчерпан (403), \
                             неизвестная модель (404), некорректный запрос (400)."
                        )));
                    }
                    last_err = TransportError::Transient(format!("HTTP {code}: {body_text}"));
                    if attempt < self.network_retries {
                        std::thread::sleep(Duration::from_secs(2 * attempt as u64));
                    }
                }
                Err(e) => {
                    last_err = TransportError::Transient(e.to_string());
                    if attempt < self.network_retries {
                        std::thread::sleep(Duration::from_secs(2 * attempt as u64));
                    }
                }
            }
        }
        Err(last_err)
    }
}

/// Различие, которого не было до находки на реальном прогоне (401 с
/// плейсхолдер-ключом): не всякий сбой транспорта СТОИТ того же, что
/// невалидный вывод модели. Fatal — повтор в принципе не поможет
/// (неверный ключ, битый JSON-ответ); Transient — временный сбой сети,
/// тот класс, для которого reject(SYNTAX)-обёртка ValidatingBackend
/// изначально и задумывалась (см. её докстринг ниже).
#[derive(Debug)]
pub enum TransportError {
    Fatal(String),
    Transient(String),
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransportError::Fatal(m) | TransportError::Transient(m) => write!(f, "{m}"),
        }
    }
}

/// Обёртка над ЛЮБЫМ ChatTransport, добавляющая грамматическую
/// валидацию постфактум (Срез 5, деградированный режим). Единственный
/// тип повторов здесь — ПО СОДЕРЖИМОМУ (невалидный вывод, `max_attempts`);
/// сетевые ретраи — отдельный уровень внутри самого транспорта (у
/// AnthropicTransport свои, сторонний транспорт вправе иметь свои),
/// та же граница, что в backends.py между network_retries и
/// max_attempts (docs/EMPIRICAL_REAL_MODEL.md: путать их значило бы
/// читать сетевой сбой шлюза как провал модели). Обобщена (не жёстко
/// привязана к AnthropicTransport) для переиспользования сторонними
/// приложениями с уже настроенным LLM-клиентом — см. докстринг ChatTransport.
pub struct ValidatingBackend<T: ChatTransport> {
    transport: T,
    max_attempts: u32,
    pub retry_count: u64,
}

impl<T: ChatTransport> ValidatingBackend<T> {
    pub fn new(transport: T, max_attempts: u32) -> Self {
        ValidatingBackend {
            transport,
            max_attempts,
            retry_count: 0,
        }
    }
}

impl<T: ChatTransport> ModelBackend for ValidatingBackend<T> {
    fn generate(
        &mut self,
        prompt: &str,
        constraint: &Constraint,
        kind: Kind,
        slots: &BTreeMap<&'static str, SlotData>,
    ) -> Result<String, InvalidModelOutput> {
        // Вырожденная грамматика — без вызова модели вообще.
        if let Some(singleton) = decoding::singleton_output(kind, slots) {
            return Ok(singleton);
        }

        let mut instruction = format!(
            "{prompt}\n\n\
             Ниже — EBNF-правило, по которому будет проверен твой ответ.\n\
             Это НЕ текст для повторения: не цитируй правило и не пиши \
             'output ='. Выведи только само значение, удовлетворяющее \
             правилу. Без пояснений, без markdown, без ```. Если правило \
             допускает только пустую строку — не выводи ни одного символа.\n\
             {}",
            constraint.definition
        );

        if kind == Kind::Spawn {
            let ex1 = decoding::example_node_source(Kind::Step, "example_1");
            let ex2 = decoding::example_node_source(Kind::Role, "example_2");
            instruction.push_str(&format!(
                "\n\nПримеры валидных узлов (форма и СВОИ ключевые слова \
                 для каждого типа — не копируй DO/INTO на другие типы, \
                 у каждого типа узла свой набор ключевых слов):\n{ex1}\n\n{ex2}"
            ));
        }

        // Найдено на батч-прогоне (не в теории — реальная модель дала
        // MATCH на условии category="build_error", когда предок явно
        // показал "=> feature_request"): структурная связь между "INTO
        // category" у предка и "WHERE category = ..." у FILTER —
        // ИМПЛИЦИТНАЯ, модели неоткуда узнать, что это одна и та же
        // переменная, если не сказать прямо. Тот же класс проблемы, что
        // уже решался для SPAWN (структуры недостаточно, нужна явная
        // формулировка) — здесь исправление симметрично: объясняем
        // связь словами, а не полагаемся на то, что модель сама
        // выведет её из имени идентификатора.
        if kind == Kind::Filter {
            instruction.push_str(
                "\n\nВ WHERE-условии выше значение переменной (например, \
                 category) — это то, что стоит после '=>' у узла-предка, \
                 который заканчивается на 'INTO <то же имя>'. Сравни это \
                 значение с правой частью условия БУКВАЛЬНО, посимвольно. \
                 MATCH — только при точном совпадении, иначе NO_MATCH. \
                 Не предполагай совпадение по смыслу или близости темы.",
            );
        }

        // LUCK_DEBUG_PROMPTS=1 — диагностика "что реально видит модель",
        // не часть обычного пути: добавлено при разборе конкретной
        // находки (FILTER давал MATCH на условии, которое явно не
        // выполнялось) — оставлено как постоянный инструмент, полезный
        // для любой будущей похожей аномалии, не выпилено после
        // разового использования. Печатает ФИНАЛЬНЫЙ текст (после
        // SPAWN-дополнения), тот, что реально уйдёт в API.
        if std::env::var("LUCK_DEBUG_PROMPTS").as_deref() == Ok("1") {
            eprintln!("--- PROMPT [{}] ---\n{instruction}\n---", kind.as_str());
        }

        let mut reason = "нет попыток".to_string();
        for attempt in 1..=self.max_attempts {
            if attempt > 1 {
                self.retry_count += 1;
            }
            let sent = if attempt == 1 {
                instruction.clone()
            } else {
                format!("{instruction}\n\nПредыдущий ответ отвергнут: {reason}")
            };
            // Fatal (401/403/400/404 — не исправятся повтором ни на
            // каком узле) останавливает ВЕСЬ обход (fatal=true ->
            // Scheduler::run прерывается целиком, см. её докстринг).
            // Transient (временный сетевой сбой) — как было раньше:
            // ОТЛИЧИЕ ОТ PYTHON: там сетевая ошибка, исчерпавшая
            // внутренние ретраи transport'а, пробрасывается как
            // необработанное исключение, рушит скрипт целиком. Здесь —
            // reject(SYNTAX) ОДНОГО узла: одна закапризничавшая сеть не
            // должна рушить прогон, если граф уже частично исполнен
            // (то же рассуждение, что оправдывает reject как узел графа
            // вообще, Этап Б Фаза 5, применённое к транспортному сбою).
            let last = match self.transport.call(&sent) {
                Ok(text) => text,
                Err(TransportError::Fatal(msg)) => {
                    return Err(InvalidModelOutput::fatal(format!("транспорт: {msg}")));
                }
                Err(TransportError::Transient(msg)) => {
                    return Err(InvalidModelOutput::new(format!("сетевая ошибка: {msg}")));
                }
            };

            if std::env::var("LUCK_DEBUG_PROMPTS").as_deref() == Ok("1") {
                eprintln!(
                    "--- RAW RESPONSE [{}] attempt {attempt} ---\n{last:?}\n---",
                    kind.as_str()
                );
            }

            match decoding::validate_output(kind, slots, &last) {
                Ok(()) => return Ok(last.trim().to_string()),
                Err(r) => reason = r,
            }
        }

        Err(InvalidModelOutput::new(reason))
    }

    fn call_count(&self) -> u64 {
        self.transport.call_count()
    }
}
