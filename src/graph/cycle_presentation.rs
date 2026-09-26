use crate::cycles::CycleGroupId;

#[must_use]
pub(crate) fn group_color(group: CycleGroupId, group_count: usize) -> String {
    assert!(group.number().get() <= group_count, "Cycle group must belong to the palette");
    let color = colorous::SET1.get(group.number().get() - 1).copied()
        .filter(|_| group_count <= colorous::SET1.len())
        .unwrap_or_else(|| colorous::RAINBOW.eval_rational(group.number().get() - 1, group_count));
    format!("#{color:X}")
}

#[cfg(test)]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Tests use assertions for expectations while returning Result for fallible setup."
)]
mod tests {
    use std::collections::BTreeMap;
    use std::collections::BTreeSet;
    use std::num::NonZeroUsize;

    use anyhow::Result;
    use pretty_assertions::assert_eq;
    use rstest::rstest;

    use super::group_color;
    use crate::cycles::analyze_folders;
    use crate::model::DependencyCount;
    use crate::model::DependencyKind;
    use crate::model::FolderEdge;
    use crate::model::FolderGraph;
    use crate::model::FolderNode;

    #[rstest]
    #[case(1)]
    #[case(2)]
    #[case(9)]
    #[case(10)]
    #[case(32)]
    fn test_distinct_component_colors(#[case] component_count: usize) -> Result<()> {
        let nodes: BTreeSet<_> = (0..component_count)
            .map(|index| FolderNode::new(format!("component{index}"))).collect();
        let edges: BTreeMap<_, _> = nodes.iter().map(|node| (
            FolderEdge::new(node.clone(), node.clone(), DependencyKind::Production),
            DependencyCount::new(1),
        )).collect();
        let fixture = analyze_folders(&FolderGraph::new(nodes, edges)?)?;
        let system_under_test = group_color;

        let colors: BTreeSet<_> = fixture.cycles(NonZeroUsize::MAX)?.iter()
            .map(|cycle| system_under_test(cycle.group(), fixture.group_count())).collect();

        assert_eq!(component_count, colors.len(), "Independent components share a color");
        assert!(colors.iter().all(|color| color.starts_with('#') && color.len() == 7
            && color.chars().skip(1).all(|character| character.is_ascii_hexdigit())),
            "Cycle colors must be RGB hex values");
        Ok(())
    }
}
