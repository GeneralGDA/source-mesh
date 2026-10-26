use std::collections::BTreeMap;
use std::collections::BTreeSet;

use anyhow::Context as _;
use anyhow::Result;

use super::markdown::inline_code;
use super::model::Edge;
use super::model::FileGraph;
use super::model::FolderGraph;

const NO_VIOLATIONS_FOUND: &str = "No outgoing dependency violations found.";

#[derive(Clone, Copy)]
pub enum Mode {
    Files,
    Folders,
}

impl Mode {
    #[must_use]
    const fn label(self) -> &'static str {
        match self {
            Self::Files => "files",
            Self::Folders => "folders",
        }
    }
}

pub struct Analysis {
    mode: Mode,
    maximum: usize,
    violations: BTreeMap<String, usize>,
}

impl Analysis {
    pub fn files(graph: &FileGraph, maximum: usize) -> Result<Self> {
        let violations = violating_sources(graph.edges().iter(), maximum).map(|(source, count)| {
            let label = source.path().to_str().context("Outgoing dependency file path is not UTF-8")?;
            Ok((label.to_owned(), count))
        }).collect::<Result<_>>()?;
        Ok(Self { mode: Mode::Files, maximum, violations })
    }

    #[must_use]
    pub fn folders(graph: &FolderGraph, maximum: usize) -> Self {
        let violations = violating_sources(graph.edges().keys(), maximum);
        let violations = violations.map(|(source, count)| (source.label().clone(), count)).collect();
        Self { mode: Mode::Folders, maximum, violations }
    }

    #[must_use]
    pub fn has_violations(&self) -> bool {
        !self.violations.is_empty()
    }

    #[must_use]
    pub fn diagnostic(&self) -> String {
        let mode = self.mode.label();
        let mut output = if self.has_violations() {
            format!(
                "Outgoing dependency threshold exceeded (mode: {mode}, maximum: {}, violating sources: {}).\n",
                self.maximum,
                self.violations.len(),
            )
        } else {
            format!("{NO_VIOLATIONS_FOUND} Mode: {mode}; maximum: {}.\n", self.maximum)
        };
        output.push_str(&self.violation_list(|label| format!("{label:?}")));
        output
    }

    #[must_use]
    pub fn markdown(&self) -> String {
        let mode = self.mode.label();
        let mut output = format!("# Outgoing dependency threshold\n\nMode: {mode}\n\nMaximum: {}\n\n", self.maximum);
        if self.has_violations() {
            output.push_str(&self.violation_list(inline_code));
        } else {
            output.push_str(NO_VIOLATIONS_FOUND);
            output.push('\n');
        }
        output
    }

    #[must_use]
    fn violation_list(&self, label: impl Fn(&str) -> String) -> String {
        let mut output = String::new();
        let maximum = self.maximum.to_string();
        for (source, count) in &self.violations {
            output.push_str("- ");
            output.push_str(&label(source));
            output.push_str(": ");
            output.push_str(&count.to_string());
            output.push_str(" outgoing dependencies (maximum: ");
            output.push_str(&maximum);
            output.push_str(")\n");
        }
        output
    }
}

