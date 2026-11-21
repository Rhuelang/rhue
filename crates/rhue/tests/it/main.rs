//! The integration tests, one module per feature, all in one binary so they
//! only get linked once. Each module's doc says which milestone it came from.

mod async_hosts;
mod calls;
mod checker;
mod checker_scaling;
mod chunks;
mod clock;
mod coroutines;
mod debug_hooks;
mod embedding;
mod expression_type;
mod expressions;
mod fast_paths;
mod functions;
mod hot_reload;
mod metatables;
mod native_budget;
// the editor API is behind the `tooling` feature
#[cfg(feature = "tooling")]
mod outline;
mod patterns_and_modules;
mod protected_yields;
mod resume_errors;
mod statements;
mod stdlib;
mod table_templates;
mod tables;
mod tasks_and_environments;
mod thread_kills;
mod type_packs;
mod type_properties;
mod vectors;
