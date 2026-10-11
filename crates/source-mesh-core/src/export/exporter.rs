use std::io::Write;

use anyhow::Result;
use anyhow::ensure;
use derive_new::new;
use getset::CopyGetters;

use crate::cycles::CycleAnalysis;
use crate::model::FolderGraph;
use crate::model::FolderNode;

#[derive(CopyGetters, new)]
#[getset(get_copy = "pub")]
pub struct Options<'analysis> {
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
