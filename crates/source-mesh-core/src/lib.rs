#![doc = include_str!("../README.md")]

mod analyzer;
mod application;
pub mod backend;
mod diagnostics;
pub mod export;
mod graph;
mod output_paths;
mod scip;

pub use analyzer::options as analyzer_options;
pub use scip::backend as scip_backend;

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
