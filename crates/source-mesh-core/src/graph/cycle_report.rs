use std::fmt::Write as _;
use std::num::NonZeroUsize;

use anyhow::Context as _;
use anyhow::Result;

use crate::cycle_presentation::group_color;
use crate::cycles::CycleAnalysis;
use crate::model::FileNode;
use crate::model::FolderNode;

pub const NO_CYCLES_FOUND: &str = "No cycles found.";

pub fn folder_report(analysis: &CycleAnalysis<FolderNode>, limit: NonZeroUsize) -> Result<String> {
    Ok(format!(
        "# Folder dependency cycles\n\n\
        These cycles belong to the final folder graph, after root filtering and depth folding. \
        Folder aggregation can introduce cycles even when the file graph is acyclic.\n\n{}",
        render_cycles(analysis, limit, |node| Ok(node.label().as_str()))?
    ))
}

pub fn file_report(analysis: &CycleAnalysis<FileNode>, limit: NonZeroUsize) -> Result<String> {
    Ok(format!(
        "# File dependency cycles\n\n\
        These cycles belong to the file graph before folder aggregation; folder depth does not apply. \
        Paths are relative to the indexed project, including their complete folder hierarchy.\n\n{}",
        render_cycles(analysis, limit, |node| node.path().to_str().context("Cycle file path is not UTF-8"))?
    ))
}

fn render_cycles<Node: Clone + Ord>(
    analysis: &CycleAnalysis<Node>,
    limit: NonZeroUsize,
    label: impl Fn(&Node) -> Result<&str>,
) -> Result<String> {
    let cycles = analysis.cycles(limit)?;
    if cycles.is_empty() {
        return Ok(format!("{NO_CYCLES_FOUND}\n"));
    }
    let mut output = String::new();
    let mut previous_group = None;
    for cycle in &cycles {
        let group = cycle.group();
        if previous_group != Some(group) {
            if previous_group.is_some() {
                output.push('\n');
            }
            writeln!(output, "## C{} ({})\n", group.number(), group_color(group, analysis.group_count()))?;
            previous_group = Some(group);
        }
        output.push_str("- ");
        for (index, node) in cycle.nodes().iter().enumerate() {
            if index != 0 {
                output.push_str(" -> ");
            }
            output.push_str(&inline_code(label(node)?)?);
        }
        output.push('\n');
    }
    Ok(output)
}

