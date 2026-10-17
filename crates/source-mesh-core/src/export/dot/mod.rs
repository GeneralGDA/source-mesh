mod exporter;

#[cfg(test)]
#[expect(clippy::panic_in_result_fn, reason = "Tests use assertions for expectations while returning Result for fallible setup.")]
mod tests;

pub use exporter::Dot;
#[cfg(test)]
pub(super) use tests::dot_membership;
#[cfg(test)]
pub(super) use tests::dot_statements;
