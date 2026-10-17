pub(super) mod edge_style;
pub(super) mod exporter;
pub(super) mod structure;

#[cfg(test)]
#[expect(clippy::panic_in_result_fn, reason = "Tests use assertions for expectations while returning Result for fallible setup.")]
pub(super) mod grouping_fixture;
#[cfg(test)]
pub(super) mod serialization_fixture;

#[cfg(test)]
pub(super) use exporter::tests::render;
#[cfg(test)]
pub(super) use exporter::tests::render_grouped;
