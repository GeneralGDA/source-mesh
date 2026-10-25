use std::io::Write;

use anyhow::Result;
use anyhow::ensure;
use bon::Builder;
use getset::CopyGetters;

use crate::cycles::CycleAnalysis;
use crate::model::FolderGraph;
use crate::model::FolderNode;

#[derive(Builder, CopyGetters, Default)]
#[getset(get_copy = "pub")]
pub struct Options<'analysis> {
    #[builder(default)]
    group_folders: bool,
    cycles: Option<&'analysis CycleAnalysis<FolderNode>>,
}

pub trait Exporter {
    fn write(&self, graph: &FolderGraph, output: &mut dyn Write) -> Result<()>;
    fn write_grouped(&self, graph: &FolderGraph, output: &mut dyn Write) -> Result<()>;

    fn write_with_options(&self, graph: &FolderGraph, options: &Options<'_>, output: &mut dyn Write) -> Result<()> {
        ensure!(options.cycles().is_none(), "This exporter does not support cycle highlighting");
        if options.group_folders() {
            self.write_grouped(graph, output)
        } else {
            self.write(graph, output)
        }
    }
}

#[cfg(test)]
pub(super) mod tests {
    use std::io::Write;

    use anyhow::Result;
    use claims::assert_ok;
    use pretty_assertions::assert_eq;
    use rstest::rstest;

    use super::Exporter;
    use super::Options;
    use crate::cycles::analyze_folders;
    use crate::export::rendering::serialization_fixture::cycle_graph_fixture as graph_fixture;
    use crate::model::FolderGraph;

    pub(in crate::export) fn render(exporter: &dyn Exporter, graph: &FolderGraph) -> Result<String> {
        let mut output = Vec::new();
        exporter.write(graph, &mut output)?;
        Ok(String::from_utf8(output)?)
    }

    pub(in crate::export) fn render_grouped(exporter: &dyn Exporter, graph: &FolderGraph) -> Result<String> {
        let mut output = Vec::new();
        exporter.write_grouped(graph, &mut output)?;
        Ok(String::from_utf8(output)?)
    }

    #[rstest]
    #[case::default(&Options::default())]
    #[case::builder(&Options::builder().build())]
    fn test_export_options_defaults(#[case] system_under_test: &Options<'_>) {
        assert!(!system_under_test.group_folders(), "Folder grouping must be disabled by default");
        assert!(system_under_test.cycles().is_none(), "Cycle highlighting must be disabled by default");
    }

    struct ExistingExporter;

    impl Exporter for ExistingExporter {
        fn write(&self, graph: &FolderGraph, output: &mut dyn Write) -> Result<()> {
            writeln!(output, "flat {}", graph.nodes().len())?;
            Ok(())
        }

        fn write_grouped(&self, graph: &FolderGraph, output: &mut dyn Write) -> Result<()> {
            writeln!(output, "grouped {}", graph.nodes().len())?;
            Ok(())
        }
    }

    #[rstest]
    #[case(false, "flat 0\n")]
    #[case(true, "grouped 0\n")]
    fn test_existing_exporter_default_dispatch(#[case] grouped: bool, #[case] expected: &str) {
        let fixture = FolderGraph::default();
        let system_under_test = ExistingExporter;
        let mut output = Vec::new();

        let options = Options::builder().group_folders(grouped).build();
        let result = system_under_test.write_with_options(&fixture, &options, &mut output);
        assert_ok!(result, "Default options must dispatch to the existing exporter, grouped={grouped}");

        assert_eq!(output, expected.as_bytes(), "Default options did not call the existing exporter method");
    }

    #[test]
    fn test_existing_exporter_unsupported_highlighting() {
        let fixture = graph_fixture();
        let fixture = assert_ok!(fixture, "Highlighting fixture must form a valid folder graph");
        let cycles = analyze_folders(&fixture);
        let cycles = assert_ok!(cycles, "Highlighting fixture must support cycle analysis");
        let system_under_test = ExistingExporter;
        let mut output = Vec::new();

        let result = system_under_test.write_with_options(&fixture, &Options::builder().cycles(&cycles).build(), &mut output);

        assert!(result.is_err(), "Unsupported highlighting was silently accepted");
        assert!(output.is_empty(), "Unsupported highlighting wrote partial output");
    }
}
