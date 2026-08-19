//! Интервью-агент: обследование предприятия разговором → IDEF0-модель.
//!
//! Замыкает контур Агента-BPWin (`docs/AGENT-BPWIN.md`): клиент НЕ рисует
//! диаграммы, он отвечает на вопросы; агент строит модель, валидатор говорит,
//! где она дырявая, агент задаёт недостающий вопрос. Готовая модель уходит в
//! `icom::map_to_graph_checked` и становится исполнимым Luck-планом.
//!
//! РАЗДЕЛЕНИЕ ОТВЕТСТВЕННОСТИ (главное решение модуля):
//!
//!   ЧТО спросить  — детерминированно, `next_question` есть чистая функция от
//!                   состояния модели. Ход обследования задают нарушения
//!                   валидатора (`icom`), а не модель языка. Поэтому порядок
//!                   вопросов воспроизводим, тестируется без сети и не зависит
//!                   от того, какая LLM подключена.
//!   КАК понять ответ — LLM, единственная её роль: свободный текст клиента →
//!                   структура (`Block`). Ничего не решает о полноте модели.
//!
//! Обратный порядок (LLM ведёт опрос, код записывает) отвергнут сознательно:
//! тогда полнота обследования зависела бы от того, догадалась ли модель
//! спросить, а проверяемость — главное требование пайпа.

use crate::icom::{validate_model, Rule, Severity, Violation};
use crate::idef0::{Block, Idef0Model};
use luck_engine::anthropic::ChatTransport;
use serde::Deserialize;

/// Следующий шаг обследования. Вопрос выведен из состояния модели, а не
/// придуман: каждому варианту соответствует конкретный пробел.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Question {
    /// Модели ещё нет: рамка A-0 — чем занято предприятие, что на входе/выходе.
    Context,
    /// Рамка есть, декомпозиции нет: из каких шагов состоит процесс.
    Steps,
    /// Вход шага не производится никем и не приходит извне.
    MissingProducer { block: String, flow: String },
    /// Обещанный моделью выход никто не производит.
    MissingProducerForResult { flow: String },
    /// Шаг без формулировки функции.
    EmptyFunction { block: String },
    /// Ветка указывает в никуда.
    BranchTarget { block: String, target: String },
    /// Цикл по данным.
    Cycle { blocks: String },
    /// Шаг вне потока.
    Orphan { block: String },
    /// Прочее нарушение, для которого нет отдельной формулировки.
    Other { block: String, message: String },
    /// Нарушений нет — показать модель клиенту на подтверждение.
    Confirm,
}

impl Question {
    /// Текст вопроса клиенту. Формулировки деловые: обследование ведёт
    /// консультант, а клиент не обязан знать слов «ICOM» и «декомпозиция».
    pub fn text(&self) -> String {
        match self {
            Question::Context => "Чем занимается предприятие? Опишите одним предложением \
                 главный процесс, что поступает на вход и что получается на выходе."
                .to_string(),
            Question::Steps => "Из каких последовательных шагов состоит этот процесс? \
                 Перечислите их по порядку, для каждого — что он получает и что выдаёт."
                .to_string(),
            Question::MissingProducer { block, flow } => format!(
                "Шагу «{block}» нужно «{flow}», но ни один из названных шагов этого не \
                 производит. Откуда это берётся — какой шаг это готовит или это приходит \
                 извне предприятия?"
            ),
            Question::MissingProducerForResult { flow } => format!(
                "Итог процесса — «{flow}», но ни один шаг его не производит. \
                 Какой шаг формирует этот результат?"
            ),
            Question::EmptyFunction { block } => {
                format!("Шаг «{block}» назван, но не описан. Что на нём делают?")
            }
            Question::BranchTarget { block, target } => format!(
                "На шаге «{block}» есть развилка, ведущая к «{target}», но такого шага в \
                 процессе нет. Куда идёт работа после этой развилки?"
            ),
            Question::Cycle { blocks } => format!(
                "Шаги {blocks} ждут результатов друг друга по кругу, поэтому начать процесс \
                 не с чего. С какого из них работа начинается на самом деле?"
            ),
            Question::Orphan { block } => format!(
                "Шаг «{block}» ни с чем не связан: он ничего не получает от других шагов и \
                 его результат никому не нужен. Куда он встраивается — или его убрать?"
            ),
            Question::Other { block, message } => {
                format!("По шагу «{block}» есть неясность: {message} Уточните, пожалуйста.")
            }
            Question::Confirm => "Модель процесса собрана и непротиворечива. \
                 Подтвердите её — или скажите, что поправить."
                .to_string(),
        }
    }

