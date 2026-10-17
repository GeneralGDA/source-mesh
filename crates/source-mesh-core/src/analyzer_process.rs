use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::Context as _;
use anyhow::Result;
use anyhow::bail;
use derive_new::new;

use crate::analyzer_diagnostics::MESSAGE_DEPENDENCY_GRAPH_FAILURE;
use crate::analyzer_diagnostics::MESSAGE_PROJECT_LOADING_FAILURE_REASON;
use crate::analyzer_diagnostics::ScipDiagnostics;
use crate::analyzer_options::Options as AnalyzerOptions;
use crate::diagnostics::MESSAGE_VERBOSE_DIAGNOSTICS_HINT;
use crate::diagnostics::emit;

#[derive(Clone, Copy)]
pub enum Phase {
    Original,
    WithoutTest,
}

impl Phase {
    #[must_use]
    const fn label(self) -> &'static str {
        match self {
            Self::Original => "analysis with original configuration",
            Self::WithoutTest => "analysis without cfg(test)",
        }
    }
}

#[derive(new)]
pub struct ScipProcess<'a> {
    project: &'a Path,
    configuration_path: Option<&'a Path>,
    options: &'a AnalyzerOptions,
}

impl ScipProcess<'_> {
    pub fn generate_index(&self, index_file: &Path, phase: Phase) -> Result<Vec<u8>> {
        let mut analyzer_command = Command::new("rust-analyzer");
        analyzer_command.current_dir(self.project);
        analyzer_command.args(["scip", ".", "--exclude-vendored-libraries", "--num-threads", "0"]);
        analyzer_command.arg("--output").arg(index_file);
        if let Some(configuration_path) = self.configuration_path {
            analyzer_command.arg("--config-path").arg(configuration_path);
        }
        let output = analyzer_command.output().with_context(|| {
            format!(
                "Cannot run rust-analyzer ({}); check the project directory and that rust-analyzer is installed and executable",
                phase.label()
            )
        })?;
        if self.options.verbose() {
            emit(format_args!(
                "rust-analyzer ({}) stdout:\n{}\nrust-analyzer ({}) stderr:\n{}",
                phase.label(),
                String::from_utf8_lossy(&output.stdout),
                phase.label(),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        let diagnostics = ScipDiagnostics::from_streams(&output.stdout, &output.stderr);
        if !output.status.success() {
            bail!(diagnostics.failure_message(phase.label(), &output.status, self.options.verbose()));
        }
        if diagnostics.has_blocking_errors() {
            bail!(diagnostics.failure_message(phase.label(), &MESSAGE_PROJECT_LOADING_FAILURE_REASON, self.options.verbose()));
        }
        fs::read(index_file).with_context(|| {
            format!(
                "{MESSAGE_DEPENDENCY_GRAPH_FAILURE}: {} produced no readable analysis data. {MESSAGE_VERBOSE_DIAGNOSTICS_HINT}",
                phase.label()
            )
        })
    }
}
