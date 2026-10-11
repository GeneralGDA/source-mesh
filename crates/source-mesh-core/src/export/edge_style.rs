use crate::model::DependencyCount;
use crate::model::DependencyKind;

pub(super) const TEST_EDGE_COLOR: &str = "#D97706";
pub(super) const CYCLE_EDGE_WIDTH: &str = "3.0";

#[must_use]
pub(super) const fn kind_name(kind: DependencyKind) -> &'static str {
    match kind {
        DependencyKind::Production => "production",
        DependencyKind::Test => "test",
    }
}

#[must_use]
pub(super) fn edge_label(kind: DependencyKind, weight: DependencyCount) -> String {
    match kind {
        DependencyKind::Production => weight.value().to_string(),
        DependencyKind::Test => format!("{}: {}", kind_name(kind), weight.value()),
    }
}
