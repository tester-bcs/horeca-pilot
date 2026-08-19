//! Атака на валидатор ICOM: модели, которые ОБЯЗАНЫ быть отвергнуты.
//! Каждый тест — гипотеза о дыре. Проваленный тест = найденная дыра.
use luck_pilot::icom::{has_errors, validate_model};
use luck_pilot::idef0::{map_to_graph, Block, Idef0Model};
use luck_pilot::{MockBackend, Scheduler, ToolRegistry};

fn b(id: &str, f: &str, ins: &[&str], outs: &[&str]) -> Block {
    Block {
        id: id.into(),
        function: f.into(),
        kind: None,
        inputs: ins.iter().map(|s| s.to_string()).collect(),
        controls: vec![],
        outputs: outs.iter().map(|s| s.to_string()).collect(),
        mechanisms: vec![],
        children: vec![],
        branches: vec![],
    }
}

fn model(root_in: &[&str], root_out: &[&str], children: Vec<Block>) -> Idef0Model {
    Idef0Model {
        context: Block {
            inputs: root_in.iter().map(|s| s.to_string()).collect(),
            outputs: root_out.iter().map(|s| s.to_string()).collect(),
            children,
            ..b("A0", "рамка", &[], &[])
        },
    }
}

/// Атака 1: взаимная зависимость A1<->A2. Каждый вход «производится соседом»,
/// правила §3.1-3.3 довольны, но граф циклический и Scheduler его не исполнит.
#[test]
fn attack_cycle_must_be_rejected() {
    let m = model(
        &["старт"],
        &["итог"],
        vec![
            b("A1", "первый", &["старт", "y"], &["x"]),
            b("A2", "второй", &["x"], &["y", "итог"]),
        ],
    );
    let v = validate_model(&m);
    assert!(
        has_errors(&v),
        "цикл A1->A2->A1 должен быть ошибкой, а валидатор молчит: {v:#?}"
    );
}

/// Атака 2: блок потребляет собственный выход. Маппер пропускает петли
/// (from_id == to_id), значит стрелка висит, но «производится» формально.
#[test]
fn attack_self_loop_must_be_rejected() {
    let m = model(
        &["старт"],
        &["итог"],
        vec![b("A1", "сам себя", &["старт", "итог"], &["итог"])],
    );
    let v = validate_model(&m);
    assert!(has_errors(&v), "самопитание блока должно быть ошибкой: {v:#?}");
}

/// Атака 3: два блока производят один и тот же поток. Маппер построит рёбра
/// от ОБОИХ — Seq-семантика требует все входящие, порядок молча меняется.
#[test]
fn attack_duplicate_output_must_be_flagged() {
    let m = model(
        &["старт"],
        &["итог"],
        vec![
            b("A1", "первый", &["старт"], &["общий"]),
            b("A2", "второй", &["старт"], &["общий"]),
            b("A3", "третий", &["общий"], &["итог"]),
        ],
    );
    let v = validate_model(&m);
    assert!(
        !v.is_empty(),
        "две разные функции с одним именем выхода — как минимум предупреждение"
    );
}

/// Атака 4: выход branch-блока читает блок, НЕ указанный в его ветках.
/// Маппер для kind=branch строит только Branch-рёбра по `branches`
/// (`continue`), поэтому такой потребитель остаётся вовсе без входящего
/// ребра и исполняется как корневой — раньше своего источника данных.
/// (Потребитель, который сам является целью ветки, ребро получает —
/// это законный случай, он проверяется отдельно ниже.)
#[test]
fn attack_branch_output_consumer_must_be_flagged() {
    let mut br = b("A2", "решить", &["данные"], &["вердикт"]);
    br.kind = Some("branch".into());
    br.branches = vec![("ok".into(), "A3".into())];
    let m = model(
        &["старт"],
        &["итог"],
        vec![
            b("A1", "подготовить", &["старт"], &["данные"]),
            br,
            b("A3", "закрыть", &["данные"], &["итог"]),
            b("A4", "отчитаться", &["вердикт"], &["побочный"]),
        ],
    );
    let v = validate_model(&m);
    assert!(
        v.iter().any(|x| x.block == "A4"),
        "A4 читает выход branch-блока, не будучи его веткой — ребра не будет: {v:#?}"
    );
}

