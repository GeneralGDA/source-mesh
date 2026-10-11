#![doc = include_str!("../README.md")]

mod analyzer_config;
mod analyzer_definitions;
mod analyzer_prerequisites;
mod analyzer_process;
pub mod analyzer_options;
pub mod backend;
pub mod export;
mod graph;
pub mod scip_backend;
mod scip_definitions;
mod scip_repair;
mod scip_path;
mod test_dependencies;

mod application;
mod output_paths;

#[cfg(test)]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Tests use assertions for expectations while returning Result for fallible setup."
)]
mod application_tests;

pub use application::CycleOptions;
pub use application::DEFAULT_CYCLE_LIMIT;
pub use application::Input;
pub use application::Project;
pub use application::Request;
pub use application::run;

pub(crate) use graph::cycle_presentation;
pub use graph::cycle_report;
pub use graph::cycles;
pub use graph::model;