fn inline_code(label: &str) -> Result<String> {
    let visible_label = label.replace('\r', "\\r").replace('\n', "\\n").replace('\t', "\\t");
    let mut output = String::from("<code>");
    for character in quick_xml::escape::escape(&visible_label).chars() {
        match character {
            '`' | '*' | '_' | '[' | ']' | '\\' | '~' | '|' => write!(output, "&#{};", u32::from(character))?,
            _ => output.push(character),
        }
    }
    output.push_str("</code>");
    Ok(output)
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
    use std::path::Path;

    use anyhow::Context as _;
    use anyhow::Result;
    use pretty_assertions::assert_eq;
    use rstest::rstest;

    use super::file_report;
    use super::folder_report;
    use super::NO_CYCLES_FOUND;
    use crate::cycle_presentation::group_color;
    use crate::cycles::analyze_folders;
    use crate::cycles::analyze_files;
    use crate::model::DependencyCount;
    use crate::model::DependencyKind;
    use crate::model::FileEdge;
    use crate::model::FileGraph;
    use crate::model::FileNode;
    use crate::model::FolderEdge;
    use crate::model::FolderGraph;
    use crate::model::FolderNode;

    #[test]
    fn test_cycle_group_diagram_colors() -> Result<()> {
        let nodes = BTreeSet::from([FolderNode::new("api".into()), FolderNode::new("storage".into())]);
        let dependencies: BTreeMap<_, _> = nodes
            .iter()
            .map(|node| (
                FolderEdge::new(node.clone(), node.clone(), DependencyKind::Production),
                DependencyCount::new(1),
            ))
            .collect();
        let fixture = FolderGraph::new(nodes, dependencies)?;
        let analysis = analyze_folders(&fixture)?;
        let system_under_test = folder_report;

        let output = system_under_test(&analysis, NonZeroUsize::MAX)?;

        for cycle in analysis.cycles(NonZeroUsize::MAX)? {
            let group = cycle.group();
            let color = group_color(group, analysis.group_count());
            assert!(output.contains(&format!("## C{} ({color})", group.number())), "Cycle report color differs from the diagram palette");
            assert!(color.starts_with('#') && color.len() == 7, "Cycle report color must use six-digit hexadecimal RGB");
        }
        Ok(())
    }

    enum ReportKind {
        Folder,
        File,
    }

    fn folder_fixture(dependencies: &[(&str, &str)]) -> Result<FolderGraph> {
        let nodes = dependencies
            .iter()
            .flat_map(|&(source, target)| [FolderNode::new(source.into()), FolderNode::new(target.into())])
            .collect();
        let edges = dependencies
            .iter()
            .map(|&(source, target)| (
                FolderEdge::new(FolderNode::new(source.into()), FolderNode::new(target.into()), DependencyKind::Production),
                DependencyCount::new(1),
            ))
            .collect();
        FolderGraph::new(nodes, edges)
    }

    fn file_fixture(dependencies: &[(&FileNode, &FileNode)]) -> Result<FileGraph> {
        let nodes = dependencies.iter().flat_map(|&(source, target)| [source.clone(), target.clone()]).collect();
        let edges = dependencies
            .iter()
            .map(|&(source, target)| FileEdge::new(source.clone(), target.clone(), DependencyKind::Production))
            .collect();
        FileGraph::new(nodes, edges)
    }

    fn overlapping_folder_fixture() -> Result<FolderGraph> {
        folder_fixture(&[("api", "domain"), ("domain", "api"), ("domain", "storage"), ("storage", "api")])
    }

    fn report_chains(report: &str) -> Result<Vec<Vec<String>>> {
        report
            .lines()
            .filter(|line| line.starts_with("- "))
            .map(|line| {
                let cycle_markup = format!("<cycle>{line}</cycle>");
                let cycle = roxmltree::Document::parse(&cycle_markup)?;
                cycle
                    .descendants()
                    .filter(|node| node.has_tag_name("code"))
                    .map(|node| Ok(node.text().context("Cycle node label is missing")?.to_owned()))
                    .collect()
            })
            .collect()
    }

    #[test]
    fn test_folder_closed_chains_on_separate_lines() -> Result<()> {
        let fixture = overlapping_folder_fixture()?;
        let analysis = analyze_folders(&fixture)?;
        let system_under_test = folder_report;

        let output = system_under_test(&analysis, NonZeroUsize::MAX)?;
        let chains = report_chains(&output)?;

        assert_eq!(
            vec![
                vec![String::from("api"), String::from("domain"), String::from("api")],
                vec![String::from("api"), String::from("domain"), String::from("storage"), String::from("api")],
            ],
            chains,
            "Report did not preserve both complete closed chains: {chains:?}"
        );
        assert_eq!(1, output.lines().filter(|line| line.starts_with("## ")).count(), "Connected cycles must share one group");
        Ok(())
    }

    #[test]
    fn test_cycle_group_sections() -> Result<()> {
        let fixture = folder_fixture(&[("api", "domain"), ("domain", "api"), ("queue", "storage"), ("storage", "queue")])?;
        let analysis = analyze_folders(&fixture)?;
        let system_under_test = folder_report;

        let output = system_under_test(&analysis, NonZeroUsize::MAX)?;
        let (first_section, last_section) = output.split_once("## C2 (").context("Second cycle group is missing")?;
        let first_chains = report_chains(first_section)?;
        let last_chains = report_chains(last_section)?;

        assert!(first_section.contains("## C1 ("), "First cycle group is missing");
        assert_eq!(vec![vec![String::from("api"), String::from("domain"), String::from("api")]], first_chains, "First group contains incorrect chains");
        assert_eq!(vec![vec![String::from("queue"), String::from("storage"), String::from("queue")]], last_chains, "Second group contains incorrect chains");
        Ok(())
    }

    #[rstest]
    #[case("unsafe`<>&\n\r\t", "unsafe&#96;&lt;&gt;&amp;&#92;n&#92;r&#92;t")]
    #[case("unsafe*[link]_\\~|", "unsafe&#42;&#91;link&#93;&#95;&#92;&#126;&#124;")]
    #[case("unsafe\n## injected\n<script>alert(1)</script>", "unsafe&#92;n## injected&#92;n&lt;script&gt;alert(1)&lt;/script&gt;")]
    fn test_folder_label_markdown_escaping(#[case] label: &str, #[case] escaped: &str) -> Result<()> {
        let fixture = folder_fixture(&[(label, "safe"), ("safe", label)])?;
        let analysis = analyze_folders(&fixture)?;
        let system_under_test = folder_report;

        let output = system_under_test(&analysis, NonZeroUsize::MAX)?;
        let chains = report_chains(&output)?;

        assert!(output.contains(&format!("<code>{escaped}</code>")), "Folder label escaped incorrectly: {output}");
        assert_eq!(1, chains.len(), "A label changed the number of cycle lines");
        assert_eq!(1, output.lines().filter(|line| line.starts_with("## ")).count(), "A label injected a group heading");
        assert!(!output.contains("<script>"), "A label injected HTML markup");
        Ok(())
    }

    #[rstest]
    #[case(ReportKind::Folder)]
    #[case(ReportKind::File)]
    fn test_empty_cycle_report(#[case] kind: ReportKind) -> Result<()> {
        let folder_analysis = analyze_folders(&FolderGraph::default())?;
        let file_analysis = analyze_files(&FileGraph::new(BTreeSet::new(), BTreeSet::new())?)?;

        let output = match kind {
            ReportKind::Folder => folder_report(&folder_analysis, NonZeroUsize::MAX)?,
            ReportKind::File => file_report(&file_analysis, NonZeroUsize::MAX)?,
        };
        let chains = report_chains(&output)?;

        assert_eq!(Some(NO_CYCLES_FOUND), output.lines().last(), "Empty report must explicitly describe the absence of cycles");
        assert!(chains.is_empty(), "Empty report contains a cycle chain");
        assert!(!output.lines().any(|line| line.starts_with("## ")), "Empty report contains a cycle group");
        Ok(())
    }

    #[test]
    fn test_file_cycle_complete_project_paths() -> Result<()> {
        let service = FileNode::new(Path::new("src").join("api").join("deep").join("service.rs"))?;
        let database = FileNode::new(Path::new("src").join("storage").join("database.rs"))?;
        let fixture = file_fixture(&[(&service, &database), (&database, &service)])?;
        let analysis = analyze_files(&fixture)?;
        let service_path = service.path().to_str().context("Service fixture path is not UTF-8")?;
        let expected_chain = vec![
            service_path.to_owned(),
            database.path().to_str().context("Database fixture path is not UTF-8")?.to_owned(),
            service_path.to_owned(),
        ];
        let system_under_test = file_report;

        let output = system_under_test(&analysis, NonZeroUsize::MAX)?;
        let chains = report_chains(&output)?;

        assert_eq!(vec![expected_chain], chains, "File report changed project-relative source paths or omitted cycle closure");
        Ok(())
    }

    #[test]
    fn test_file_path_markdown_escaping() -> Result<()> {
        let service = FileNode::new(Path::new("src").join("unsafe`<>&\n.rs"))?;
        let database = FileNode::new(Path::new("src").join("database.rs"))?;
        let fixture = file_fixture(&[(&service, &database), (&database, &service)])?;
        let analysis = analyze_files(&fixture)?;
        let system_under_test = file_report;

        let output = system_under_test(&analysis, NonZeroUsize::MAX)?;
        let chains = report_chains(&output)?;

        assert!(output.contains("unsafe&#96;&lt;&gt;&amp;&#92;n.rs</code>"), "File path markup or newline was not escaped");
        assert_eq!(1, chains.len(), "File path introduced another report line");
        Ok(())
    }

    #[rstest]
    #[case(None, vec![])]
    #[case(Some(1), vec![vec!["api", "domain", "api"]])]
    fn test_folder_aggregation_cycle_without_file_cycle(
        #[case] depth: Option<usize>,
        #[case] expected_chains: Vec<Vec<&str>>,
    ) -> Result<()> {
        let service = FileNode::new(Path::new("src").join("api").join("service.rs"))?;
        let request = FileNode::new(Path::new("src").join("domain").join("deep").join("request.rs"))?;
        let rules = FileNode::new(Path::new("src").join("domain").join("rules.rs"))?;
        let response = FileNode::new(Path::new("src").join("api").join("response.rs"))?;
        let fixture = file_fixture(&[(&service, &request), (&rules, &response)])?;
        let folder_analysis = analyze_folders(&fixture.folders(Path::new("src"), depth, false)?)?;
        let file_analysis = analyze_files(&fixture)?;
        let system_under_test = folder_report;

        let folder_output = system_under_test(&folder_analysis, NonZeroUsize::MAX)?;
        let file_output = file_report(&file_analysis, NonZeroUsize::MAX)?;
        let folder_chains = report_chains(&folder_output)?;
        let expected_chains: Vec<Vec<String>> = expected_chains.into_iter()
            .map(|chain| chain.into_iter().map(str::to_owned).collect())
            .collect();

        assert_eq!(expected_chains, folder_chains, "Folder report did not reflect the aggregation depth");
        assert_eq!(Some(NO_CYCLES_FOUND), file_output.lines().last(), "File report inherited a cycle created by folder aggregation");
        Ok(())
    }

    #[test]
    fn test_cycle_limit_error_propagation() -> Result<()> {
        let fixture = overlapping_folder_fixture()?;
        let analysis = analyze_folders(&fixture)?;
        let system_under_test = folder_report;

        let result = system_under_test(&analysis, NonZeroUsize::MIN);

        assert!(result.is_err(), "Report silently truncated cycles above the requested limit");
        Ok(())
    }
}