    /// Обследование завершено (осталось подтверждение клиента).
    pub fn is_final(&self) -> bool {
        matches!(self, Question::Confirm)
    }
}

/// Первое нарушение, которое стоит закрыть: сначала ошибки, потом
/// предупреждения; внутри — в порядке появления. Детерминированно, поэтому
/// один и тот же ответ клиента всегда ведёт к одному и тому же вопросу.
fn first_gap(violations: &[Violation]) -> Option<&Violation> {
    violations
        .iter()
        .find(|v| v.severity == Severity::Error)
        .or_else(|| violations.first())
}

fn violation_to_question(v: &Violation) -> Question {
    let arrow = v.arrow.clone().unwrap_or_default();
    match v.rule {
        Rule::DanglingInput => Question::MissingProducer {
            block: v.block.clone(),
            flow: arrow,
        },
        Rule::MissingOutput => Question::MissingProducerForResult { flow: arrow },
        Rule::EmptyBlock => Question::EmptyFunction {
            block: v.block.clone(),
        },
        Rule::BranchTargetMissing | Rule::BranchKindMismatch => Question::BranchTarget {
            block: v.block.clone(),
            target: arrow,
        },
        Rule::Cycle | Rule::SelfLoop => Question::Cycle {
            blocks: if arrow.is_empty() {
                v.block.clone()
            } else {
                format!("{} ({arrow})", v.block)
            },
        },
        Rule::OrphanBlock => Question::Orphan {
            block: v.block.clone(),
        },
        _ => Question::Other {
            block: v.block.clone(),
            message: v.message.clone(),
        },
    }
}

/// ЧТО спросить дальше — чистая функция от состояния модели.
/// Никакой сети, никакой LLM: ход обследования задаёт валидатор.
pub fn next_question(model: &Option<Idef0Model>) -> Question {
    let Some(model) = model else {
        return Question::Context;
    };
    if model.context.children.is_empty() {
        return Question::Steps;
    }
    match first_gap(&validate_model(model)) {
        Some(v) => violation_to_question(v),
        None => Question::Confirm,
    }
}

// --- Извлечение структуры из свободного текста (единственная роль LLM) ---

