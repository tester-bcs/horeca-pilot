//! Мутационное тестирование валидатора на РЕАЛЬНОЙ модели проекта.
//!
//! Атакующие тесты (`icom_attack.rs`) бьют по синтетическим моделям; они
//! доказывают, что правило существует, но не что оно сработает на модели,
//! похожей на настоящую. Здесь наоборот: берём `examples_luck/horeca-model.json`
//! (A1..A8, проходит чисто), вносим ОДИН реалистичный дефект и требуем, чтобы
//! валидатор его назвал — и назвал в правильном блоке.
//!
//! Провал теста здесь означает, что правило есть, но на реальной модели молчит.
use luck_pilot::icom::{has_errors, validate_model, Rule, Severity, Violation};
use luck_pilot::idef0::{Block, Idef0Model};

const MODEL_JSON: &str = include_str!("../../examples_luck/horeca-model.json");

fn demo() -> Idef0Model {
    serde_json::from_str(MODEL_JSON).expect("демо-модель разбирается")
}

fn child<'a>(m: &'a mut Idef0Model, id: &str) -> &'a mut Block {
    m.context
        .children
        .iter_mut()
        .find(|b| b.id == id)
        .unwrap_or_else(|| panic!("в демо-модели нет блока {id}"))
}

fn errors(v: &[Violation]) -> Vec<&Violation> {
    v.iter().filter(|x| x.severity == Severity::Error).collect()
}

/// Контроль: немутированная модель обязана проходить чисто. Без этого
/// все тесты ниже ничего не доказывают — они ловили бы фоновый шум.
#[test]
fn unmutated_demo_is_clean() {
    let v = validate_model(&demo());
    assert!(v.is_empty(), "эталонная модель должна быть без замечаний: {v:#?}");
}

/// Мутация 1 — опечатка в имени потока. Самый частый реальный дефект:
/// до валидатора он молча оставлял граф без ребра A4→A5.
#[test]
fn mutation_typo_in_flow_name() {
    let mut m = demo();
    child(&mut m, "A4").outputs = vec!["picked_order_опечатка".into()];
    let v = validate_model(&m);
    let e = errors(&v);
    assert!(
        e.iter().any(|x| x.rule == Rule::DanglingInput && x.block == "A5"),
        "опечатка в выходе A4 должна оставить вход A5 без источника: {v:#?}"
    );
}

/// Мутация 2 — потерян блок, производящий выход всей модели.
#[test]
fn mutation_dropped_final_block() {
    let mut m = demo();
    m.context.children.retain(|b| b.id != "A8");
    let v = validate_model(&m);
    assert!(
        errors(&v)
            .iter()
            .any(|x| x.rule == Rule::MissingOutput && x.arrow.as_deref() == Some("отчёт_дня")),
        "без A8 обещанный выход модели никто не производит: {v:#?}"
    );
}

/// Мутация 3 — цикл: ранний блок начинает читать поздний поток.
#[test]
fn mutation_introduces_cycle() {
    let mut m = demo();
    child(&mut m, "A1").inputs.push("invoice".into());
    let v = validate_model(&m);
    assert!(
        errors(&v).iter().any(|x| x.rule == Rule::Cycle),
        "A1, читающий invoice от A7, замыкает цикл: {v:#?}"
    );
}

/// Мутация 4 — блок начинает читать собственный выход.
#[test]
fn mutation_self_loop() {
    let mut m = demo();
    let own = child(&mut m, "A3").outputs[0].clone();
    child(&mut m, "A3").inputs.push(own.clone());
    let v = validate_model(&m);
    assert!(
        errors(&v)
            .iter()
            .any(|x| x.rule == Rule::SelfLoop && x.block == "A3" && x.arrow.as_deref() == Some(&own)),
        "A3 не может питаться собственным выходом: {v:#?}"
    );
}

/// Мутация 5 — дубликат id (копипаста блока при правке модели руками).
#[test]
fn mutation_duplicate_id() {
    let mut m = demo();
    let mut copy = child(&mut m, "A6").clone();
    copy.function = "Доставить ещё раз".into();
    m.context.children.push(copy);
    let v = validate_model(&m);
    assert!(
        errors(&v).iter().any(|x| x.rule == Rule::DuplicateId),
        "дубликат id ловится до маппера, который на нём паникует: {v:#?}"
    );
}

/// Мутация 6 — пустая формулировка функции (блок добавили, назвать забыли).
#[test]
fn mutation_empty_function() {
    let mut m = demo();
    child(&mut m, "A2").function = String::new();
    let v = validate_model(&m);
    assert!(
        errors(&v).iter().any(|x| x.rule == Rule::EmptyBlock && x.block == "A2"),
        "блок без формулировки функции бессмыслен: {v:#?}"
    );
}

/// Мутация 7 — ветка в никуда (блок переименовали, ветку не поправили).
#[test]
fn mutation_branch_to_nowhere() {
    let mut m = demo();
    let b = child(&mut m, "A5");
    b.kind = Some("branch".into());
    b.branches = vec![("ok".into(), "A99".into())];
    let v = validate_model(&m);
    assert!(
        errors(&v)
            .iter()
            .any(|x| x.rule == Rule::BranchTargetMissing && x.block == "A5"),
        "ветка на несуществующий блок должна быть ошибкой: {v:#?}"
    );
}

/// Мутация 8 — блок-сирота: добавлен, но ни с чем не связан.
#[test]
fn mutation_orphan_block() {
    let mut m = demo();
    m.context.children.push(Block {
        id: "A9".into(),
        function: "Забытый шаг".into(),
        kind: None,
        inputs: vec![],
        controls: vec![],
        outputs: vec!["никем_не_читается".into()],
        mechanisms: vec![],
        children: vec![],
        branches: vec![],
    });
    let v = validate_model(&m);
    assert!(
        v.iter().any(|x| x.rule == Rule::OrphanBlock && x.block == "A9"),
        "блок вне потока должен быть замечен: {v:#?}"
    );
    assert!(
        !has_errors(&v),
        "сирота — предупреждение: модель остаётся исполнимой"
    );
}

/// Ни одна мутация не должна проходить незамеченной. Сводный тест: если
/// какая-то из них молчит, здесь видно сразу, какая именно.
#[test]
fn every_mutation_is_caught() {
    let mutations: Vec<(&str, fn(&mut Idef0Model))> = vec![
        ("опечатка в потоке", |m| {
            child(m, "A4").outputs = vec!["typo".into()]
        }),
        ("потерян финальный блок", |m| {
            m.context.children.retain(|b| b.id != "A8")
        }),
        ("цикл", |m| child(m, "A1").inputs.push("invoice".into())),
        ("петля", |m| {
            let own = child(m, "A3").outputs[0].clone();
            child(m, "A3").inputs.push(own)
        }),
        ("пустая функция", |m| child(m, "A2").function = String::new()),
        ("ветка в никуда", |m| {
            let b = child(m, "A5");
            b.kind = Some("branch".into());
            b.branches = vec![("ok".into(), "A99".into())];
        }),
    ];
    let mut missed = Vec::new();
    for (name, mutate) in mutations {
        let mut m = demo();
        mutate(&mut m);
        if !has_errors(&validate_model(&m)) {
            missed.push(name);
        }
    }
    assert!(missed.is_empty(), "мутации прошли незамеченными: {missed:?}");
}