fn violating_sources<'a, Node: Ord + 'a>(
    edges: impl Iterator<Item = &'a Edge<Node>>,
    maximum: usize,
) -> impl Iterator<Item = (&'a Node, usize)> {
    let mut targets_by_source: BTreeMap<_, BTreeSet<_>> = BTreeMap::new();
    for edge in edges {
        targets_by_source.entry(edge.source()).or_default().insert(edge.target());
    }
    let counts = targets_by_source.into_iter().map(|(source, targets)| (source, targets.len()));
    counts.filter(move |&(_, count)| count > maximum)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::collections::BTreeSet;
    use std::path::Path;
    use std::path::PathBuf;

    use anyhow::Error;
    use anyhow::Result;
    use claims::assert_ok;
    use claims::assert_some;
    use pretty_assertions::assert_eq;
    use rstest::rstest;

    use super::Analysis;
    use super::Mode;
    use super::NO_VIOLATIONS_FOUND;
    use crate::SourcePath;
    use crate::model::DependencyCount;
    use crate::model::DependencyKind;
    use crate::model::DependencyKind::Production;
    use crate::model::DependencyKind::Test;
    use crate::model::FileEdge;
    use crate::model::FileGraph;
    use crate::model::FileNode;
    use crate::model::FolderEdge;
    use crate::model::FolderGraph;
    use crate::model::FolderNode;

    fn file_node_fixture(path: PathBuf) -> Result<FileNode> {
        SourcePath::new(path).map(FileNode::new)
    }

    fn graph_fixture(
        dependencies: &[(&str, &str, DependencyKind, usize)],
        isolated: &[&str],
    ) -> Result<(FileGraph, FolderGraph)> {
        let labels = dependencies.iter().flat_map(|&(source, target, _, _)| [source, target]);
        let labels: BTreeSet<_> = labels.chain(isolated.iter().copied()).collect();
        let files = labels.iter().map(|&label| file_node_fixture(label.into())).collect::<Result<_>>()?;
        let file_edges = dependencies.iter().map(|&(source, target, kind, _)| {
            Ok(FileEdge::new(file_node_fixture(source.into())?, file_node_fixture(target.into())?, kind))
        }).collect::<Result<_>>()?;
        let folders = labels.iter().map(|&label| FolderNode::new(label.into())).collect();
        let folder_edges = dependencies.iter().map(|&(source, target, kind, weight)| {
            let edge = FolderEdge::new(FolderNode::new(source.into()), FolderNode::new(target.into()), kind);
            (edge, DependencyCount::new(weight))
        }).collect();
        Ok((FileGraph::new(files, file_edges)?, FolderGraph::new(folders, folder_edges)?))
    }

    #[rstest]
    fn test_outgoing_direction(#[values(Mode::Files, Mode::Folders)] mode: Mode) {
        let fixture = graph_fixture(&[
            ("alpha", "sink", Production, 1),
            ("beta", "sink", Production, 1),
            ("gamma", "sink", Production, 1),
            ("source", "alpha", Production, 1),
            ("source", "beta", Production, 1),
        ], &[]);
        let (files, folders) = assert_ok!(fixture, "Incoming and outgoing dependencies must form valid graphs");

        let system_under_test = match mode {
            Mode::Files => Analysis::files(&files, 1),
            Mode::Folders => Ok(Analysis::folders(&folders, 1)),
        };
        let system_under_test = assert_ok!(system_under_test, "Directed dependencies must support threshold analysis");

        let expected = BTreeMap::from([(String::from("source"), 2)]);
        assert_eq!(expected, system_under_test.violations, "Incoming dependencies must not contribute to the source count");
        assert!(system_under_test.has_violations(), "Two outgoing targets must exceed a maximum of one");
    }

    #[rstest]
    #[case::zero(0, true)]
    #[case::above(1, true)]
    #[case::equal(2, false)]
    #[case::below(3, false)]
    #[case::maximum(usize::MAX, false)]
    fn test_distinct_targets_and_thresholds(
        #[values(Mode::Files, Mode::Folders)] mode: Mode,
        #[case] maximum: usize,
        #[case] expected_violation: bool,
    ) {
        let fixture = graph_fixture(&[
            ("source", "first", Production, usize::MAX),
            ("source", "first", Test, usize::MAX),
            ("source", "second", Test, 7),
        ], &[]);
        let (files, folders) = assert_ok!(fixture, "Mixed dependency kinds and large weights must form valid graphs");

        let system_under_test = match mode {
            Mode::Files => Analysis::files(&files, maximum),
            Mode::Folders => Ok(Analysis::folders(&folders, maximum)),
        };
        let system_under_test = assert_ok!(system_under_test, "Threshold analysis must accept every usize maximum");

        let expected = if expected_violation { BTreeMap::from([(String::from("source"), 2)]) } else { BTreeMap::new() };
        assert_eq!(expected, system_under_test.violations, "Count distinct targets, not edge kinds or folder weights");
        assert_eq!(expected_violation, system_under_test.has_violations(), "Only counts strictly above the maximum violate");
    }

    #[rstest]
    #[case::empty(&[])]
    #[case::isolated(&["isolated"])]
    fn test_empty_and_isolated_reports(#[values(Mode::Files, Mode::Folders)] mode: Mode, #[case] isolated: &[&str]) {
        let fixture = graph_fixture(&[], isolated);
        let (files, folders) = assert_ok!(fixture, "Empty and isolated-node graphs must be valid");

        let system_under_test = match mode {
            Mode::Files => Analysis::files(&files, 0),
            Mode::Folders => Ok(Analysis::folders(&folders, 0)),
        };
        let system_under_test = assert_ok!(system_under_test, "Graphs without edges must support threshold analysis");
        let diagnostic = system_under_test.diagnostic();
        let markdown = system_under_test.markdown();

        assert!(!system_under_test.has_violations(), "An empty outgoing target set must satisfy a zero maximum");
        assert!(diagnostic.contains(NO_VIOLATIONS_FOUND), "Diagnostic must explicitly state that no sources violate");
        assert!(diagnostic.contains(&format!("Mode: {}; maximum: 0", mode.label())), "Diagnostic must identify its scope");
        assert_eq!(Some(NO_VIOLATIONS_FOUND), markdown.lines().last(), "Markdown must explicitly state that no sources violate");
        assert!(markdown.contains(&format!("Mode: {}", mode.label())), "Markdown must identify the analysis mode");
        assert!(markdown.contains("Maximum: 0"), "Markdown must include the configured zero maximum");
        assert!(!diagnostic.lines().any(|line| line.starts_with("- ")), "Diagnostic must not list isolated nodes");
        assert!(!markdown.lines().any(|line| line.starts_with("- ")), "Markdown must not list isolated nodes");
    }

    #[rstest]
    #[case::above(0, true)]
    #[case::equal(1, false)]
    fn test_self_edges(
        #[values(Mode::Files, Mode::Folders)] mode: Mode,
        #[case] maximum: usize,
        #[case] expected_violation: bool,
    ) {
        let fixture = graph_fixture(&[("self", "self", Production, usize::MAX), ("self", "self", Test, 3)], &[]);
        let (files, folders) = assert_ok!(fixture, "Self edges of both dependency kinds must form valid graphs");

        let system_under_test = match mode {
            Mode::Files => Analysis::files(&files, maximum),
            Mode::Folders => Ok(Analysis::folders(&folders, maximum)),
        };
        let system_under_test = assert_ok!(system_under_test, "Self edges must support threshold analysis");

        let expected = if expected_violation { BTreeMap::from([(String::from("self"), 1)]) } else { BTreeMap::new() };
        assert_eq!(expected, system_under_test.violations, "A retained self edge must count as one distinct target");
        assert_eq!(expected_violation, system_under_test.has_violations(), "Self-edge thresholds must use strict comparison");
    }

    #[rstest]
    fn test_all_violations_in_sorted_order(#[values(Mode::Files, Mode::Folders)] mode: Mode) {
        let fixture = graph_fixture(&[
            ("zebra", "first", Production, 1),
            ("zebra", "second", Test, 1),
            ("middle", "first", Production, 1),
            ("middle", "second", Production, 1),
            ("alpha", "first", Production, 1),
            ("alpha", "second", Production, 1),
            ("alpha", "third", Production, 1),
        ], &[]);
        let (files, folders) = assert_ok!(fixture, "Several violating sources must form valid graphs");

        let system_under_test = match mode {
            Mode::Files => Analysis::files(&files, 1),
            Mode::Folders => Ok(Analysis::folders(&folders, 1)),
        };
        let system_under_test = assert_ok!(system_under_test, "Every violating source must be analyzed");
        let diagnostic = Error::msg(system_under_test.diagnostic()).to_string();
        let markdown = system_under_test.markdown();
        let diagnostic_lines: Vec<_> = diagnostic.lines().filter(|line| line.starts_with("- ")).collect();
        let markdown_lines: Vec<_> = markdown.lines().filter(|line| line.starts_with("- ")).collect();

        assert_eq!(vec![
            "- \"alpha\": 3 outgoing dependencies (maximum: 1)",
            "- \"middle\": 2 outgoing dependencies (maximum: 1)",
            "- \"zebra\": 2 outgoing dependencies (maximum: 1)",
        ], diagnostic_lines, "The complete error string must list all offending sources in label order");
        assert_eq!(vec![
            "- <code>alpha</code>: 3 outgoing dependencies (maximum: 1)",
            "- <code>middle</code>: 2 outgoing dependencies (maximum: 1)",
            "- <code>zebra</code>: 2 outgoing dependencies (maximum: 1)",
        ], markdown_lines, "Markdown must list every source with its actual count and maximum");
        let summary = format!("mode: {}, maximum: 1, violating sources: 3", mode.label());
        assert!(diagnostic.contains(&summary), "The error summary must identify the mode, maximum, and violation total");
        assert!(markdown.contains(&format!("Mode: {}", mode.label())), "Markdown must identify the analysis mode");
        assert!(markdown.contains("Maximum: 1"), "Markdown must include the configured maximum");
    }

    #[test]
    fn test_file_paths_and_folder_labels() {
        let service = file_node_fixture(Path::new("src").join("api").join("deep").join("service.rs"));
        let service = assert_ok!(service, "Nested source file must be a valid project-relative path");
        let database = file_node_fixture(Path::new("src").join("storage").join("database.rs"));
        let database = assert_ok!(database, "Target file must be a valid project-relative path");
        let edge = FileEdge::new(service.clone(), database.clone(), Production);
        let fixture = FileGraph::new(BTreeSet::from([service.clone(), database]), BTreeSet::from([edge]));
        let fixture = assert_ok!(fixture, "Cross-folder dependency must form a valid file graph");
        let folders = fixture.folders(Path::new("src"), Some(1), false);
        let folders = assert_ok!(folders, "Folder aggregation must supply root-relative depth-folded labels");
        let service_path = service.path().to_str();
        let service_path = assert_some!(service_path, "Source fixture path must be UTF-8");

        let system_under_test = Analysis::files(&fixture, 0);
        let system_under_test = assert_ok!(system_under_test, "Nested source paths must support file threshold analysis");
        let folder_analysis = Analysis::folders(&folders, 0);

        let expected_files = BTreeMap::from([(service_path.to_owned(), 1)]);
        assert_eq!(expected_files, system_under_test.violations, "File labels must retain the complete project-relative path");
        let expected_folders = BTreeMap::from([(String::from("api"), 1)]);
        assert_eq!(expected_folders, folder_analysis.violations, "Folder labels must come from the final aggregated graph");
        let diagnostic_path = format!("{service_path:?}");
        assert!(system_under_test.diagnostic().contains(&diagnostic_path), "Diagnostic must retain the complete file path");
    }

    #[rstest]
    fn test_folder_self_edge_filtering(#[values(false, true)] include_self: bool) {
        let service = file_node_fixture(Path::new("src").join("api").join("service.rs"));
        let service = assert_ok!(service, "Service fixture path must be valid");
        let request = file_node_fixture(Path::new("src").join("api").join("request.rs"));
        let request = assert_ok!(request, "Request fixture path must be valid");
        let edge = FileEdge::new(service.clone(), request.clone(), Production);
        let fixture = FileGraph::new(BTreeSet::from([service, request]), BTreeSet::from([edge]));
        let fixture = assert_ok!(fixture, "A dependency inside one folder must form a valid graph");
        let folders = fixture.folders(Path::new("src"), None, include_self);
        let folders = assert_ok!(folders, "Folder aggregation must apply the requested self-edge policy");

        let system_under_test = Analysis::folders(&folders, 0);

        let expected = if include_self { BTreeMap::from([(String::from("api"), 1)]) } else { BTreeMap::new() };
        assert_eq!(expected, system_under_test.violations, "Analysis must count exactly the self edges retained by aggregation");
        assert_eq!(include_self, system_under_test.has_violations(), "Analysis must not add or remove self edges");
    }

    #[rstest]
    #[case("unsafe`<>&\n\r\t", "unsafe&#96;&lt;&gt;&amp;&#92;n&#92;r&#92;t", "\"unsafe`<>&\\n\\r\\t\"")]
    #[case("unsafe*[link]_\\~|", "unsafe&#42;&#91;link&#93;&#95;&#92;&#126;&#124;", "\"unsafe*[link]_\\\\~|\"")]
    #[case(
        "unsafe\n## injected\n<script>alert(1)</script>",
        "unsafe&#92;n## injected&#92;n&lt;script&gt;alert(1)&lt;/script&gt;",
        "\"unsafe\\n## injected\\n<script>alert(1)</script>\""
    )]
    fn test_report_label_escaping(
        #[values(Mode::Files, Mode::Folders)] mode: Mode,
        #[case] label: &str,
        #[case] markdown_label: &str,
        #[case] diagnostic_label: &str,
    ) {
        let fixture = graph_fixture(&[(label, "safe", Production, 1)], &[]);
        let (files, folders) = assert_ok!(fixture, "Markup and control characters must be accepted as lexical graph labels");

        let system_under_test = match mode {
            Mode::Files => Analysis::files(&files, 0),
            Mode::Folders => Ok(Analysis::folders(&folders, 0)),
        };
        let system_under_test = assert_ok!(system_under_test, "Special characters must not prevent threshold analysis");
        let diagnostic = system_under_test.diagnostic();
        let markdown = system_under_test.markdown();
        let expected_diagnostic = format!("- {diagnostic_label}: 1 outgoing dependencies (maximum: 0)");
        let expected_markdown = format!("- <code>{markdown_label}</code>: 1 outgoing dependencies (maximum: 0)");
        let diagnostic_lines: Vec<_> = diagnostic.lines().filter(|line| line.starts_with("- ")).collect();
        let markdown_lines: Vec<_> = markdown.lines().filter(|line| line.starts_with("- ")).collect();

        assert_eq!(vec![expected_diagnostic.as_str()], diagnostic_lines, "Diagnostic labels must remain quoted on one line");
        assert_eq!(vec![expected_markdown.as_str()], markdown_lines, "Markdown must preserve inline-code escaping");
        assert!(!markdown.contains("<script>"), "A graph label must not inject HTML into the report");
        assert!(!markdown.lines().any(|line| line.starts_with("## ")), "A graph label must not inject a report heading");
    }

    #[rstest]
    #[case("unsafe\0\u{7}\u{1b}[31m\u{85}", "\"unsafe\\0\\u{7}\\u{1b}[31m\\u{85}\"")]
    #[case("unsafe\u{2028}\u{202e}", "\"unsafe\\u{2028}\\u{202e}\"")]
    #[case("quote\"and\\backslash", "\"quote\\\"and\\\\backslash\"")]
    fn test_diagnostic_control_characters(
        #[values(Mode::Files, Mode::Folders)] mode: Mode,
        #[case] label: &str,
        #[case] escaped: &str,
    ) {
        let fixture = graph_fixture(&[(label, "safe", Production, 1)], &[]);
        let (files, folders) = assert_ok!(fixture, "Non-printing characters must be accepted as lexical graph labels");

        let system_under_test = match mode {
            Mode::Files => Analysis::files(&files, 0),
            Mode::Folders => Ok(Analysis::folders(&folders, 0)),
        };
        let system_under_test = assert_ok!(system_under_test, "Control characters must not prevent threshold analysis");
        let diagnostic = system_under_test.diagnostic();
        let expected = format!("- {escaped}: 1 outgoing dependencies (maximum: 0)");

        assert_eq!(Some(expected.as_str()), diagnostic.lines().last(), "Diagnostic labels must be quoted and control-safe");
        assert!(diagnostic.chars().all(|character| character == '\n' || !character.is_control()), "Unsafe diagnostic control");
        assert!(!diagnostic.contains('\u{202e}'), "A graph label must not change the diagnostic text direction");
    }

    #[cfg(any(unix, windows))]
    #[rstest]
    #[case::violating_path(0, true)]
    #[case::nonviolating_path(1, false)]
    fn test_file_path_encoding(#[case] maximum: usize, #[case] expected_error: bool) {
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
        let file = file_node_fixture(file_name.into());
        let file = assert_ok!(file, "A non-UTF-8 filename must remain a valid lexical source path");
        let edge = FileEdge::new(file.clone(), file.clone(), Production);
        let fixture = FileGraph::new(BTreeSet::from([file]), BTreeSet::from([edge]));
        let fixture = assert_ok!(fixture, "A non-UTF-8 source must form a valid file graph");
        let system_under_test = Analysis::files;

        let result = system_under_test(&fixture, maximum);

        assert_eq!(expected_error, result.is_err(), "Only reported source paths require lossless UTF-8 labels");
    }
}
