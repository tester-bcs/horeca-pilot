//! Сквозной прогон обследования: разговор → IDEF0-модель → исполнимый граф.
//!
//! Без сети: транспорт скриптован, то есть ответы «LLM» заданы заранее. Это
//! не упрощение ради удобства — проверять надо ход обследования, а он по
//! устройству модуля детерминирован и от выбора LLM не зависит. Реальная
//! модель проверяется вручную через `cargo run --bin interview`.
use luck_engine::anthropic::{ChatTransport, TransportError};
use luck_pilot::icom::map_to_graph_checked;
use luck_pilot::interview::{Interview, Question};
use luck_pilot::{MockBackend, Scheduler, ToolRegistry};

/// Транспорт, отдающий заготовленные ответы по очереди.
struct Scripted {
    replies: Vec<String>,
    calls: u64,
}

impl Scripted {
    fn new(replies: &[&str]) -> Self {
        Self {
            replies: replies.iter().rev().map(|s| s.to_string()).collect(),
            calls: 0,
        }
    }
}

impl ChatTransport for Scripted {
    fn call_count(&self) -> u64 {
        self.calls
    }
    fn call(&mut self, _prompt: &str) -> Result<String, TransportError> {
        self.calls += 1;
        Ok(self.replies.pop().expect("скрипт исчерпан — лишний вызов LLM"))
    }
}

const CONTEXT: &str = r#"{"function":"Печь и продавать хлеб",
  "inputs":["заявки"],"controls":["рецептура"],"outputs":["отгрузка"]}"#;

/// Шаги с намеренным пробелом: A3 требует «накладная», которой никто не делает.
const STEPS_WITH_GAP: &str = r#"{"steps":[
  {"id":"A1","function":"Принять заявки","inputs":["заявки"],"outputs":["заказ"]},
  {"id":"A2","function":"Испечь партию","inputs":["заказ"],"controls":["рецептура"],"outputs":["партия"]},
  {"id":"A3","function":"Отгрузить клиенту","inputs":["партия","накладная"],"outputs":["отгрузка"]}
]}"#;

/// Правка: A2 начинает выдавать накладную — пробел закрыт.
const STEPS_FIXED: &str = r#"{"steps":[
  {"id":"A1","function":"Принять заявки","inputs":["заявки"],"outputs":["заказ"]},
  {"id":"A2","function":"Испечь партию","inputs":["заказ"],"controls":["рецептура"],"outputs":["партия","накладная"]},
  {"id":"A3","function":"Отгрузить клиенту","inputs":["партия","накладная"],"outputs":["отгрузка"]}
]}"#;

#[test]
fn interview_reaches_executable_model() {
    let mut t = Scripted::new(&[CONTEXT, STEPS_WITH_GAP, STEPS_FIXED]);
    let mut iv = Interview::new();

    // 1. Первый вопрос — про рамку, ещё до всякой LLM.
    assert_eq!(iv.question(), Question::Context);

    // 2. Ответ про предприятие → появилась рамка, спрашиваем шаги.
    let q = iv.answer(&mut t, "Мы небольшая пекарня: берём заявки и возим хлеб кафе").unwrap();
    assert_eq!(q, Question::Steps);

    // 3. Ответ про шаги содержит пробел — агент обязан его заметить сам
    //    и спросить именно про него, а не пойти дальше.
    let q = iv.answer(&mut t, "Принимаем заявки, печём, отгружаем").unwrap();
    match &q {
        Question::MissingProducer { block, flow } => {
            assert_eq!(block, "A3");
            assert_eq!(flow, "накладная");
            assert!(q.text().contains("накладная"), "вопрос называет пробел: {}", q.text());
        }
        other => panic!("ожидался вопрос о недостающем источнике, получено {other:?}"),
    }

    // 4. Клиент объясняет — модель дособрана и непротиворечива.
    let q = iv.answer(&mut t, "Накладную печатает пекарь вместе с партией").unwrap();
    assert_eq!(q, Question::Confirm);
    assert!(q.is_final());

    // 5. Готовая модель проходит валидатор и становится исполнимым графом.
    let model = iv.finished_model().expect("обследование завершено");
    let (mut graph, warnings) = map_to_graph_checked(model).expect("модель валидна");
    assert!(warnings.is_empty(), "лишних замечаний быть не должно: {warnings:#?}");

    let mut backend = MockBackend::default();
    let mut tools = ToolRegistry::default();
    let mut sched = Scheduler::new(&mut backend, &mut tools);
    let res = sched.run(&mut graph, "interview").expect("граф исполним");
    assert_eq!(res.order.len(), 3, "исполнены все три шага: {:?}", res.order);

    // 6. Журнал сохранил весь разговор — это протокол обследования.
    assert_eq!(iv.log.len(), 3, "три заданных вопроса и три ответа");
    assert!(iv.log[0].question.contains("Чем занимается предприятие"));
}

/// Правка, ухудшающая модель, откатывается: иначе разговор ходит по кругу,
/// а модель с каждым ответом становится дырявее.
#[test]
fn worsening_fix_is_rejected() {
    const STEPS_WORSE: &str = r#"{"steps":[
      {"id":"A1","function":"Принять заявки","inputs":["заявки"],"outputs":["заказ"]},
      {"id":"A2","function":"Испечь партию","inputs":["ниоткуда"],"outputs":["партия"]},
      {"id":"A3","function":"Отгрузить клиенту","inputs":["ещё_ниоткуда"],"outputs":["отгрузка"]}
    ]}"#;

    let mut t = Scripted::new(&[CONTEXT, STEPS_WITH_GAP, STEPS_WORSE]);
    let mut iv = Interview::new();
    iv.answer(&mut t, "пекарня").unwrap();
    iv.answer(&mut t, "заявки, выпечка, отгрузка").unwrap();

    let before = iv.model.clone().expect("модель есть");
    let err = iv
        .answer(&mut t, "запутанный ответ")
        .expect_err("ухудшающая правка должна быть отклонена");
    assert!(err.contains("отклонена"), "внятная причина отказа: {err}");

    let after = iv.model.clone().expect("модель на месте");
    assert_eq!(
        serde_json::to_string(&before.context.children).unwrap(),
        serde_json::to_string(&after.context.children).unwrap(),
        "после отклонённой правки модель обязана остаться прежней"
    );
}

/// Мусор вместо JSON — внятная ошибка, а не паника и не порча модели.
#[test]
fn garbage_answer_does_not_corrupt_model() {
    let mut t = Scripted::new(&["я не понял вопроса"]);
    let mut iv = Interview::new();
    let err = iv.answer(&mut t, "что-то").expect_err("ожидалась ошибка разбора");
    assert!(err.contains("не ожидаемый JSON"), "внятная диагностика: {err}");
    assert!(iv.model.is_none(), "модель не должна появиться из мусора");
}
