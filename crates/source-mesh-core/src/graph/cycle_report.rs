use std::fmt::Write as _;
use std::num::NonZeroUsize;

use anyhow::Context as _;
use anyhow::Result;

use super::markdown::inline_code;
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
    let listing = analysis.bounded_cycles(limit);
    if listing.cycles().is_empty() {
        return Ok(format!("{NO_CYCLES_FOUND}\n"));
    }
    let mut output = String::new();
    if listing.truncated() {
        write!(
            output,
            "> **Warning: cycle listing truncated at {} chains.** More cycles exist; \
            the total cycle count was not computed. Complete cycle group count: {}.\n\n",
            listing.cycles().len(),
            analysis.group_count(),
        ).unwrap_or_else(|_| unreachable!("Formatting a cycle warning into a String cannot fail"));
    }
    let mut previous_group = None;
    for cycle in listing.cycles() {
        let group = cycle.group();
        if previous_group != Some(group) {
            if previous_group.is_some() {
                output.push('\n');
            }
            let color = group_color(group, analysis.group_count());
            write!(output, "## C{} ({color})\n\n", group.number())
                .unwrap_or_else(|_| unreachable!("Formatting a cycle heading into a String cannot fail"));
            previous_group = Some(group);
        }
        output.push_str("- ");
        for (index, node) in cycle.nodes().iter().enumerate() {
            if index != 0 {
                output.push_str(" -> ");
            }
            output.push_str(&inline_code(label(node)?));
        }
        output.push('\n');
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::collections::BTreeSet;
    use std::num::NonZeroUsize;
    use std::path::Path;

    use anyhow::Context as _;
    use anyhow::Result;
    use assertables::assert_contains;
    use assertables::assert_not_contains;
    use claims::assert_ok;
    use claims::assert_some;
    use nonzero_ext::nonzero;
    use pretty_assertions::assert_eq;
    use rstest::rstest;

    use super::NO_CYCLES_FOUND;
    use super::file_report;
    use super::folder_report;
    use crate::SourcePath;
    use crate::cycle_presentation::group_color;
    use crate::cycles::analyze_files;
    use crate::cycles::analyze_folders;
    use crate::model::DependencyCount;
    use crate::model::DependencyKind;
    use crate::model::FileEdge;
    use crate::model::FileGraph;
    use crate::model::FileNode;
    use crate::model::FolderEdge;
    use crate::model::FolderGraph;
    use crate::model::FolderNode;

    #[test]
    fn test_cycle_group_diagram_colors() {
        let nodes = BTreeSet::from([FolderNode::new("api".into()), FolderNode::new("storage".into())]);
        let dependencies: BTreeMap<_, _> = nodes.iter().map(|node| {
            let edge = FolderEdge::new(node.clone(), node.clone(), DependencyKind::Production);
            (edge, DependencyCount::new(1))
        }).collect();
        let fixture = FolderGraph::new(nodes, dependencies);
        let fixture = assert_ok!(fixture, "Independent self-loop folders must form a valid graph");
        let analysis = analyze_folders(&fixture);
        let analysis = assert_ok!(analysis, "Independent self-loop folders must support cycle analysis");
        let system_under_test = folder_report;

        let output = system_under_test(&analysis, NonZeroUsize::MAX);
        let output = assert_ok!(output, "Cycle report must render independent self-loop groups");

        let cycles = analysis.cycles(NonZeroUsize::MAX);
        let cycles = assert_ok!(cycles, "Both independent self-loop cycles must be enumerable");
        for cycle in cycles {
            let group = cycle.group();
            let color = group_color(group, analysis.group_count()).to_string();
            let heading = format!("## C{} ({color})", group.number());
            assert_contains!(output, &heading, "Cycle report color differs from the diagram palette");
            assert!(color.starts_with('#') && color.len() == 7, "Cycle report color must use six-digit hexadecimal RGB");
        }
    }

    enum ReportKind {
        Folder,
        File,
    }

    fn folder_fixture(dependencies: &[(&str, &str)]) -> Result<FolderGraph> {
        let nodes = dependencies.iter().flat_map(|&(source, target)| {
            [FolderNode::new(source.into()), FolderNode::new(target.into())]
        }).collect();
        let edges = dependencies.iter().map(|&(source, target)| {
            let source = FolderNode::new(source.into());
            let edge = FolderEdge::new(source, FolderNode::new(target.into()), DependencyKind::Production);
            (edge, DependencyCount::new(1))
        }).collect();
        FolderGraph::new(nodes, edges)
    }

    fn file_fixture(dependencies: &[(&FileNode, &FileNode)]) -> Result<FileGraph> {
        let nodes = dependencies.iter().flat_map(|&(source, target)| [source.clone(), target.clone()]).collect();
        let edges = dependencies.iter().map(|&(source, target)| {
            FileEdge::new(source.clone(), target.clone(), DependencyKind::Production)
        }).collect();
        FileGraph::new(nodes, edges)
    }

    fn overlapping_folder_fixture() -> Result<FolderGraph> {
        folder_fixture(&[("api", "domain"), ("domain", "api"), ("domain", "storage"), ("storage", "api")])
    }

    fn report_chains(report: &str) -> Result<Vec<Vec<String>>> {
        let cycle_lines = report.lines().filter(|line| line.starts_with("- "));
        cycle_lines.map(|line| {
            let cycle_markup = format!("<cycle>{line}</cycle>");
            let cycle = roxmltree::Document::parse(&cycle_markup)?;
            let nodes = cycle.descendants().filter(|node| node.has_tag_name("code"));
            nodes.map(|node| Ok(node.text().context("Cycle node label is missing")?.to_owned())).collect()
        }).collect()
    }

    fn assert_report_completeness(report: &str, group_count: usize, truncated: bool) {
        if truncated {
            let chain_count = report.lines().filter(|line| line.starts_with("- ")).count();
            let warning = format!("Warning: cycle listing truncated at {chain_count} chains.");
            assert_contains!(report, &warning, "Report is missing the truncation warning: {warning}");
            assert_contains!(report, "More cycles exist", "Report is missing the notice that more cycles exist");
            assert_contains!(report, "total cycle count was not computed", "Report omits the notice that no total was computed");
            let group_summary = format!("Complete cycle group count: {group_count}.");
            assert_contains!(report, &group_summary, "Report is missing the group summary: {group_summary}");
        } else {
            assert_not_contains!(report, "Warning:", "Complete report contains an unexpected warning");
            assert_not_contains!(report, "truncated", "Complete report contains unexpected truncation text");
        }
    }

    #[rstest]
    #[case::self_loop(
        &[("api", "api")],
        "- <code>api</code> -> <code>api</code>",
    )]
    #[case::two_nodes(
        &[("api", "domain"), ("domain", "api")],
        "- <code>api</code> -> <code>domain</code> -> <code>api</code>",
    )]
    #[case::three_nodes(
        &[("api", "domain"), ("domain", "storage"), ("storage", "api")],
        "- <code>api</code> -> <code>domain</code> -> <code>storage</code> -> <code>api</code>",
    )]
    fn test_cycle_chain_separators(#[case] dependencies: &[(&str, &str)], #[case] expected: &str) {
        let fixture = folder_fixture(dependencies);
        let fixture = assert_ok!(fixture, "Cycle-chain dependencies must form a valid folder graph");
        let analysis = analyze_folders(&fixture);
        let analysis = assert_ok!(analysis, "Cycle-chain graph must support analysis");
        let system_under_test = folder_report;

        let output = system_under_test(&analysis, NonZeroUsize::MAX);
        let output = assert_ok!(output, "Cycle-chain report must render every node and separator");
        let chains: Vec<_> = output.lines().filter(|line| line.starts_with("- ")).collect();

        assert_eq!(chains, vec![expected], "Arrows must separate every adjacent node, including the cycle closure");
    }

    #[test]
    fn test_folder_closed_chains_on_separate_lines() {
        let fixture = overlapping_folder_fixture();
        let fixture = assert_ok!(fixture, "Overlapping folder cycles must form a valid graph");
        let analysis = analyze_folders(&fixture);
        let analysis = assert_ok!(analysis, "Overlapping folder cycles must support analysis");
        let system_under_test = folder_report;

        let output = system_under_test(&analysis, NonZeroUsize::MAX);
        let output = assert_ok!(output, "Report must render every overlapping folder cycle");
        let chains = report_chains(&output);
        let chains = assert_ok!(chains, "Overlapping-cycle report must contain parseable node labels");

        assert_eq!(
            vec![
                vec![String::from("api"), String::from("domain"), String::from("api")],
                vec![String::from("api"), String::from("domain"), String::from("storage"), String::from("api")],
            ],
            chains,
            "Report did not preserve both complete closed chains: {chains:?}"
        );
        assert_eq!(1, output.lines().filter(|line| line.starts_with("## ")).count(), "Connected cycles must share one group");
    }

    #[test]
    fn test_cycle_group_sections() {
        let fixture = folder_fixture(&[("api", "domain"), ("domain", "api"), ("queue", "storage"), ("storage", "queue")]);
        let fixture = assert_ok!(fixture, "Two independent cycle groups must form a valid folder graph");
        let analysis = analyze_folders(&fixture);
        let analysis = assert_ok!(analysis, "Independent folder cycles must support analysis");
        let system_under_test = folder_report;

        let output = system_under_test(&analysis, NonZeroUsize::MAX);
        let output = assert_ok!(output, "Report must render both independent cycle groups");
        let sections = output.split_once("## C2 (");
        let (first_section, last_section) = assert_some!(sections, "Report must contain a second cycle group");
        let first_chains = report_chains(first_section);
        let first_chains = assert_ok!(first_chains, "First cycle-group section must contain parseable labels");
        let last_chains = report_chains(last_section);
        let last_chains = assert_ok!(last_chains, "Second cycle-group section must contain parseable labels");

        assert_contains!(first_section, "## C1 (", "First cycle group is missing");
        let first_expected = vec![vec![String::from("api"), String::from("domain"), String::from("api")]];
        assert_eq!(first_expected, first_chains, "First group contains incorrect chains");
        let last_expected = vec![vec![String::from("queue"), String::from("storage"), String::from("queue")]];
        assert_eq!(last_expected, last_chains, "Second group contains incorrect chains");
    }

    #[rstest]
    #[case(
        "unsafe`<>&\n\r\t",
        "unsafe&#96;&lt;&gt;&amp;&#92;n&#92;r&#92;t",
    )]
    #[case(
        "unsafe*[link]_\\~|",
        "unsafe&#42;&#91;link&#93;&#95;&#92;&#126;&#124;",
    )]
    #[case(
        "unsafe\n## injected\n<script>alert(1)</script>",
        "unsafe&#92;n## injected&#92;n&lt;script&gt;alert(1)&lt;/script&gt;",
    )]
    fn test_folder_label_markdown_escaping(#[case] label: &str, #[case] escaped: &str) {
        let fixture = folder_fixture(&[(label, "safe"), ("safe", label)]);
        let fixture = assert_ok!(fixture, "Folder labels containing Markdown must be valid graph labels");
        let analysis = analyze_folders(&fixture);
        let analysis = assert_ok!(analysis, "Markdown in folder labels must not prevent cycle analysis");
        let system_under_test = folder_report;

        let output = system_under_test(&analysis, NonZeroUsize::MAX);
        let output = assert_ok!(output, "Report must render escaped folder labels");
        let chains = report_chains(&output);
        let chains = assert_ok!(chains, "Escaped folder labels must remain parseable report markup");

        assert_contains!(output, &format!("<code>{escaped}</code>"), "Folder label escaped incorrectly: {output}");
        assert_eq!(1, chains.len(), "A label changed the number of cycle lines");
        assert_eq!(1, output.lines().filter(|line| line.starts_with("## ")).count(), "A label injected a group heading");
        assert_not_contains!(output, "<script>", "A label injected HTML markup");
    }

    #[rstest]
    #[case::folder(ReportKind::Folder, NonZeroUsize::MIN)]
    #[case::file(ReportKind::File, NonZeroUsize::MAX)]
    fn test_empty_cycle_report(#[case] kind: ReportKind, #[case] limit: NonZeroUsize) {
        let system_under_test = match kind {
            ReportKind::Folder => {
                let analysis = analyze_folders(&FolderGraph::default());
                let analysis = assert_ok!(analysis, "Cycle analysis failed for the empty folder graph");
                folder_report(&analysis, limit)
            }
            ReportKind::File => {
                let analysis = FileGraph::new(BTreeSet::new(), BTreeSet::new()).and_then(|graph| analyze_files(&graph));
                let analysis = assert_ok!(analysis, "Building and analyzing the empty file graph failed");
                file_report(&analysis, limit)
            }
        };
        let output = assert_ok!(system_under_test, "Cycle report generation failed for an empty graph");

        assert_eq!(Some(NO_CYCLES_FOUND), output.lines().last(), "Empty report did not end with the no-cycles message");
        let has_cycles = output.lines().any(|line| line.starts_with("- ") || line.starts_with("## "));
        assert!(!has_cycles, "Empty report contains a cycle chain or group");
        assert_report_completeness(&output, 0, false);
    }

    #[test]
    fn test_file_cycle_complete_project_paths() {
        let service_path = SourcePath::new(Path::new("src").join("api").join("deep").join("service.rs"));
        let service_path = assert_ok!(service_path, "Nested service path must be a valid source path");
        let service = FileNode::new(service_path);
        let database_path = SourcePath::new(Path::new("src").join("storage").join("database.rs"));
        let database_path = assert_ok!(database_path, "Database path must be a valid source path");
        let database = FileNode::new(database_path);
        let fixture = file_fixture(&[(&service, &database), (&database, &service)]);
        let fixture = assert_ok!(fixture, "Service/database cycle must form a valid file graph");
        let analysis = analyze_files(&fixture);
        let analysis = assert_ok!(analysis, "Service/database graph must support cycle analysis");
        let service_path_text = service.path().to_str();
        let service_path_text = assert_some!(service_path_text, "Service fixture path must be UTF-8");
        let database_path_text = database.path().to_str();
        let database_path_text = assert_some!(database_path_text, "Database fixture path must be UTF-8");
        let expected_chain = vec![service_path_text.to_owned(), database_path_text.to_owned(), service_path_text.to_owned()];
        let system_under_test = file_report;

        let output = system_under_test(&analysis, NonZeroUsize::MAX);
        let output = assert_ok!(output, "File cycle report must render complete project-relative paths");
        let chains = report_chains(&output);
        let chains = assert_ok!(chains, "File cycle report must contain parseable paths");

        assert_eq!(vec![expected_chain], chains, "File report changed project-relative source paths or omitted cycle closure");
    }

    #[test]
    fn test_file_path_markdown_escaping() {
        let service_path = SourcePath::new(Path::new("src").join("unsafe`<>&\n.rs"));
        let service_path = assert_ok!(service_path, "Markdown characters must be allowed in a lexical source path");
        let service = FileNode::new(service_path);
        let database_path = SourcePath::new(Path::new("src").join("database.rs"));
        let database_path = assert_ok!(database_path, "Database fixture path must be valid");
        let database = FileNode::new(database_path);
        let fixture = file_fixture(&[(&service, &database), (&database, &service)]);
        let fixture = assert_ok!(fixture, "Files with Markdown characters must form a valid graph");
        let analysis = analyze_files(&fixture);
        let analysis = assert_ok!(analysis, "Markdown in file paths must not prevent cycle analysis");
        let system_under_test = file_report;

        let output = system_under_test(&analysis, NonZeroUsize::MAX);
        let output = assert_ok!(output, "File report must render escaped Markdown characters");
        let chains = report_chains(&output);
        let chains = assert_ok!(chains, "Escaped file paths must remain parseable report markup");

        assert_contains!(output, "unsafe&#96;&lt;&gt;&amp;&#92;n.rs</code>", "File path markup or newline was not escaped");
        assert_eq!(1, chains.len(), "File path introduced another report line");
    }

    #[rstest]
    #[case(None, vec![])]
    #[case(Some(1), vec![vec!["api", "domain", "api"]])]
    fn test_folder_aggregation_cycle_without_file_cycle(#[case] depth: Option<usize>, #[case] expected_chains: Vec<Vec<&str>>) {
        let service_path = SourcePath::new(Path::new("src").join("api").join("service.rs"));
        let service_path = assert_ok!(service_path, "Service fixture path must be valid");
        let service = FileNode::new(service_path);
        let request_path = SourcePath::new(Path::new("src").join("domain").join("deep").join("request.rs"));
        let request_path = assert_ok!(request_path, "Nested request fixture path must be valid");
        let request = FileNode::new(request_path);
        let rules_path = SourcePath::new(Path::new("src").join("domain").join("rules.rs"));
        let rules_path = assert_ok!(rules_path, "Rules fixture path must be valid");
        let rules = FileNode::new(rules_path);
        let response_path = SourcePath::new(Path::new("src").join("api").join("response.rs"));
        let response_path = assert_ok!(response_path, "Response fixture path must be valid");
        let response = FileNode::new(response_path);
        let fixture = file_fixture(&[(&service, &request), (&rules, &response)]);
        let fixture = assert_ok!(fixture, "Acyclic file dependencies must form a valid graph");
        let folders = fixture.folders(Path::new("src"), depth, false);
        let folders = assert_ok!(folders, "Folder aggregation must succeed at depth {depth:?}");
        let folder_analysis = analyze_folders(&folders);
        let folder_analysis = assert_ok!(folder_analysis, "Aggregated folders must support cycle analysis");
        let file_analysis = analyze_files(&fixture);
        let file_analysis = assert_ok!(file_analysis, "Original file graph must support cycle analysis");
        let system_under_test = folder_report;

        let folder_output = system_under_test(&folder_analysis, NonZeroUsize::MAX);
        let folder_output = assert_ok!(folder_output, "Aggregated folder cycle report must render");
        let file_output = file_report(&file_analysis, NonZeroUsize::MAX);
        let file_output = assert_ok!(file_output, "Original file cycle report must render");
        let folder_chains = report_chains(&folder_output);
        let folder_chains = assert_ok!(folder_chains, "Aggregated folder report must contain parseable labels");
        let expected_chains = expected_chains.into_iter().map(|chain| chain.into_iter().map(str::to_owned).collect());
        let expected_chains: Vec<Vec<String>> = expected_chains.collect();

        assert_eq!(expected_chains, folder_chains, "Folder report did not reflect the aggregation depth");
        assert_eq!(Some(NO_CYCLES_FOUND), file_output.lines().last(), "File report inherited a folder-aggregation cycle");
    }

    #[rstest]
    #[case::within_group(nonzero!(1_usize), true)]
    #[case::group_boundary(nonzero!(2_usize), true)]
    #[case::exact_limit(nonzero!(3_usize), false)]
    #[case::below_limit(nonzero!(4_usize), false)]
    fn test_cycle_report_listing_limit(
        #[values(ReportKind::Folder, ReportKind::File)] kind: ReportKind,
        #[case] limit: NonZeroUsize,
        #[case] truncated: bool,
    ) {
        let system_under_test = match kind {
            ReportKind::Folder => {
                let analysis = folder_fixture(&[
                    ("api", "domain"), ("domain", "api"), ("domain", "storage"), ("storage", "domain"), ("queue", "queue"),
                ]).and_then(|graph| analyze_folders(&graph));
                let analysis = assert_ok!(analysis, "Building and analyzing the folder report fixture failed");
                folder_report(&analysis, limit)
            }
            ReportKind::File => {
                let [api, domain, storage, queue] = ["api", "domain", "storage", "queue"].map(|name| {
                    let file = SourcePath::new(name.into()).map(FileNode::new);
                    assert_ok!(file, "Source path creation failed for the cycle-report fixture")
                });
                let analysis = file_fixture(&[
                    (&api, &domain), (&domain, &api), (&domain, &storage), (&storage, &domain), (&queue, &queue),
                ]).and_then(|graph| analyze_files(&graph));
                let analysis = assert_ok!(analysis, "Building and analyzing the file report fixture failed");
                file_report(&analysis, limit)
            }
        };
        let output = assert_ok!(system_under_test, "Bounded cycle report generation failed");
        let chains = report_chains(&output);
        let chains = assert_ok!(chains, "Parsing cycle chains from the bounded report failed");
        let expected: Vec<_> = [
            vec!["api", "domain", "api"], vec!["domain", "storage", "domain"], vec!["queue", "queue"],
        ].into_iter().take(limit.get()).collect();

        assert_eq!(chains, expected, "Reported cycle chains differed from the expected bounded listing");
        assert_report_completeness(&output, 2, truncated);
        let groups = output.lines().filter(|line| line.starts_with("## ")).count();
        assert_eq!(groups, if truncated { 1 } else { 2 }, "Cycle group section count differed from the expected value");
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn test_file_path_encoding_error() {
        use std::ffi::OsString;

        #[cfg(unix)]
        let file_name = {
            use std::os::unix::ffi::OsStringExt as _;
            OsString::from_vec(vec![0xff])
        };
        #[cfg(windows)]
        let file_name = {
            use std::os::windows::ffi::OsStringExt as _;
            OsString::from_wide(&[0xd800])
        };
        let source_path = SourcePath::new(file_name.into());
        let source_path = assert_ok!(source_path, "A non-UTF-8 filename must still be a valid lexical source path");
        let file = FileNode::new(source_path);
        let fixture = file_fixture(&[(&file, &file)]);
        let fixture = assert_ok!(fixture, "A non-UTF-8 file self-loop must form a valid graph");
        let analysis = analyze_files(&fixture);
        let analysis = assert_ok!(analysis, "A non-UTF-8 file path must not prevent cycle analysis");
        let system_under_test = file_report;

        let result = system_under_test(&analysis, NonZeroUsize::MIN);

        assert!(result.is_err(), "A non-UTF-8 cycle path must remain a report encoding error");
    }
}
