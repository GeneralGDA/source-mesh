#![doc = include_str!("../README.md")]

mod analyzer_config;
mod analyzer_definitions;
mod analyzer_diagnostics;
pub mod analyzer_options;
mod analyzer_prerequisites;
mod analyzer_process;
pub mod backend;
mod diagnostics;
pub mod export;
mod graph;
pub mod scip_backend;
mod scip_definitions;
mod scip_index;
mod scip_path;
mod scip_repair;
mod test_dependencies;

mod application;
mod output_paths;
mod parallel;

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
pub use graph::source_path::SourcePath;
