mod exporter;

#[cfg(test)]
#[expect(clippy::panic_in_result_fn, reason = "Tests use assertions for expectations while returning Result for fallible setup.")]
mod tests;

pub use exporter::GraphMl;
#[cfg(test)]
pub(super) use exporter::GRAPHML_NAMESPACE;
#[cfg(test)]
pub(super) use exporter::YWORKS_NAMESPACE;
