//! luck-pilot — автономный форк подмножества Luck (компилятор + планировщик).
//!
//! Происхождение: ~/ws1/ai-agent/src/{luck_plan,luck_compile,luck_scheduler}.rs
//! (порт Luck в ai-agent) — скопировано как прототип для пилота HoReCa
//! (бизнес-инкубатор). Нативные проекты (luck-репо Python, luck-repo Rust,
//! ai-agent) НЕ модифицируются.
//!
//! Отличия от оригинала: вырезан AiAgentRuntime (зависимости от
//! provider/tool_routing/types ai-agent); остался PlanRuntime trait —
//! реализации пишутся под пайп (см. тесты: MockRuntime/EmptyRuntime).

pub mod luck_plan;
pub mod luck_compile;
pub mod luck_scheduler;

pub use luck_plan::{Node, NodeKind, Plan, Policy, VerifySpec, validate, parse_and_validate};
pub use luck_compile::compile;
pub use luck_scheduler::{PlanExecutor, PlanOutcome, PlanEvent, PlanRuntime};