/// Обратная сторона атаки 4: потребитель, который ЯВЛЯЕТСЯ целью ветки,
/// ребро получает, и нарушением это быть не должно.
#[test]
fn branch_target_consumer_is_legal() {
    let mut br = b("A2", "решить", &["данные"], &["вердикт"]);
    br.kind = Some("branch".into());
    br.branches = vec![("ok".into(), "A3".into())];
    let m = model(
        &["старт"],
        &["итог"],
        vec![
            b("A1", "подготовить", &["старт"], &["данные"]),
            br,
            b("A3", "закрыть", &["вердикт"], &["итог"]),
        ],
    );
    let v = validate_model(&m);
    assert!(
        !v.iter().any(|x| x.block == "A3"),
        "цель ветки получает Branch-ребро, ложной тревоги быть не должно: {v:#?}"
    );
}

/// Атака 5: изолированный блок — ни входов, ни потребителей выхода.
#[test]
fn attack_orphan_block_must_be_flagged() {
    let m = model(
        &["старт"],
        &["итог"],
        vec![
            b("A1", "полезный", &["старт"], &["итог"]),
            b("A2", "сирота", &[], &["никому"]),
        ],
    );
    let v = validate_model(&m);
    assert!(!v.is_empty(), "блок вне потока должен быть замечен: {v:#?}");
}

/// Атака 6: двухуровневая декомпозиция. flatten() кладёт в исполнение и
/// родителя, и его детей — родитель дублирует работу детей.
#[test]
fn attack_nested_parent_also_executes() {
    let mut parent = b("A1", "родитель", &["старт"], &["итог"]);
    parent.children = vec![b("A11", "ребёнок", &["старт"], &["итог"])];
    let m = model(&["старт"], &["итог"], vec![parent]);

    let v = validate_model(&m);
    let graph = map_to_graph(&m);
    // Родитель и ребёнок оба стали исполняемыми узлами и оба пишут «итог».
    assert!(
        !v.is_empty(),
        "родитель с детьми исполняется наравне с ними ({} узлов) — надо предупредить",
        graph.nodes.len()
    );
}

/// Атака 7: модель, прошедшая валидацию, обязана давать исполнимый граф.
/// Это главный контракт: «валидатор доволен» ⇒ «Scheduler не падает».
#[test]
fn attack_valid_model_always_runs() {
    let m = model(
        &["старт"],
        &["итог"],
        vec![
            b("A1", "первый", &["старт"], &["mid"]),
            b("A2", "второй", &["mid"], &["итог"]),
        ],
    );
    let v = validate_model(&m);
    assert!(!has_errors(&v), "модель корректна: {v:#?}");

    let mut graph = map_to_graph(&m);
    let mut backend = MockBackend::default();
    let mut tools = ToolRegistry::default();
    let mut sched = Scheduler::new(&mut backend, &mut tools);
    let res = sched.run(&mut graph, "attack");
    assert!(res.is_ok(), "валидная модель не исполнилась: {:?}", res.err());
}

/// Подтверждение, что атака 1 — реальный сбой, а не придирка:
/// циклическая модель действительно не исполняется Scheduler-ом.
#[test]
fn cycle_model_really_breaks_scheduler() {
    let m = model(
        &["старт"],
        &["итог"],
        vec![
            b("A1", "первый", &["старт", "y"], &["x"]),
            b("A2", "второй", &["x"], &["y", "итог"]),
        ],
    );
    let mut graph = map_to_graph(&m);
    let mut backend = MockBackend::default();
    let mut tools = ToolRegistry::default();
    let mut sched = Scheduler::new(&mut backend, &mut tools);
    let res = sched.run(&mut graph, "attack");
    assert!(res.is_err(), "ожидался отказ на цикле, получено: {res:?}");
    eprintln!("ФАКТ: {}", res.unwrap_err());
}

