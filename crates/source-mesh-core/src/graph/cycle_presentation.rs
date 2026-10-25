use crate::cycles::CycleGroupId;

#[must_use]
pub(crate) fn group_color(group: CycleGroupId, group_count: usize) -> String {
    assert!(group.number().get() <= group_count, "Cycle group must belong to the palette");
    let color = colorous::SET1.get(group.number().get() - 1).copied().filter(|_| group_count <= colorous::SET1.len())
        .unwrap_or_else(|| colorous::RAINBOW.eval_rational(group.number().get() - 1, group_count));
    format!("#{color:X}")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::collections::BTreeSet;
    use std::num::NonZeroUsize;

    use claims::assert_ok;
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
    #[case::single_group(1, 1, "#E41A1C")]
    #[case::second_group(2, 2, "#377EB8")]
    #[case::below_palette_limit(8, 8, "#F781BF")]
    #[case::set1_first(9, 1, "#E41A1C")]
    #[case::set1_second(9, 2, "#377EB8")]
    #[case::set1_third(9, 3, "#4DAF4A")]
    #[case::set1_fourth(9, 4, "#984EA3")]
    #[case::set1_fifth(9, 5, "#FF7F00")]
    #[case::set1_sixth(9, 6, "#FFFF33")]
    #[case::set1_seventh(9, 7, "#A65628")]
    #[case::set1_eighth(9, 8, "#F781BF")]
    #[case::set1_last(9, 9, "#999999")]
    #[case::rainbow_boundary(10, 1, "#6D3FA9")]
    #[case::larger_palette(32, 1, "#6D3FA9")]
    fn test_component_palette(#[case] component_count: usize, #[case] group_number: usize, #[case] expected_color: &str) {
        let nodes: BTreeSet<_> = (0..component_count).map(|index| FolderNode::new(format!("component{index}"))).collect();
        let edges: BTreeMap<_, _> = nodes.iter().map(|node| (
            FolderEdge::new(node.clone(), node.clone(), DependencyKind::Production),
            DependencyCount::new(1),
        )).collect();
        let graph = FolderGraph::new(nodes, edges);
        let graph = assert_ok!(graph, "Independent self-loop components must form a valid graph");
        let fixture = analyze_folders(&graph);
        let fixture = assert_ok!(fixture, "Independent self-loop components must support cycle analysis");
        let system_under_test = group_color;

        let cycles = fixture.cycles(NonZeroUsize::MAX);
        let cycles = assert_ok!(cycles, "Every independent component must yield its self-loop cycle");
        let colors_by_group: BTreeMap<_, _> = cycles.iter().map(|cycle| {
            (cycle.group().number().get(), system_under_test(cycle.group(), fixture.group_count()))
        }).collect();
        let colors: BTreeSet<_> = colors_by_group.values().collect();
        let selected_color = colors_by_group.get(&group_number).map(String::as_str);

        assert_eq!(selected_color, Some(expected_color), "Cycle group numbering or palette selection changed");
        assert_eq!(component_count, colors.len(), "Independent components share a color");
        assert!(colors.iter().all(|color| color.starts_with('#') && color.len() == 7
            && color.chars().skip(1).all(|character| character.is_ascii_hexdigit())),
            "Cycle colors must be RGB hex values");
    }
}