#[derive(Debug, Deserialize)]
struct ContextAnswer {
    function: String,
    #[serde(default)]
    inputs: Vec<String>,
    #[serde(default)]
    controls: Vec<String>,
    #[serde(default)]
    outputs: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct StepsAnswer {
    steps: Vec<Block>,
}

/// Снять обрамление ```json ... ``` — модели его добавляют вопреки просьбе.
fn strip_fence(s: &str) -> &str {
    let t = s.trim();
    let Some(rest) = t.strip_prefix("```") else {
        return t;
    };
    let rest = rest.strip_prefix("json").unwrap_or(rest);
    rest.trim_start()
        .strip_suffix("```")
        .unwrap_or(rest)
        .trim()
        .trim_end_matches("```")
        .trim()
}

fn ask_json<T: serde::de::DeserializeOwned>(
    transport: &mut dyn ChatTransport,
    prompt: &str,
) -> Result<T, String> {
    let raw = transport.call(prompt).map_err(|e| format!("транспорт: {e:?}"))?;
    let text = strip_fence(&raw);
    serde_json::from_str(text)
        .map_err(|e| format!("ответ модели — не ожидаемый JSON: {e}; получено: {text}"))
}

const RULES: &str = "Отвечай ТОЛЬКО JSON, без пояснений и без markdown-обрамления. \
Имена потоков — короткие идентификаторы латиницей или кириллицей без пробелов; \
одинаковые сущности в разных шагах называй ОДИНАКОВО (выход одного шага должен \
дословно совпадать с входом следующего).";

/// Свободный текст → рамка A-0.
pub fn extract_context(
    transport: &mut dyn ChatTransport,
    answer: &str,
) -> Result<Idef0Model, String> {
    let prompt = format!(
        "{RULES}\nПо описанию предприятия заполни рамку процесса.\n\
         Схема: {{\"function\":str,\"inputs\":[str],\"controls\":[str],\"outputs\":[str]}}\n\
         function — главный процесс; inputs — что поступает извне; controls — нормы, \
         правила, ограничения; outputs — конечный результат.\n\nОписание: {answer}"
    );
    let a: ContextAnswer = ask_json(transport, &prompt)?;
    if a.function.trim().is_empty() {
        return Err("модель не назвала главный процесс".into());
    }
    Ok(Idef0Model {
        context: Block {
            id: "A0".into(),
            function: a.function,
            kind: None,
            inputs: a.inputs,
            controls: a.controls,
            outputs: a.outputs,
            mechanisms: vec![],
            children: vec![],
            branches: vec![],
        },
    })
}

/// Свободный текст → шаги процесса (декомпозиция рамки).
pub fn extract_steps(
    transport: &mut dyn ChatTransport,
    model: &Idef0Model,
    answer: &str,
) -> Result<Vec<Block>, String> {
    let ctx = &model.context;
    let prompt = format!(
        "{RULES}\nРазложи процесс на последовательные шаги.\n\
         Схема: {{\"steps\":[{{\"id\":str,\"function\":str,\"inputs\":[str],\
         \"controls\":[str],\"outputs\":[str],\"mechanisms\":[str]}}]}}\n\
         id — вида A1, A2, ...; вход первого шага бери из inputs рамки, выход последнего \
         обязан совпасть с outputs рамки; вход каждого следующего шага — выход предыдущего.\n\n\
         Рамка: процесс «{}», вход {:?}, управление {:?}, выход {:?}\n\nОтвет клиента: {answer}",
        ctx.function, ctx.inputs, ctx.controls, ctx.outputs
    );
    let a: StepsAnswer = ask_json(transport, &prompt)?;
    if a.steps.is_empty() {
        return Err("модель не выделила ни одного шага".into());
    }
    Ok(a.steps)
}

/// Свободный текст → уточняющая правка модели (ответ на вопрос о пробеле).
pub fn extract_fix(
    transport: &mut dyn ChatTransport,
    model: &Idef0Model,
    question: &Question,
    answer: &str,
) -> Result<Vec<Block>, String> {
    let current = serde_json::to_string(&model.context.children).unwrap_or_default();
    let prompt = format!(
        "{RULES}\nИсправь список шагов процесса по ответу клиента.\n\
         Схема: {{\"steps\":[{{\"id\":str,\"function\":str,\"inputs\":[str],\
         \"controls\":[str],\"outputs\":[str],\"mechanisms\":[str]}}]}}\n\
         Верни ПОЛНЫЙ список шагов после правки, а не только изменённые.\n\n\
         Текущие шаги: {current}\n\nБыл задан вопрос: {}\n\nОтвет клиента: {answer}",
        question.text()
    );
    let a: StepsAnswer = ask_json(transport, &prompt)?;
    if a.steps.is_empty() {
        return Err("после правки не осталось ни одного шага".into());
    }
    Ok(a.steps)
}

/// Ход обследования: состояние модели плюс журнал.
#[derive(Debug, Default)]
pub struct Interview {
    pub model: Option<Idef0Model>,
    pub log: Vec<Turn>,
}

#[derive(Debug, Clone)]
pub struct Turn {
    pub question: String,
    pub answer: String,
}

impl Interview {
    pub fn new() -> Self {
        Self::default()
    }

    /// Текущий вопрос — чистая функция состояния, без обращения к сети.
    pub fn question(&self) -> Question {
        next_question(&self.model)
    }

