//! luck-pilot — рабочий прототип пайпа бизнес-инкубатора (HoReCa) поверх
//! КАНОНИЧЕСКОГО luck-engine (vendor/luck-engine, crate `luck`, path-
//! зависимость `luck-engine` в Cargo.toml).
//!
//! История: раньше luck-pilot был автономным форком подмножества Luck
//! (собственные luck_plan/luck_compile/luck_scheduler.rs). Эти три модуля
//! удалены — их заменяет vendor/luck-engine целиком (парсер, реестр типов
//! узлов, constrained decoding, планировщик). vendor/luck-engine НИКОГДА
//! не модифицируется (read-only, проектная политика).
//!
//! Что осталось специфичным для luck-pilot (не часть vendor-движка):
//!   - `verify` — VERIFY-предикаты бизнес-домена (HoReCa) + enforcement
//!     слота VERIFY, который vendor только парсит, но не проверяет.
//!   - `idef0` — маппер IDEF0 (ICOM-блоки) -> граф luck-engine.
//!   - `openrouter` — ChatTransport для OpenRouter/Ollama, обёрнутый в
//!     vendor-овский `anthropic::ValidatingBackend`.

pub mod idef0;
pub mod openrouter;
pub mod verify;

// Реэкспорт публичного API движка — так, чтобы `luck_pilot::{parse, Node, ...}`
// работал без прямой зависимости вызывающего кода от имени пакета `luck`.
pub use luck_engine::parser::{parse, parse_fragment, Edge, EdgeType, IntentGraph, LuckParseError, Node, SlotData};
pub use luck_engine::registry::{Kind, Subtype};
pub use luck_engine::scheduler::{
    ComputationCache, ExecutionResult, InvalidModelOutput, MockBackend, ModelBackend, Scheduler,
    ToolRegistry,
};
pub use verify::{run_verified, VerifiedOutcome};