// --- Рандомизированная проверка главного инварианта -----------------------
// «Если валидатор не нашёл ошибок, Scheduler обязан исполнить граф.»
// Детерминированный ГПСЧ (без внешних крейтов): один и тот же посев даёт
// одну и ту же серию, поэтому падение всегда воспроизводимо по seed.

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn upto(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Корректная по построению цепочка A0 -> A1 -> ... -> An, затем ОДНА
/// случайная мутация. Чисто случайные модели не годятся: при узком алфавите
/// потоков валидатор отвергает практически все (проверено — 0 принятых из
/// 3000), и инвариант остаётся непроверенным. Мутация даёт смесь, в которой
/// есть и валидные модели, и дефектные, причём дефекты — реалистичные.
fn random_model(rng: &mut Rng) -> Idef0Model {
    let n = 2 + rng.upto(5);
    let flow = |i: usize| format!("f{i}");
    let mut children: Vec<Block> = (0..n)
        .map(|i| {
            let ins = if i == 0 { "старт".to_string() } else { flow(i - 1) };
            let outs = if i == n - 1 { "итог".to_string() } else { flow(i) };
            b(
                &format!("B{i}"),
                &format!("шаг {i}"),
                &[ins.as_str()],
                &[outs.as_str()],
            )
        })
        .collect();

    match rng.upto(8) {
        // 0 — без мутации: заведомо валидная модель.
        0 => {}
        // 1 — опечатка в имени потока (самый частый реальный дефект).
        1 => {
            let i = rng.upto(n);
            children[i].outputs = vec![format!("{}_опечатка", flow(i))];
        }
        // 2 — цикл: ранний блок начинает читать поздний поток.
        2 if n > 2 => {
            let last = flow(n - 2);
            children[0].inputs.push(last);
        }
        // 3 — петля на себе.
        3 => {
            let i = rng.upto(n);
            let own = children[i].outputs[0].clone();
            children[i].inputs.push(own);
        }
        // 4 — дублирующий источник потока.
        4 if n > 1 => {
            let i = rng.upto(n - 1);
            children[i].outputs.push(flow(i + 1 - 1));
        }
        // 5 — сирота.
        5 => children.push(b("BX", "сирота", &[], &["ничей"])),
        // 6 — ветвление (иногда с целью «в никуда»).
        6 if n > 1 => {
            let i = rng.upto(n);
            children[i].kind = Some("branch".into());
            let target = if rng.upto(3) == 0 {
                "B999".to_string()
            } else {
                format!("B{}", rng.upto(n))
            };
            children[i].branches = vec![("ok".into(), target)];
        }
        // 7 — пустая формулировка функции.
        _ => {
            let i = rng.upto(n);
            children[i].function = String::new();
        }
    }
    model(&["старт"], &["итог"], children)
}

#[test]
fn invariant_accepted_model_always_executes() {
    let mut rng = Rng(0x5EED_1234_ABCD_0001);
    let mut accepted = 0usize;
    let mut rejected = 0usize;

    for case in 0..3000 {
        let m = random_model(&mut rng);
        let v = validate_model(&m);
        if has_errors(&v) {
            rejected += 1;
            continue;
        }
        accepted += 1;
        let mut graph = map_to_graph(&m);
        let mut backend = MockBackend::default();
        let mut tools = ToolRegistry::default();
        let mut sched = Scheduler::new(&mut backend, &mut tools);
        let res = sched.run(&mut graph, "fuzz");
        assert!(
            res.is_ok(),
            "НАРУШЕН ИНВАРИАНТ (случай {case}): валидатор принял модель, \
             планировщик отказал: {:?}\nмодель: {m:#?}\nзамечания: {v:#?}",
            res.err()
        );
    }
    // Обе доли должны быть непустыми: если принято 0 — тест ничего не проверяет,
    // если отвергнуто 0 — генератор не производит дефектных моделей.
    assert!(accepted > 100, "принято слишком мало моделей: {accepted}");
    assert!(rejected > 100, "отвергнуто слишком мало моделей: {rejected}");
    eprintln!("ФАКТ: принято {accepted}, отвергнуто {rejected} из 3000");
}

/// Обратный инвариант — против ложных тревог: если валидатор отверг модель
/// ИМЕННО за цикл (и больше ни за что), планировщик обязан на ней отказать.
/// Ошибочный диагноз «цикл» на исполнимой модели заблокировал бы работающий
/// процесс — для валидатора это худший вид дефекта.
#[test]
fn invariant_cycle_verdict_is_never_false_alarm() {
    use luck_pilot::icom::{Rule, Severity};
    let mut rng = Rng(0xC0FF_EE00_1234_5678);
    let mut checked = 0usize;

    for case in 0..3000 {
        let m = random_model(&mut rng);
        let v = validate_model(&m);
        let errs: Vec<_> = v.iter().filter(|x| x.severity == Severity::Error).collect();
        if errs.is_empty() || !errs.iter().all(|x| x.rule == Rule::Cycle) {
            continue;
        }
        checked += 1;
        let mut graph = map_to_graph(&m);
        let mut backend = MockBackend::default();
        let mut tools = ToolRegistry::default();
        let mut sched = Scheduler::new(&mut backend, &mut tools);
        let res = sched.run(&mut graph, "fuzz");
        assert!(
            res.is_err(),
            "ЛОЖНАЯ ТРЕВОГА (случай {case}): валидатор объявил цикл, \
             а планировщик исполнил модель без ошибок\nмодель: {m:#?}"
        );
    }
    assert!(checked > 20, "слишком мало моделей с вердиктом «цикл»: {checked}");
    eprintln!("ФАКТ: вердикт «цикл» проверен на {checked} моделях, ложных тревог нет");
}