    /// Принять ответ клиента и продвинуть обследование на шаг.
    /// Возвращает следующий вопрос.
    ///
    /// Модель НЕ применяется вслепую: правка, добавляющая новые нарушения
    /// вместо того чтобы убрать старое, откатывается — иначе разговор
    /// зацикливается на всё более дырявой модели.
    pub fn answer(
        &mut self,
        transport: &mut dyn ChatTransport,
        answer: &str,
    ) -> Result<Question, String> {
        let question = self.question();
        self.log.push(Turn {
            question: question.text(),
            answer: answer.to_string(),
        });

        match &question {
            Question::Context => {
                self.model = Some(extract_context(transport, answer)?);
            }
            Question::Steps => {
                let frame = self.model.clone().expect("рамка уже собрана");
                let steps = extract_steps(transport, &frame, answer)?;
                self.model.as_mut().expect("рамка уже собрана").context.children = steps;
            }
            Question::Confirm => return Ok(Question::Confirm),
            q => {
                let model = self.model.as_ref().expect("модель уже собрана");
                let before = count_errors(model);
                let fixed = extract_fix(transport, model, q, answer)?;
                let mut candidate = model.clone();
                candidate.context.children = fixed;
                let after = count_errors(&candidate);
                if after > before {
                    return Err(format!(
                        "правка отклонена: ошибок было {before}, стало {after} — \
                         модель осталась прежней, переспросите клиента"
                    ));
                }
                self.model = Some(candidate);
            }
        }
        Ok(self.question())
    }

    /// Готовая модель, если обследование завершено.
    pub fn finished_model(&self) -> Option<&Idef0Model> {
        match (&self.model, self.question()) {
            (Some(m), Question::Confirm) => Some(m),
            _ => None,
        }
    }
}

fn count_errors(model: &Idef0Model) -> usize {
    validate_model(model)
        .iter()
        .filter(|v| v.severity == Severity::Error)
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_state_asks_for_context() {
        assert_eq!(next_question(&None), Question::Context);
    }

    #[test]
    fn frame_without_steps_asks_for_steps() {
        let m = Idef0Model {
            context: Block {
                id: "A0".into(),
                function: "Печь хлеб".into(),
                kind: None,
                inputs: vec!["мука".into()],
                controls: vec![],
                outputs: vec!["хлеб".into()],
                mechanisms: vec![],
                children: vec![],
                branches: vec![],
            },
        };
        assert_eq!(next_question(&Some(m)), Question::Steps);
    }

    #[test]
    fn dangling_input_becomes_targeted_question() {
        let v = Violation {
            block: "A5".into(),
            rule: Rule::DanglingInput,
            severity: Severity::Error,
            arrow: Some("picked_order".into()),
            message: "…".into(),
        };
        let q = violation_to_question(&v);
        assert_eq!(
            q,
            Question::MissingProducer {
                block: "A5".into(),
                flow: "picked_order".into()
            }
        );
        let text = q.text();
        assert!(text.contains("A5") && text.contains("picked_order"));
    }

    #[test]
    fn errors_are_asked_before_warnings() {
        let warn = Violation {
            block: "A9".into(),
            rule: Rule::OrphanBlock,
            severity: Severity::Warning,
            arrow: None,
            message: "…".into(),
        };
        let err = Violation {
            block: "A5".into(),
            rule: Rule::DanglingInput,
            severity: Severity::Error,
            arrow: Some("x".into()),
            message: "…".into(),
        };
        let violations = [warn, err];
        let gap = first_gap(&violations).expect("есть пробел");
        assert_eq!(gap.block, "A5", "ошибка важнее предупреждения");
    }

    #[test]
    fn clean_model_reaches_confirm() {
        let json = include_str!("../../examples_luck/horeca-model.json");
        let m: Idef0Model = serde_json::from_str(json).expect("демо-модель");
        assert_eq!(next_question(&Some(m)), Question::Confirm);
    }

    #[test]
    fn fence_is_stripped() {
        assert_eq!(strip_fence("```json\n{\"a\":1}\n```"), "{\"a\":1}");
        assert_eq!(strip_fence("  {\"a\":1}  "), "{\"a\":1}");
    }
}
