mod dot;
mod edge_style;
mod exporter;
mod format;
mod graphml;
mod mermaid;
mod structure;

#[cfg(test)]
#[expect(clippy::panic_in_result_fn, reason = "Tests use assertions for expectations while returning Result for fallible setup.")]
mod tests;
#[cfg(test)]
#[expect(clippy::panic_in_result_fn, reason = "Tests use assertions for expectations while returning Result for fallible setup.")]
mod cycles_tests;
#[cfg(test)]
#[expect(clippy::panic_in_result_fn, reason = "Tests use assertions for expectations while returning Result for fallible setup.")]
mod group_graphml_tests;
#[cfg(test)]
#[expect(clippy::panic_in_result_fn, reason = "Tests use assertions for expectations while returning Result for fallible setup.")]
mod group_text_tests;
#[cfg(test)]
#[expect(clippy::panic_in_result_fn, reason = "Tests use assertions for expectations while returning Result for fallible setup.")]
mod serialization_tests;

pub use dot::Dot;
pub use exporter::Options;
pub use exporter::Exporter;
pub use format::Format;
pub use graphml::GraphMl;
pub use mermaid::Mermaid;
