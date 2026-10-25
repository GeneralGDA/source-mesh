use std::fs;
use std::io;
use std::io::BufRead;
use std::io::BufReader;
use std::io::Read;
use std::io::Seek as _;
use std::io::SeekFrom;
use std::io::copy;
use std::io::stderr;
use std::path::Path;
use std::path::PathBuf;
use std::process::Child;

use std::process::ChildStdin;
use std::process::Command;
use std::process::ExitStatus;
use std::process::Stdio;
use std::thread;
use std::time::Duration;
use std::time::Instant;

use anyhow::Context as _;
use anyhow::Result;
use anyhow::anyhow;
use anyhow::bail;
use anyhow::ensure;
use derive_new::new;
use getset::Getters;
use lsp_server::Message;
use lsp_server::Notification;
use lsp_server::Request;
use lsp_server::RequestId;
use lsp_server::Response;
use serde_json::Value;
use serde_json::json;
use tempfile::NamedTempFile;
use url::Url;

use crate::diagnostics::MESSAGE_VERBOSE_DIAGNOSTICS_HINT;
use crate::diagnostics::emit;

const TIMEOUT_INITIALIZATION: Duration = Duration::from_mins(10);
const TIMEOUT_DEFINITION: Duration = Duration::from_mins(5);
const POLL_INTERVAL_READER: Duration = Duration::from_millis(10);
const SERVER_STATUS: &str = "experimental/serverStatus";
const MESSAGE_WORKSPACE_LOADING_WARNING: &str = "Warning during workspace loading; run with --verbose for analyzer diagnostics";
const MESSAGE_WORKSPACE_LOADING_ERROR: &str = "unknown workspace loading error";
const MESSAGE_DEFINITION_FILE_RESOLUTION_ERROR: &str = "Cannot resolve definition at";
const MESSAGE_DEPENDENCY_RESOLUTION_FAILURE: &str = "Dependency resolution failed";
const MESSAGE_DEFINITION_FAILURE: &str = "Semantic definition lookup failed";
const MESSAGE_DEFINITION_DIAGNOSTICS_HEADER: &str = "rust-analyzer diagnostics:";
const MAXIMUM_DIAGNOSTIC_BYTES: u64 = 8_192;

#[derive(Getters)]
#[getset(get = "pub(crate)")]
pub(crate) struct DefinitionLocation {
    file: PathBuf,
    line: u32,
    column: u32,
}

pub(crate) struct DefinitionResolver {
    process: AnalyzerProcess,
    input: ChildStdin,
    output: BufReader<NamedTempFile>,
    configuration: Value,
    next_request: i32,
}

#[derive(new)]
struct DefinitionReader<'a, R, F> {
    output: &'a mut R,
    has_exited: F,
    deadline: Instant,
    #[new(default)]
    exited: bool,
}

#[derive(Debug, PartialEq, Eq)]
enum InitializationStatus<'a> {
    Loading,
    Ready,
    Warning(&'a str),
}

struct AnalyzerProcess {
    child: Child,
    diagnostics: NamedTempFile,
    verbose: bool,
}

impl DefinitionResolver {
    pub(crate) fn start(project: &Path, configuration_path: Option<&Path>, verbose: bool) -> Result<Self> {
        let project = dunce::canonicalize(project).context("Cannot resolve definition lookup project")?;
        let configuration = definition_configuration(configuration_path)?;
        let diagnostics = NamedTempFile::new().context("Cannot capture rust-analyzer definition diagnostics")?;
        let output = NamedTempFile::new().context("Cannot capture rust-analyzer definition responses")?;
        let child = {
            let mut command = Command::new("rust-analyzer");
            command.current_dir(&project).stdin(Stdio::piped());
            command.stdout(output.reopen().context("Cannot open definition response output")?);
            command.stderr(diagnostics.reopen().context("Cannot open definition diagnostics output")?);
            command.spawn().with_context(|| {
                format!(
                    "Cannot start rust-analyzer for semantic definition lookup in {}; check the project directory \
                     and that rust-analyzer is installed and executable",
                    project.display()
                )
            })?
        };
        let mut process = AnalyzerProcess { child, diagnostics, verbose };
        let Some(input) = process.child.stdin.take() else {
            unreachable!("A successfully spawned child with piped stdin must own its input stream");
        };
        let mut resolver = Self { process, input, output: BufReader::new(output), configuration, next_request: 0 };
        if verbose {
            emit(format_args!("Loading rust-analyzer semantic definitions for {}...", project.display()));
        }
        resolver.initialize(&project).with_context(|| resolver.process.failure_context())?;
        Ok(resolver)
    }

    pub(crate) fn definitions(&mut self, file: &Path, line: u32, column: u32) -> Result<Vec<DefinitionLocation>> {
        if !file.is_absolute() {
            unreachable!("Definition lookup must use an absolute source path from the canonical project: {}", file.display());
        }
        let url = Url::from_file_path(file).map_err(|()| anyhow!("Cannot encode source path {} as a file URI", file.display()))?;
        let started = Instant::now();
        if self.process.verbose {
            emit(format_args!(
                "Looking up definition at {}:{}:{}...", file.display(), u64::from(line) + 1, u64::from(column) + 1
            ));
        }
        let parameters = json!({"textDocument": {"uri": url.as_str()}, "position": {"line": line, "character": column}});
        let result = self.request("textDocument/definition", parameters, TIMEOUT_DEFINITION);
        let result = result.with_context(|| self.process.failure_context())?;
        if self.process.verbose {
            emit(format_args!("Definition lookup completed in {:.2}s.", started.elapsed().as_secs_f64()));
        }
        definition_locations(&result).with_context(|| self.process.failure_context())
    }

    fn initialize(&mut self, project: &Path) -> Result<()> {
        let root = Url::from_directory_path(project);
        let root = root.map_err(|()| anyhow!("Cannot encode project directory {} as a file URI", project.display()))?;
        let initialization = self.request(
            "initialize",
            json!({
                "processId": std::process::id(),
                "rootUri": root.as_str(),
                "capabilities": {
                    "general": {"positionEncodings": ["utf-8"]},
                    "experimental": {"serverStatusNotification": true},
                    "workspace": {"configuration": true}
                },
                "initializationOptions": self.configuration
            }),
            TIMEOUT_INITIALIZATION,
        )?;
        ensure!(
            initialization.pointer("/capabilities/positionEncoding").and_then(Value::as_str) == Some("utf-8"),
            "rust-analyzer did not negotiate UTF-8 positions required for SCIP definition lookup; update rust-analyzer"
        );
        Message::Notification(Notification::new("initialized".into(), json!({}))).write(&mut self.input)?;
        let deadline = Instant::now() + TIMEOUT_INITIALIZATION;
        loop {
            let message = self.receive(deadline)?;
            if let Message::Notification(notification) = message && notification.method == SERVER_STATUS {
                match initialization_status(&notification.params)? {
                    InitializationStatus::Loading => {}
                    InitializationStatus::Ready => return Ok(()),
                    InitializationStatus::Warning(warning) => {
                        if self.process.verbose {
                            let warning = warning.split_whitespace().collect::<Vec<_>>().join(" ");
                            let warning: String = warning.chars().take(512).collect();
                            emit(format_args!("warning: Project loading: {warning}"));
                        }
                        return Ok(());
                    }
                }
            }
        }
    }

    fn request(&mut self, method: &str, parameters: Value, timeout: Duration) -> Result<Value> {
        self.next_request = self.next_request.checked_add(1).context("Too many definition lookup requests")?;
        let request_id = RequestId::from(self.next_request);
        let request = Message::Request(Request { id: request_id.clone(), method: method.into(), params: parameters });
        request.write(&mut self.input).with_context(|| format!("Cannot send rust-analyzer {method} request"))?;
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(result) = matching_response(&self.receive(deadline)?, &request_id)? {
                return Ok(result);
            }
        }
    }

    fn receive(&mut self, deadline: Instant) -> Result<Message> {
        let has_exited = || self.process.child.try_wait().map(|status| status.is_some());
        let mut reader = DefinitionReader::new(&mut self.output, has_exited, deadline);
        let message = reader.receive()?;
        if let Message::Request(ref request) = message {
            let response = match request.method.as_str() {
                "workspace/configuration" => {
                    Response::new_ok(request.id.clone(), configuration_response(&self.configuration, &request.params)?)
                }
                "window/workDoneProgress/create" | "client/registerCapability" => {
                    Response::new_ok(request.id.clone(), Value::Null)
                }
                _ => Response::new_err(request.id.clone(), -32_601, format!("Unsupported client request: {}", request.method)),
            };
            Message::Response(response).write(&mut self.input).context("Cannot answer rust-analyzer client request")?;
        }
        Ok(message)
    }
}

impl<R: BufRead, F: FnMut() -> io::Result<bool>> DefinitionReader<'_, R, F> {
    fn receive(&mut self) -> Result<Message> {
        let message = Message::read(self).and_then(|message| {
            message.ok_or_else(|| {
                io::Error::new(io::ErrorKind::UnexpectedEof, "rust-analyzer definition output closed before responding")
            })
        });
        message.context("Cannot read rust-analyzer definition response")
    }
}

impl<R: BufRead, F: FnMut() -> io::Result<bool>> Read for DefinitionReader<'_, R, F> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let length = self.fill_buf()?.read(buf)?;
        self.consume(length);
        Ok(length)
    }
}

impl<R: BufRead, F: FnMut() -> io::Result<bool>> BufRead for DefinitionReader<'_, R, F> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        loop {
            let remaining = self.deadline.checked_duration_since(Instant::now()).filter(|remaining| !remaining.is_zero());
            let remaining = remaining.ok_or_else(|| {
                io::Error::new(io::ErrorKind::TimedOut, "rust-analyzer definition lookup exceeded its timeout")
            })?;
            if !self.output.fill_buf()?.is_empty() {
                return self.output.fill_buf();
            }
            if self.exited {
                return Ok(&[]);
            }
            if (self.has_exited)()? {
                self.exited = true;
                /* The child may have written its final bytes between EOF and the exit check. */
                return self.output.fill_buf();
            }
            thread::sleep(POLL_INTERVAL_READER.min(remaining));
        }
    }

    fn consume(&mut self, amount: usize) {
        self.output.consume(amount);
    }
}

impl AnalyzerProcess {
    #[must_use]
    fn failure_context(&self) -> String {
        if !self.verbose {
            return format!("{MESSAGE_DEPENDENCY_RESOLUTION_FAILURE}. {MESSAGE_VERBOSE_DIAGNOSTICS_HINT}");
        }
        let details = (|| -> Result<String> {
            let mut file = self.diagnostics.reopen()?;
            let length = file.metadata()?.len();
            file.seek(SeekFrom::Start(length.saturating_sub(MAXIMUM_DIAGNOSTIC_BYTES)))?;
            let mut output = Vec::new();
            file.take(MAXIMUM_DIAGNOSTIC_BYTES).read_to_end(&mut output)?;
            Ok(String::from_utf8_lossy(&output).trim().to_owned())
        })();
        match details {
            Ok(details) if !details.is_empty() => {
                format!("{MESSAGE_DEFINITION_FAILURE}. {MESSAGE_DEFINITION_DIAGNOSTICS_HEADER}\n{details}")
            }
            Ok(_) => MESSAGE_DEFINITION_FAILURE.into(),
            Err(error) => {
                emit(format_args!("warning: Cannot read rust-analyzer definition diagnostics: {error}"));
                MESSAGE_DEFINITION_FAILURE.into()
            }
        }
    }
}

impl Drop for AnalyzerProcess {
    fn drop(&mut self) {
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            match self.child.kill() {
                Ok(()) => {
                    if let Err(error) = self.child.wait() {
                        emit(format_args!("warning: Cannot collect rust-analyzer definition lookup process: {error}"));
                    }
                }
                Err(error) => {
                    if let Err(error) = reconcile_analyzer_kill_error(error, &self.child.try_wait()) {
                        emit(format_args!("warning: Cannot stop rust-analyzer definition lookup: {error}"));
                    }
                }
            }
        }
        if self.verbose {
            let output = self.diagnostics.reopen().and_then(|mut file| copy(&mut file, &mut stderr().lock()));
            if let Err(error) = output {
                emit(format_args!("warning: Cannot display rust-analyzer definition diagnostics: {error}"));
            }
        }
    }
}

fn reconcile_analyzer_kill_error(error: io::Error, status: &io::Result<Option<ExitStatus>>) -> io::Result<()> {
    /* An exit racing with kill is harmless; an unconfirmed exit must retain the original termination failure. */
    if !matches!(*status, Ok(Some(_))) {
        return Err(error);
    }
    Ok(())
}

fn definition_configuration(path: Option<&Path>) -> Result<Value> {
    let mut configuration = if let Some(path) = path {
        let contents = fs::read(path).with_context(|| format!("Cannot read rust-analyzer configuration {}", path.display()))?;
        serde_json::from_slice(&contents)?
    } else {
        json!({})
    };
    let settings = configuration.as_object_mut().context("rust-analyzer configuration must be a JSON object")?;
    settings.insert("checkOnSave".into(), Value::Bool(false));
    for group in ["cachePriming", "diagnostics"] {
        let group_settings = settings.entry(group).or_insert_with(|| json!({})).as_object_mut();
        let group_settings = group_settings.with_context(|| format!("rust-analyzer {group} settings must be a JSON object"))?;
        group_settings.insert("enable".into(), Value::Bool(false));
    }
    Ok(configuration)
}

fn initialization_status(parameters: &Value) -> Result<InitializationStatus<'_>> {
    let parameters = parameters.as_object().context("rust-analyzer server status must be an object")?;
    let quiescent = parameters.get("quiescent").and_then(Value::as_bool);
    let quiescent = quiescent.context("rust-analyzer server status quiescent must be a boolean")?;
    let health = parameters.get("health").and_then(Value::as_str);
    let health = health.context("rust-analyzer server status health must be a string")?;
    let message = parameters.get("message").filter(|message| !message.is_null());
    let message = message.map(|message| {
        message.as_str().context("rust-analyzer server status message must be a string or null")
    });
    let message = message.transpose()?;
    match (quiescent, health) {
        (false, "ok" | "warning" | "error") => Ok(InitializationStatus::Loading),
        (true, "ok") => Ok(InitializationStatus::Ready),
        (true, "warning") => Ok(InitializationStatus::Warning(message.unwrap_or(MESSAGE_WORKSPACE_LOADING_WARNING))),
        (true, "error") => {
            bail!(
                "rust-analyzer cannot load the project for definition lookup: {}",
                message.unwrap_or(MESSAGE_WORKSPACE_LOADING_ERROR)
            )
        }
        (_, _) => bail!("Invalid rust-analyzer server status health: {health}"),
    }
}

fn configuration_response(configuration: &Value, parameters: &Value) -> Result<Value> {
    let items = parameters.get("items").and_then(Value::as_array).context("Invalid rust-analyzer configuration request")?;
    let values = items.iter().map(|item| {
        let item = item.as_object().context("rust-analyzer configuration item must be an object")?;
        let section = item.get("section");
        let section = section.map(|section| section.as_str().context("rust-analyzer configuration section must be a string"));
        let section = section.transpose()?;
        Ok(match section {
            None | Some("" | "rust-analyzer") => configuration.clone(),
            Some(section) => {
                let section = section.strip_prefix("rust-analyzer.").unwrap_or(section);
                let value = section.split('.').try_fold(configuration, |value, name| value.get(name));
                value.cloned().unwrap_or(Value::Null)
            }
        })
    });
    Ok(Value::Array(values.collect::<Result<Vec<_>>>()?))
}

fn matching_response(message: &Message, request_id: &RequestId) -> Result<Option<Value>> {
    if let Message::Response(ref response) = *message && &response.id == request_id {
        return match response.response_result.as_ref() {
            Ok(result) => Ok(Some(result.clone())),
            Err(error) => bail!("rust-analyzer definition request failed ({}): {}", error.code, error.message),
        };
    }
    Ok(None)
}

fn definition_locations(value: &Value) -> Result<Vec<DefinitionLocation>> {
    if value.is_null() {
        return Ok(Vec::new());
    }
    let locations = value.as_array().map_or_else(|| std::slice::from_ref(value), Vec::as_slice);
    let locations = locations.iter().map(|location| {
        let (uri, range) = if let Some(uri) = location.get("targetUri") {
            (uri, location.get("targetSelectionRange"))
        } else {
            (location.get("uri").context("Definition response is missing its URI")?, location.get("range"))
        };
        let uri = Url::parse(uri.as_str().context("Definition URI must be a string")?)?;
        let file = uri.to_file_path().map_err(|()| anyhow!("rust-analyzer definition does not point to a local file"))?;
        let start = range.and_then(|range| range.get("start")).context("Definition response is missing its start position")?;
        let line = u32::try_from(start.get("line").and_then(Value::as_u64).context("Invalid definition line")?)?;
        let column = u32::try_from(start.get("character").and_then(Value::as_u64).context("Invalid definition column")?)?;
        Ok(DefinitionLocation { file, line, column })
    });
    locations.collect()
}

pub(crate) fn canonical_definition_file(location: &DefinitionLocation) -> Result<PathBuf> {
    dunce::canonicalize(location.file()).with_context(|| {
        let line = u64::from(*location.line()) + 1;
        let column = u64::from(*location.column()) + 1;
        format!("{MESSAGE_DEFINITION_FILE_RESOLUTION_ERROR} {}:{line}:{column}", location.file().display())
    })
}


#[cfg(test)]
mod tests {
    use std::env;
    use std::fmt::Arguments;
    use std::fs;
    use std::fs::File;
    use std::fs::OpenOptions;
    use std::io;
    use std::io::BufRead as _;
    use std::io::BufReader;
    use std::io::Cursor;
    use std::io::Read;
    use std::io::Write as _;
    #[cfg(unix)]
    use std::os::unix::process::ExitStatusExt as _;
    #[cfg(windows)]
    use std::os::windows::process::ExitStatusExt as _;
    use std::panic::catch_unwind;
    use std::panic::resume_unwind;
    use std::path::Path;
    use std::process::Child;
    use std::process::Command;
    use std::process::ExitStatus;
    use std::process::Stdio;
    use std::sync::Arc;
    use std::sync::mpsc;
    use std::sync::mpsc::RecvTimeoutError;
    use std::thread;
    use std::time::Duration;
    use std::time::Instant;

    use anyhow::Context as _;
    use anyhow::Result;
    use anyhow::bail;
    use claims::assert_ok;
    use claims::assert_some;
    use lsp_server::Message;
    use lsp_server::Notification;
    use lsp_server::Request;
    use lsp_server::RequestId;
    use lsp_server::Response;
    use pretty_assertions::assert_eq;
    use rstest::rstest;
    use serde_json::Value;
    use serde_json::json;
    use tempfile::NamedTempFile;
    use tempfile::tempdir;
    use url::Url;

    use super::AnalyzerProcess;
    use super::DefinitionLocation;
    use super::DefinitionReader;
    use super::DefinitionResolver;
    use super::InitializationStatus;
    use super::MESSAGE_DEFINITION_DIAGNOSTICS_HEADER;
    use super::MESSAGE_DEFINITION_FAILURE;
    use super::MESSAGE_DEPENDENCY_RESOLUTION_FAILURE;
    use super::MAXIMUM_DIAGNOSTIC_BYTES;
    use super::SERVER_STATUS;
    use super::MESSAGE_DEFINITION_FILE_RESOLUTION_ERROR;
    use super::MESSAGE_WORKSPACE_LOADING_ERROR;
    use super::MESSAGE_WORKSPACE_LOADING_WARNING;
    use super::canonical_definition_file;
    use super::configuration_response;
    use super::definition_configuration;
    use super::definition_locations;
    use super::initialization_status;
    use super::matching_response;
    use super::reconcile_analyzer_kill_error;
    use crate::diagnostics::MESSAGE_VERBOSE_DIAGNOSTICS_HINT;

    const ENV_PEER_SCRIPT: &str = "SOURCE_MESH_DEFINITION_PEER_SCRIPT";
    const ENV_PEER_OUTPUT: &str = "SOURCE_MESH_DEFINITION_PEER_OUTPUT";
    const ENV_PEER_TRANSCRIPT: &str = "SOURCE_MESH_DEFINITION_PEER_TRANSCRIPT";
    const TIMEOUT_PEER: Duration = Duration::from_secs(10);
    const MESSAGE_PEER_INPUT_TIMEOUT: &str = "Resolver peer input exceeded the script deadline";
    const POLL_INTERVAL_PROCESS: Duration = Duration::from_millis(10);
    const STEP_RECEIVE: &str = "receive";
    const STEP_REPLY: &str = "reply";
    const STEP_SEND: &str = "send";
    const TRANSCRIPT_STARTED: &str = "started";
    const TRANSCRIPT_RECEIVED: &str = "received";
    const TRANSCRIPT_COMPLETED: &str = "completed";

    enum PeerStep {
        Receive,
        Reply(Value),
        Send(Message),
    }

    struct ResolverFixture {
        resolver: Option<DefinitionResolver>,
        process: Option<AnalyzerProcess>,
        output: Option<BufReader<NamedTempFile>>,
        script: NamedTempFile,
        transcript: NamedTempFile,
        deadline: Instant,
    }

    impl ResolverFixture {
        fn new(configuration: Value, steps: Vec<PeerStep>) -> Self {
            let steps: Vec<_> = steps.into_iter().map(|step| match step {
                PeerStep::Receive => json!({"step": STEP_RECEIVE}),
                PeerStep::Reply(result) => json!({"step": STEP_REPLY, "result": result}),
                PeerStep::Send(message) => json!({"step": STEP_SEND, "message": message}),
            }).collect();
            let script_creation = NamedTempFile::new();
            let mut script = assert_ok!(script_creation, "The resolver peer script file must be created");
            let script_write = serde_json::to_writer(&mut script, &steps);
            assert_ok!(script_write, "The resolver peer script must serialize to JSON");
            let script_flush = script.flush();
            assert_ok!(script_flush, "The resolver peer script must be flushed before launching the peer");
            let output_creation = NamedTempFile::new();
            let protocol_output = assert_ok!(output_creation, "The resolver peer protocol output file must be created");
            let capture_creation = NamedTempFile::new();
            let diagnostics = assert_ok!(capture_creation, "The resolver peer diagnostic capture must be created");
            let transcript_creation = NamedTempFile::new();
            let transcript = assert_ok!(transcript_creation, "The resolver peer transcript file must be created");
            let module = module_path!().split_once("::");
            let (_, module) = assert_some!(module, "The resolver fixture must be nested in the library test module");
            let filter = format!("{module}::{}", stringify!(test_resolver_peer));
            let executable_lookup = env::current_exe();
            let executable = assert_ok!(executable_lookup, "The resolver fixture test executable must be located");
            let mut command = Command::new(executable);
            command.args(["--exact", &filter, "--ignored", "--nocapture"]);
            command.stdin(Stdio::piped()).stdout(Stdio::null());
            let capture_reopen = diagnostics.reopen();
            let capture = assert_ok!(capture_reopen, "The resolver peer diagnostic capture must be reopened for stderr");
            command.stderr(capture);
            command.env(ENV_PEER_OUTPUT, protocol_output.path());
            let mut fixture = Self {
                resolver: None,
                process: None,
                output: Some(BufReader::new(protocol_output)),
                script,
                transcript,
                deadline: Instant::now() + TIMEOUT_PEER,
            };
            command.env(ENV_PEER_SCRIPT, fixture.script.path());
            command.env(ENV_PEER_TRANSCRIPT, fixture.transcript.path());
            let peer_spawn = command.spawn();
            let child = assert_ok!(peer_spawn, "The resolver fixture peer must start");
            fixture.process = Some(AnalyzerProcess { child, diagnostics, verbose: false });
            let peer = fixture.process.as_mut();
            let peer = assert_some!(peer, "The fixture must own its newly spawned peer");
            let input = peer.child.stdin.take();
            let input = assert_some!(input, "A successfully spawned peer with piped stdin must own its input stream");
            let (process, output) = (fixture.process.take(), fixture.output.take());
            let process = assert_some!(process, "The fixture must retain its process until resolver construction");
            let output = assert_some!(output, "The fixture must retain its output until resolver construction");
            fixture.resolver = Some(DefinitionResolver { process, input, output, configuration, next_request: 0 });
            fixture
        }

        fn resolver_mut(&mut self) -> &mut DefinitionResolver {
            let resolver = self.resolver.as_mut();
            assert_some!(resolver, "The resolver must be available between construction and finish")
        }

        fn finish(mut self) -> Vec<Message> {
            let resolver = self.resolver.take();
            let resolver = assert_some!(resolver, "A resolver fixture can only be finished once");
            let DefinitionResolver { process: peer, input, output, .. } = resolver;
            self.process = Some(peer);
            self.output = Some(output);
            drop(input);
            let process = self.process.as_mut();
            let process = assert_some!(process, "The fixture must retain its peer while collecting its exit status");
            let status = loop {
                let status_query = process.child.try_wait();
                let status = assert_ok!(status_query, "The resolver fixture peer exit status must be collected");
                if let Some(status) = status {
                    break status;
                }
                let remaining = self.deadline.checked_duration_since(Instant::now()).filter(|remaining| !remaining.is_zero());
                let remaining = assert_some!(remaining, "The resolver fixture peer must exit before its deadline");
                thread::sleep(POLL_INTERVAL_PROCESS.min(remaining));
            };
            if !status.success() {
                let diagnostics_read = fs::read(process.diagnostics.path());
                let diagnostics = assert_ok!(diagnostics_read, "Diagnostics must be readable after peer failure ({status})");
                let diagnostics = String::from_utf8_lossy(&diagnostics);
                assert!(status.success(), "The resolver fixture peer must exit successfully ({status}):\n{diagnostics}");
            }
            let transcript_read = fs::read_to_string(self.transcript.path());
            let transcript_contents = assert_ok!(transcript_read, "The resolver peer transcript must be readable");
            let mut messages = Vec::new();
            let mut started = false;
            let mut completed = false;
            for line in transcript_contents.lines() {
                let entry_parse = serde_json::from_str::<Value>(line);
                let entry = assert_ok!(entry_parse, "The resolver peer transcript entry must be valid JSON: {line}");
                let event = entry.get("event").and_then(Value::as_str);
                let event = assert_some!(event, "The resolver peer transcript entry must identify its event: {entry}");
                let recognized = match event {
                    TRANSCRIPT_STARTED => {
                        assert!(!started && !completed, "The resolver peer must start its transcript exactly once: {entry}");
                        started = true;
                        true
                    }
                    TRANSCRIPT_RECEIVED => {
                        assert!(started && !completed, "Peer messages must occur between start and completion: {entry}");
                        let message_field = entry.get("message");
                        let message_json = assert_some!(message_field, "Received transcript entry needs a message: {entry}");
                        let message_parse = serde_json::from_value(message_json.clone());
                        let protocol_message = assert_ok!(message_parse, "The message must decode as a protocol message");
                        messages.push(protocol_message);
                        true
                    }
                    TRANSCRIPT_COMPLETED => {
                        assert!(started && !completed, "The resolver peer must complete a started transcript once: {entry}");
                        completed = true;
                        true
                    }
                    _ => false,
                };
                assert!(recognized, "The resolver peer transcript must use known event markers: {entry}");
            }
            assert!(started && completed, "The resolver peer transcript must contain both started and completed markers");
            messages
        }
    }

    impl Drop for ResolverFixture {
        fn drop(&mut self) {
            if let Some(resolver) = self.resolver.as_mut() {
                stop_peer(&mut resolver.process.child);
            }
            if let Some(process) = self.process.as_mut() {
                stop_peer(&mut process.child);
            }
        }
    }

    fn cleanup_warning(message: Arguments<'_>) {
        /* A failed stderr write must not replace an existing panic during fixture cleanup. */
        let _: io::Result<()> = writeln!(io::stderr().lock(), "warning: {message}");
    }

    fn stop_peer(child: &mut Child) {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => {}
            Err(error) => cleanup_warning(format_args!("Cannot inspect resolver fixture peer during cleanup: {error}")),
        }
        match child.kill() {
            Ok(()) => {
                if let Err(error) = child.wait() {
                    cleanup_warning(format_args!("Cannot collect resolver fixture peer during cleanup: {error}"));
                }
            }
            Err(error) => match child.try_wait() {
                Ok(Some(_)) => {}
                Ok(None) => cleanup_warning(format_args!("Cannot stop resolver fixture peer during cleanup: {error}")),
                Err(wait_error) => {
                    let message = format_args!("Cannot stop resolver fixture peer: {error}; cannot check its exit: {wait_error}");
                    cleanup_warning(message);
                }
            },
        }
    }

    fn write_transcript(transcript: &mut File, entry: &Value) -> Result<()> {
        serde_json::to_writer(&mut *transcript, entry).context("Cannot write the resolver peer transcript")?;
        writeln!(transcript)?;
        transcript.flush().context("Cannot flush the resolver peer transcript")
    }

    fn run_peer() -> Result<()> {
        let Some(script_path) = env::var_os(ENV_PEER_SCRIPT) else {
            return Ok(());
        };
        let deadline = Instant::now() + TIMEOUT_PEER;
        let output_path = env::var_os(ENV_PEER_OUTPUT).context("Resolver peer protocol output path is missing")?;
        let transcript_path = env::var_os(ENV_PEER_TRANSCRIPT).context("Resolver peer transcript path is missing")?;
        let entries: Vec<Value> = serde_json::from_slice(&fs::read(script_path)?)?;
        let steps = entries.into_iter().map(|entry| -> Result<PeerStep> {
            match entry.get("step").and_then(Value::as_str) {
                Some(STEP_RECEIVE) => Ok(PeerStep::Receive),
                Some(STEP_REPLY) => {
                    let result = entry.get("result").context("Resolver peer reply step is missing its result")?;
                    Ok(PeerStep::Reply(result.clone()))
                }
                Some(STEP_SEND) => {
                    let message = entry.get("message").context("Resolver peer send step is missing its message")?;
                    Ok(PeerStep::Send(serde_json::from_value(message.clone())?))
                }
                Some(_) | None => bail!("Invalid resolver peer script step: {entry}"),
            }
        });
        let steps = steps.collect::<Result<Vec<_>>>()?;
        let mut output = OpenOptions::new().write(true).open(output_path)?;
        let mut transcript = OpenOptions::new().write(true).open(transcript_path)?;
        write_transcript(&mut transcript, &json!({"event": TRANSCRIPT_STARTED}))?;
        let (sender, receiver) = mpsc::channel();
        let worker = thread::Builder::new().name("resolver-peer-input".into()).spawn(move || -> Result<()> {
            let stdin = io::stdin();
            let mut input = stdin.lock();
            while let Some(message) = Message::read(&mut input).context("Cannot read resolver peer input")? {
                sender.send(message).context("Resolver peer coordinator stopped receiving input")?;
            }
            Ok(())
        }).context("Cannot start the resolver peer input worker")?;
        let mut last_request = None;
        let mut disconnected = false;
        let exchange = (|| -> Result<()> {
            for step in steps {
                let remaining = deadline.checked_duration_since(Instant::now()).filter(|remaining| !remaining.is_zero());
                let remaining = remaining.context("Resolver peer script exceeded its deadline")?;
                match step {
                    PeerStep::Receive => {
                        let message = match receiver.recv_timeout(remaining) {
                            Ok(message) => message,
                            Err(RecvTimeoutError::Timeout) => bail!(MESSAGE_PEER_INPUT_TIMEOUT),
                            Err(RecvTimeoutError::Disconnected) => {
                                disconnected = true;
                                bail!("Resolver input ended before the peer script finished");
                            }
                        };
                        if let Message::Request(ref request) = message {
                            last_request = Some(request.id.clone());
                        }
                        write_transcript(&mut transcript, &json!({"event": TRANSCRIPT_RECEIVED, "message": message}))?;
                    }
                    PeerStep::Reply(result) => {
                        let request_id = last_request.clone().context("Peer reply requires a previously received request")?;
                        Message::Response(Response::new_ok(request_id, result)).write(&mut output)?;
                    }
                    PeerStep::Send(message) => message.write(&mut output)?,
                }
            }
            loop {
                let remaining = deadline.checked_duration_since(Instant::now()).filter(|remaining| !remaining.is_zero());
                let remaining = remaining.context(MESSAGE_PEER_INPUT_TIMEOUT)?;
                match receiver.recv_timeout(remaining) {
                    Ok(message) => {
                        write_transcript(&mut transcript, &json!({"event": TRANSCRIPT_RECEIVED, "message": message}))?;
                    }
                    Err(RecvTimeoutError::Timeout) => bail!(MESSAGE_PEER_INPUT_TIMEOUT),
                    Err(RecvTimeoutError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
            Ok(())
        })();
        if exchange.is_ok() || disconnected || worker.is_finished() {
            match worker.join() {
                Ok(result) => result?,
                Err(payload) => resume_unwind(payload),
            }
        } else {
            /* A blocked reader is detached only inside this exiting subprocess; joining it would defeat the deadline. */
            drop(worker);
        }
        exchange?;
        write_transcript(&mut transcript, &json!({"event": TRANSCRIPT_COMPLETED}))
    }

    #[test]
    #[ignore = "Executed by resolver component tests in a subprocess"]
    fn test_resolver_peer() {
        let result = run_peer();
        assert_ok!(result, "The resolver peer must complete its scripted exchange and transcript");
    }

    #[test]
    fn test_definition_exchange() {
        let project_creation = tempdir();
        let project = assert_ok!(project_creation, "The definition lookup project fixture must be created");
        let project_resolution = dunce::canonicalize(project.path());
        let directory = assert_ok!(project_resolution, "The definition lookup project must have a canonical path");
        let source = directory.join("source with spaces.rs");
        let target = directory.join("target.rs");
        let source_uri_encoding = Url::from_file_path(&source);
        let source_uri = assert_ok!(source_uri_encoding, "The fixture source path must encode as a file URI");
        let target_uri_encoding = Url::from_file_path(&target);
        let target_uri = assert_ok!(target_uri_encoding, "The fixture target path must encode as a file URI");
        let definition_response = json!([{
            "uri": target_uri.as_str(),
            "range": {"start": {"line": 11, "character": 23}, "end": {"line": 11, "character": 29}}
        }]);
        let mut fixture = ResolverFixture::new(json!({}), vec![PeerStep::Receive, PeerStep::Reply(definition_response)]);
        let system_under_test = fixture.resolver_mut();

        let locations = system_under_test.definitions(&source, 7, 13);
        let transcript = fixture.finish();

        let locations = assert_ok!(locations, "Definition lookup must complete its exchange with the peer");
        let locations: Vec<_> = locations.into_iter().map(|location| {
            (location.file().clone(), *location.line(), *location.column())
        }).collect();
        assert_eq!(locations, vec![(target, 11, 23)], "Definition lookup must return the peer's exact location");
        assert_eq!(transcript.len(), 1, "Definition lookup must send one request");
        let request = transcript.iter().find_map(|message| match *message {
            Message::Request(ref request) => Some(request),
            Message::Response(_) | Message::Notification(_) => None,
        });
        let request = assert_some!(request, "The peer transcript must contain the definition request");
        assert_eq!(request.method, "textDocument/definition");
        assert_eq!(request.params, json!({
            "textDocument": {"uri": source_uri.as_str()}, "position": {"line": 7, "character": 13}
        }));
    }

    #[test]
    fn test_initialization_exchange() {
        let project_creation = tempdir();
        let project = assert_ok!(project_creation, "The initialization project fixture must be created");
        let project_resolution = dunce::canonicalize(project.path());
        let directory = assert_ok!(project_resolution, "The initialization project must have a canonical path");
        let root_uri_encoding = Url::from_directory_path(&directory);
        let root_uri = assert_ok!(root_uri_encoding, "The fixture project directory must encode as a file URI");
        let configuration = json!({"cfg": {"setTest": false}});
        let marker_method = "fixture/after-ready";
        let progress = Notification::new("$/progress".into(), json!({"token": "loading", "value": {"kind": "begin"}}));
        let loading = Notification::new(SERVER_STATUS.into(), json!({"quiescent": false, "health": "ok"}));
        let ready = Notification::new(SERVER_STATUS.into(), json!({"quiescent": true, "health": "ok"}));
        let marker = Notification::new(marker_method.into(), json!({}));
        let steps = vec![
            PeerStep::Receive,
            PeerStep::Reply(json!({"capabilities": {"positionEncoding": "utf-8"}})),
            PeerStep::Receive,
            PeerStep::Send(Message::Notification(progress)),
            PeerStep::Send(Message::Notification(loading)),
            PeerStep::Send(Message::Notification(ready)),
            PeerStep::Send(Message::Notification(marker)),
        ];
        let mut fixture = ResolverFixture::new(configuration.clone(), steps);
        let system_under_test = fixture.resolver_mut();

        let initialized = system_under_test.initialize(&directory);
        let following = initialized.and_then(|()| system_under_test.receive(Instant::now() + Duration::from_secs(5)));
        let transcript = fixture.finish();

        let following = assert_ok!(following, "Initialization and receipt of the following notification must succeed");
        assert!(matches!(following, Message::Notification(notification) if notification.method == marker_method),
            "Initialization must consume loading and ready, leaving the next message for the next operation");
        assert_eq!(transcript.len(), 2, "Initialization must send its request and initialized notification");
        let request = transcript.iter().find_map(|message| match *message {
            Message::Request(ref request) => Some(request),
            Message::Response(_) | Message::Notification(_) => None,
        });
        let request = assert_some!(request, "The peer transcript must contain the initialization request");
        assert_eq!(request.method, "initialize");
        assert_eq!(request.params.get("processId"), Some(&json!(std::process::id())));
        assert_eq!(request.params.get("rootUri"), Some(&json!(root_uri.as_str())));
        assert_eq!(request.params.pointer("/capabilities/general/positionEncodings"), Some(&json!(["utf-8"])));
        assert_eq!(request.params.pointer("/capabilities/experimental/serverStatusNotification"), Some(&json!(true)));
        assert_eq!(request.params.pointer("/capabilities/workspace/configuration"), Some(&json!(true)));
        assert_eq!(request.params.get("initializationOptions"), Some(&configuration));
        let notifications: Vec<_> = transcript.iter().filter_map(|message| match *message {
            Message::Notification(ref notification) => Some((notification.method.as_str(), notification.params.clone())),
            Message::Request(_) | Message::Response(_) => None,
        }).collect();
        assert_eq!(notifications, vec![("initialized", json!({}))]);
    }

    #[rstest]
    #[case::missing_encoding(json!({}))]
    #[case::utf16(json!({"positionEncoding": "utf-16"}))]
    fn test_initialization_encoding(#[case] capabilities: Value) {
        let result = tempdir();
        let project = assert_ok!(result, "The incompatible-encoding project fixture must be created");
        let steps = vec![PeerStep::Receive, PeerStep::Reply(json!({"capabilities": capabilities}))];
        let mut fixture = ResolverFixture::new(json!({}), steps);
        let system_under_test = fixture.resolver_mut();

        let initialized = system_under_test.initialize(project.path());
        let transcript = fixture.finish();

        assert!(initialized.is_err_and(|error| error.to_string().contains("UTF-8")),
            "Initialization must reject incompatible positions, not fail later due to a transport error");
        assert_eq!(transcript.len(), 1, "An incompatible server must not receive an initialized notification");
    }

    #[test]
    fn test_request_exchange() {
        let unrelated = Response::new_ok(RequestId::from("unrelated-response".to_owned()), json!({"wrong": true}));
        let progress = Notification::new("$/progress".into(), json!({"token": "work", "value": {"kind": "end"}}));
        let first_result = json!({"answer": "first"});
        let second_result = json!({"answer": "second"});
        let steps = vec![
            PeerStep::Receive,
            PeerStep::Send(Message::Response(unrelated)),
            PeerStep::Send(Message::Notification(progress)),
            PeerStep::Reply(first_result.clone()),
            PeerStep::Receive,
            PeerStep::Reply(second_result.clone()),
        ];
        let mut fixture = ResolverFixture::new(json!({}), steps);
        let system_under_test = fixture.resolver_mut();

        let first = system_under_test.request("fixture/first", json!({"input": 7}), Duration::from_secs(5));
        let second = system_under_test.request("fixture/second", json!({"input": 13}), Duration::from_secs(5));
        let transcript = fixture.finish();

        let first = assert_ok!(first, "The first request must complete despite unrelated responses and notifications");
        assert_eq!(first, first_result, "A request must ignore unrelated responses and notifications");
        let second = assert_ok!(second, "The second request must complete with its own response");
        assert_eq!(second, second_result, "The next request must receive its own response");
        assert_eq!(transcript.len(), 2);
        let requests: Vec<_> = transcript.iter().filter_map(|message| match *message {
            Message::Request(ref request) => Some(request),
            Message::Response(_) | Message::Notification(_) => None,
        }).collect();
        let parameters: Vec<_> = requests.iter().map(|request| (request.method.as_str(), request.params.clone())).collect();
        assert_eq!(parameters, vec![("fixture/first", json!({"input": 7})), ("fixture/second", json!({"input": 13}))]);
        assert_ne!(requests.first().map(|request| &request.id), requests.last().map(|request| &request.id),
            "Successive requests must have distinct IDs without depending on a particular initial ID");
    }

    #[test]
    fn test_request_deadline() {
        let steps = vec![PeerStep::Receive, PeerStep::Reply(json!({"answer": "late"}))];
        let mut fixture = ResolverFixture::new(json!({}), steps);
        let system_under_test = fixture.resolver_mut();

        let result = system_under_test.request("fixture/expired", json!({}), Duration::ZERO);
        let transcript = fixture.finish();

        assert!(result.is_err_and(|error| {
            error.downcast_ref::<io::Error>().is_some_and(|cause| cause.kind() == io::ErrorKind::TimedOut)
        }), "A request must preserve its supplied deadline and the timeout error kind");
        assert_eq!(transcript.len(), 1, "An expired request must not be retried");
    }

    #[rstest]
    #[case::configuration(
        "workspace/configuration", json!({"items": [{"section": "rust-analyzer.cfg.setTest"}]}), Some(json!([false])),
    )]
    #[case::progress("window/workDoneProgress/create", json!({"token": "loading"}), Some(Value::Null))]
    #[case::registration("client/registerCapability", json!({"registrations": []}), Some(Value::Null))]
    #[case::unsupported("fixture/unsupported", json!({}), None)]
    fn test_server_request_exchange(#[case] method: &str, #[case] parameters: Value, #[case] expected_result: Option<Value>) {
        let server_id = RequestId::from("server-request".to_owned());
        let server_request = Request { id: server_id.clone(), method: method.into(), params: parameters };
        let expected_client_result = json!({"completed": true});
        let steps = vec![
            PeerStep::Receive,
            PeerStep::Send(Message::Request(server_request)),
            PeerStep::Receive,
            PeerStep::Reply(expected_client_result.clone()),
        ];
        let mut fixture = ResolverFixture::new(json!({"cfg": {"setTest": false}}), steps);
        let system_under_test = fixture.resolver_mut();

        let client_request_result = system_under_test.request("fixture/client", json!({}), Duration::from_secs(5));
        let transcript = fixture.finish();

        let client_response = assert_ok!(client_request_result, "The client request must complete while serving a peer request");
        assert_eq!(client_response, expected_client_result, "Serving a peer request must preserve the client response");
        assert_eq!(transcript.len(), 2, "Each server request must receive exactly one response");
        let response = transcript.iter().find_map(|message| match *message {
            Message::Response(ref response) => Some(response),
            Message::Request(_) | Message::Notification(_) => None,
        });
        let response = assert_some!(response, "The server request must receive a response");
        assert_eq!(response.id, server_id, "A server response must preserve its string request ID");
        if let Some(expected_result) = expected_result {
            let server_response_result = response.response_result.as_ref();
            let response_value = assert_ok!(server_response_result, "Server request {method} must receive a successful response");
            assert_eq!(response_value, &expected_result, "Incorrect server request dispatch");
        } else {
            let error = response.response_result.as_ref().err();
            let error = assert_some!(error, "An unsupported method must receive an error response");
            assert_eq!(error.code, -32_601, "Unsupported requests must use the JSON-RPC method-not-found code");
            assert!(error.message.contains(method), "The response must identify the unsupported method");
        }
    }

    fn diagnostic_fixture(diagnostics: &[u8]) -> Result<ResolverFixture> {
        let mut fixture = ResolverFixture::new(json!({}), Vec::new());
        fs::write(fixture.resolver_mut().process.diagnostics.path(), diagnostics)?;
        Ok(fixture)
    }

    #[rstest]
    #[case::empty(b"", "")]
    #[case::whitespace(b" \r\n\t ", "")]
    #[case::content(b"backend: missing dependency", "backend: missing dependency")]
    #[case::trimmed(b" \r\nbackend: first\nbackend: last\t\n", "backend: first\nbackend: last")]
    #[case::unicode_whitespace("\u{2003}backend: detail\u{2003}".as_bytes(), "backend: detail")]
    #[case::invalid_utf8(b"backend: \xff detail", "backend: \u{fffd} detail")]
    fn test_failure_context_visibility(#[case] diagnostics: &[u8], #[case] details: &str, #[values(false, true)] verbose: bool) {
        let result = diagnostic_fixture(diagnostics);
        let mut fixture = assert_ok!(result, "The resolver diagnostic visibility fixture must be prepared");
        let system_under_test = &mut fixture.resolver_mut().process;
        system_under_test.verbose = verbose;
        let expected = if !verbose {
            format!("{MESSAGE_DEPENDENCY_RESOLUTION_FAILURE}. {MESSAGE_VERBOSE_DIAGNOSTICS_HINT}")
        } else if details.is_empty() {
            MESSAGE_DEFINITION_FAILURE.into()
        } else {
            format!("{MESSAGE_DEFINITION_FAILURE}. {MESSAGE_DEFINITION_DIAGNOSTICS_HEADER}\n{details}")
        };

        let context = system_under_test.failure_context();
        system_under_test.verbose = false;
        fixture.finish();

        assert_eq!(context, expected, "Failure context must respect verbosity, trim diagnostics, and omit empty headers");
    }

    #[rstest]
    #[case::ascii(b"tail: ", "tail: ")]
    #[case::split_utf8(b"\x82\xac", "\u{fffd}\u{fffd}")]
    fn test_failure_context_tail(#[case] tail_prefix: &[u8], #[case] displayed_prefix: &str) {
        const LATEST: &str = "\nbackend: latest failure";
        let limit_conversion = usize::try_from(MAXIMUM_DIAGNOSTIC_BYTES);
        let limit = assert_ok!(limit_conversion, "The diagnostic capture limit must fit in the fixture buffer size");
        let padding = "x".repeat(limit - tail_prefix.len() - LATEST.len());
        let mut diagnostics = b"discarded backend details\n".repeat(limit);
        diagnostics.push(0xe2);
        diagnostics.extend_from_slice(tail_prefix);
        diagnostics.extend_from_slice(padding.as_bytes());
        diagnostics.extend_from_slice(LATEST.as_bytes());
        let expected_details = format!("{displayed_prefix}{padding}{LATEST}");
        let expected = format!("{MESSAGE_DEFINITION_FAILURE}. {MESSAGE_DEFINITION_DIAGNOSTICS_HEADER}\n{expected_details}");
        let fixture_preparation = diagnostic_fixture(&diagnostics);
        let mut fixture = assert_ok!(fixture_preparation, "The oversized resolver diagnostic fixture must be prepared");
        let system_under_test = &mut fixture.resolver_mut().process;
        system_under_test.verbose = true;

        let context = system_under_test.failure_context();
        system_under_test.verbose = false;
        fixture.finish();

        assert_eq!(context, expected, "Only the last diagnostic bytes may be read, even across a UTF-8 boundary");
    }

    #[rstest]
    #[cfg_attr(unix, case::replaced_capture(&Path::new("/").join("dev").join("null")))]
    #[cfg_attr(windows, case::replaced_capture(Path::new("NUL")))]
    fn test_failure_context_unavailable_diagnostics(#[case] device: &Path, #[values(false, true)] verbose: bool) {
        let fixture_preparation = diagnostic_fixture(b"unavailable backend detail");
        let mut fixture = assert_ok!(fixture_preparation, "The resolver fixture must be ready before replacing its capture");
        let system_under_test = &mut fixture.resolver_mut().process;
        /* Windows reopens the handle, not the pathname; a device cannot supply captured regular-file metadata. */
        let device_open = File::open(device);
        let device_file = assert_ok!(device_open, "The device replacing the regular-file diagnostic capture must open");
        *system_under_test.diagnostics.as_file_mut() = device_file;
        let captured_metadata = system_under_test.diagnostics.reopen().and_then(|file| file.metadata());
        assert!(captured_metadata.is_err(), "The fixture must make the diagnostic capture unavailable");
        system_under_test.verbose = verbose;
        let expected = if verbose {
            MESSAGE_DEFINITION_FAILURE.into()
        } else {
            format!("{MESSAGE_DEPENDENCY_RESOLUTION_FAILURE}. {MESSAGE_VERBOSE_DIAGNOSTICS_HINT}")
        };

        let context = system_under_test.failure_context();
        system_under_test.verbose = false;
        fixture.finish();

        assert_eq!(context, expected, "Unavailable extra diagnostics must not replace the original lookup failure");
    }

    #[rstest]
    fn test_analyzer_drop_termination(#[values(false, true)] unwind: bool) {
        const READY: &str = "fixture/drop-ready";
        let readiness = Message::Notification(Notification::new(READY.into(), json!({})));
        let mut fixture = ResolverFixture::new(json!({}), vec![PeerStep::Send(readiness)]);
        let deadline = fixture.deadline;
        let readiness_receive = fixture.resolver_mut().receive(deadline);
        let ready = assert_ok!(readiness_receive, "The peer must send its readiness notification before the drop probe");
        assert!(matches!(ready, Message::Notification(notification) if notification.method == READY));
        let status_query = fixture.resolver_mut().process.child.try_wait();
        let running = assert_ok!(status_query, "The live peer status must be available before the drop probe");
        assert!(running.is_none(), "The drop test must start with a live peer");
        let resolver = fixture.resolver.take();
        let resolver = assert_some!(resolver, "The drop fixture must own its initialized resolver");
        let DefinitionResolver { process: system_under_test, mut input, output, .. } = resolver;
        fixture.output = Some(output);
        let payload = Arc::new(());

        /* Retain stdin across drop so closing a Child handle cannot masquerade as termination.
           Closing it after the probe independently releases the peer even if Drop is deleted;
           the peer also has its own finite input deadline. */
        let dropped = catch_unwind(|| {
            if unwind {
                let _process = system_under_test;
                resume_unwind(Box::new(Arc::clone(&payload)));
            }
            drop(system_under_test);
        });
        let stopped = input.write_all(b"\n");
        drop(input);
        let completed_before_deadline = Instant::now() < deadline;

        assert!(completed_before_deadline, "Drop must stop the child before the peer's independent timeout");
        assert!(stopped.is_err_and(|error| error.kind() == io::ErrorKind::BrokenPipe),
            "Dropping AnalyzerProcess must terminate the live child, leaving a broken pipe");
        if unwind {
            assert!(dropped.is_err_and(|panic| {
                panic.downcast_ref::<Arc<()>>().is_some_and(|original| Arc::ptr_eq(original, &payload))
            }), "Process cleanup must preserve the original unwind payload");
        } else {
            assert_ok!(dropped, "Normal process cleanup must not panic");
        }
    }

    #[rstest]
    #[case::running(&Ok(None), true)]
    #[case::successful_exit(&Ok(Some(ExitStatus::from_raw(0))), false)]
    #[case::unsuccessful_exit(&Ok(Some(ExitStatus::from_raw(1))), false)]
    #[case::unknown_exit(&Err(io::Error::new(io::ErrorKind::BrokenPipe, "exit status unavailable")), true)]
    fn test_analyzer_kill_error_reconciliation(#[case] status: &io::Result<Option<ExitStatus>>, #[case] report_error: bool) {
        const KILL_ERROR: i32 = 5;
        let kill_error = io::Error::from_raw_os_error(KILL_ERROR);
        let expected_message = kill_error.to_string();

        let system_under_test = reconcile_analyzer_kill_error(kill_error, status);

        assert_eq!(system_under_test.is_err(), report_error, "Only a confirmed exit may suppress a termination failure");
        if let Err(error) = system_under_test {
            assert_eq!(error.raw_os_error(), Some(KILL_ERROR), "Cleanup must preserve the original kill error");
            assert_eq!(error.to_string(), expected_message, "A failed exit check must not replace the kill error");
        }
    }

    enum ReaderFixture {
        Failure(io::ErrorKind),
        Eof,
        EofThenFailure(io::ErrorKind),
        Panic(Arc<()>),
    }

    impl Read for ReaderFixture {
        fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
            match *self {
                Self::Failure(kind) => Err(io::Error::new(kind, "Synthetic reader failure")),
                Self::Eof => Ok(0),
                Self::EofThenFailure(kind) => {
                    *self = Self::Failure(kind);
                    Ok(0)
                }
                Self::Panic(ref payload) => resume_unwind(Box::new(Arc::clone(payload))),
            }
        }
    }

    fn definition_output_fixture() -> Result<(BufReader<NamedTempFile>, File)> {
        let output = NamedTempFile::new()?;
        let writer = output.reopen()?;
        Ok((BufReader::new(output), writer))
    }

    #[rstest]
    #[case::header(false)]
    #[case::body(true)]
    fn test_reader_partial_frames(#[case] split_in_body: bool, #[values(false, true)] process_exited: bool) {
        const METHOD: &str = "fixture/fragmented";
        let output_creation = definition_output_fixture();
        let (mut output, mut writer) = assert_ok!(output_creation, "The fragmented definition output fixture must be created");
        let mut wire = Vec::new();
        let frame_write = Message::Notification(Notification::new(METHOD.into(), json!({}))).write(&mut wire);
        assert_ok!(frame_write, "The fragmented notification must serialize to a protocol frame");
        let (prefix, suffix) = wire.split_at(if split_in_body { wire.len() - 1 } else { 1 });
        let prefix_write = writer.write_all(prefix);
        assert_ok!(prefix_write, "The partial notification prefix must be written before receiving");
        let mut exit_checks = 0;

        let message = {
            let mut system_under_test = DefinitionReader::new(
                &mut output,
                || {
                    exit_checks += 1;
                    writer.write_all(suffix).map(|()| process_exited)
                },
                Instant::now() + Duration::from_secs(5),
            );
            let message_receive = system_under_test.receive();
            assert_ok!(message_receive, "The reader must complete the fragmented notification before its deadline")
        };

        assert!(
            matches!(message, Message::Notification(notification) if notification.method == METHOD),
            "A partial frame was not completed, including bytes written just before process exit"
        );
        assert_eq!(exit_checks, 1, "Temporary EOF must trigger exactly one exit check for this frame");
    }

    #[test]
    fn test_reader_buffered_messages() {
        const METHOD: &str = "fixture/buffered";
        let output_creation = definition_output_fixture();
        let (mut output, mut writer) = assert_ok!(output_creation, "The buffered definition output fixture must be created");
        let initial_frame_write = Message::Notification(Notification::new(METHOD.into(), json!(1))).write(&mut writer);
        assert_ok!(initial_frame_write, "The first notification must be written before reading buffered messages");
        let following_frame_write = Message::Notification(Notification::new(METHOD.into(), json!(2))).write(&mut writer);
        assert_ok!(following_frame_write, "The second notification must be written before reading buffered messages");
        let deadline = Instant::now() + Duration::from_secs(5);

        let first = {
            let has_exited = || unreachable!("Buffered output must not poll the process");
            let mut system_under_test = DefinitionReader::new(&mut output, has_exited, deadline);
            let initial_receive = system_under_test.receive();
            assert_ok!(initial_receive, "The reader must receive the first buffered notification without polling the process")
        };
        let second = {
            let has_exited = || unreachable!("The next response must retain bytes buffered by the previous read");
            let mut system_under_test = DefinitionReader::new(&mut output, has_exited, deadline);
            let following_receive = system_under_test.receive();
            assert_ok!(following_receive, "The reader must receive the second notification from the retained buffer")
        };
        let mut system_under_test = DefinitionReader::new(&mut output, || Ok(true), deadline);
        let eof = system_under_test.receive();

        assert!(matches!(first, Message::Notification(notification)
            if notification.method == METHOD && notification.params == json!(1)));
        assert!(matches!(second, Message::Notification(notification)
            if notification.method == METHOD && notification.params == json!(2)));
        assert!(
            eof.is_err_and(|error| {
                error.downcast_ref::<io::Error>().is_some_and(|cause| cause.kind() == io::ErrorKind::UnexpectedEof)
            }),
            "Exited output must report EOF even while another handle to the file remains open"
        );
    }

    #[rstest]
    #[case::empty(b"", io::ErrorKind::UnexpectedEof)]
    #[case::partial_header(b"Content-L", io::ErrorKind::InvalidData)]
    #[case::missing_separator(b"Content-Length: 5\r\n", io::ErrorKind::UnexpectedEof)]
    #[case::partial_body(b"Content-Length: 5\r\n\r\n{", io::ErrorKind::UnexpectedEof)]
    #[case::malformed_header(b"Content-Length: invalid\r\n\r\n", io::ErrorKind::InvalidData)]
    #[case::malformed_body(b"Content-Length: 1\r\n\r\n!", io::ErrorKind::InvalidData)]
    fn test_reader_protocol_errors(#[case] wire: &[u8], #[case] expected: io::ErrorKind) {
        let mut output = BufReader::new(Cursor::new(wire));
        let mut system_under_test = DefinitionReader::new(&mut output, || Ok(true), Instant::now() + Duration::from_secs(5));

        let result = system_under_test.receive();

        assert!(
            result.is_err_and(|error| error.downcast_ref::<io::Error>().is_some_and(|cause| cause.kind() == expected)),
            "Closed or malformed protocol output lost its parser error"
        );
    }

    #[rstest]
    #[case::empty(b"")]
    #[case::partial_header(b"Content-L")]
    #[case::partial_body(b"Content-Length: 5\r\n\r\n{")]
    fn test_reader_incomplete_frame_timeout(#[case] wire: &[u8]) {
        let mut output = BufReader::new(Cursor::new(wire));
        let mut system_under_test = DefinitionReader::new(&mut output, || Ok(false), Instant::now() + Duration::from_millis(20));

        let result = system_under_test.receive();

        assert!(
            result.is_err_and(|error| {
                error.downcast_ref::<io::Error>().is_some_and(|cause| cause.kind() == io::ErrorKind::TimedOut)
            }),
            "A stalled header or body must time out instead of ending the stream or waiting indefinitely"
        );
    }

    #[test]
    fn test_reader_deadline_with_buffered_message() {
        let mut wire = Vec::new();
        let frame_write = Message::Notification(Notification::new("fixture/expired".into(), json!({}))).write(&mut wire);
        assert_ok!(frame_write, "The deadline fixture notification must serialize to a protocol frame");
        let mut output = BufReader::new(Cursor::new(wire));
        let buffer_fill = output.fill_buf();
        let buffered = assert_ok!(buffer_fill, "The notification must be buffered before the expired read");
        assert!(!buffered.is_empty(), "The fixture must contain a buffered message");
        let has_exited = || unreachable!("An expired read must not poll the process");
        let mut system_under_test = DefinitionReader::new(&mut output, has_exited, Instant::now());

        let message_receive = system_under_test.receive();

        assert!(
            message_receive.is_err_and(|error| {
                error.downcast_ref::<io::Error>().is_some_and(|cause| cause.kind() == io::ErrorKind::TimedOut)
            }),
            "Buffered bytes must not bypass the operation deadline"
        );
    }

    #[rstest]
    #[case::read(ReaderFixture::Failure(io::ErrorKind::PermissionDenied), false, io::ErrorKind::PermissionDenied)]
    #[case::process(ReaderFixture::Eof, false, io::ErrorKind::BrokenPipe)]
    #[case::final_read(ReaderFixture::EofThenFailure(io::ErrorKind::PermissionDenied), true, io::ErrorKind::PermissionDenied)]
    fn test_reader_io_error_identity(
        #[case] fixture: ReaderFixture,
        #[case] process_exited: bool,
        #[case] expected: io::ErrorKind,
    ) {
        let mut output = BufReader::new(fixture);
        let mut system_under_test = DefinitionReader::new(
            &mut output,
            || {
                if process_exited {
                    Ok(true)
                } else {
                    Err(io::Error::new(io::ErrorKind::BrokenPipe, "Synthetic process wait failure"))
                }
            },
            Instant::now() + Duration::from_secs(5),
        );

        let result = system_under_test.receive();

        assert!(
            result.is_err_and(|error| error.downcast_ref::<io::Error>().is_some_and(|cause| cause.kind() == expected)),
            "Reading or polling the process lost its original I/O failure"
        );
    }

    #[rstest]
    #[case::read(false)]
    #[case::exit_check(true)]
    fn test_reader_panic_identity(#[case] exit_check: bool) {
        let payload = Arc::new(());

        let system_under_test = catch_unwind(|| {
            let fixture = if exit_check { ReaderFixture::Eof } else { ReaderFixture::Panic(Arc::clone(&payload)) };
            let mut output = BufReader::new(fixture);
            let has_exited = || resume_unwind(Box::new(Arc::clone(&payload)));
            let mut reader = DefinitionReader::new(&mut output, has_exited, Instant::now() + Duration::from_secs(5));
            reader.receive()
        });

        assert!(
            system_under_test.is_err_and(|panic| {
                panic.downcast_ref::<Arc<()>>().is_some_and(|original| Arc::ptr_eq(original, &payload))
            }),
            "A synchronous reader panic must retain its original payload"
        );
    }

    #[test]
    fn test_reader_cleanup_during_unwind() {
        let payload = Arc::new(());

        let system_under_test = catch_unwind(|| {
            let mut output = BufReader::new(ReaderFixture::Panic(Arc::clone(&payload)));
            let has_exited = || unreachable!("Reader cleanup must not poll the process");
            let mut reader = DefinitionReader::new(&mut output, has_exited, Instant::now());
            let empty_read = reader.read(&mut []);
            let length = assert_ok!(empty_read, "An empty read must succeed before the unwind probe");
            assert_eq!(length, 0, "An empty read must not access the file or wait");
            resume_unwind(Box::new(Arc::clone(&payload)));
        });

        assert!(
            system_under_test.is_err_and(|panic| {
                panic.downcast_ref::<Arc<()>>().is_some_and(|original| Arc::ptr_eq(original, &payload))
            }),
            "Reader cleanup must not replace an existing panic or perform another read"
        );
    }

    #[rstest]
    #[case::loading(json!({"quiescent":false,"health":"ok"}), InitializationStatus::Loading)]
    #[case::loading_error(json!({"quiescent":false,"health":"error"}), InitializationStatus::Loading)]
    #[case::ready(json!({"quiescent":true,"health":"ok"}), InitializationStatus::Ready)]
    #[case::loading_null_message(json!({"quiescent":false,"health":"ok","message":null}), InitializationStatus::Loading)]
    #[case::loading_error_null_message(json!({"quiescent":false,"health":"error","message":null}), InitializationStatus::Loading)]
    #[case::ready_null_message(json!({"quiescent":true,"health":"ok","message":null}), InitializationStatus::Ready)]
    #[case::warning_null_message(
        json!({"quiescent":true,"health":"warning","message":null}),
        InitializationStatus::Warning(MESSAGE_WORKSPACE_LOADING_WARNING)
    )]
    #[case::warning(
        json!({"quiescent":true,"health":"warning","message":"Workspace warning"}),
        InitializationStatus::Warning("Workspace warning")
    )]
    #[case::warning_without_message(
        json!({"quiescent":true,"health":"warning"}),
        InitializationStatus::Warning(MESSAGE_WORKSPACE_LOADING_WARNING)
    )]
    fn test_initialization_status(#[case] parameters: Value, #[case] expected: InitializationStatus<'_>) {
        let system_under_test = initialization_status(&parameters);

        let status = assert_ok!(system_under_test, "A valid server status notification must be accepted");
        assert_eq!(status, expected);
    }

    #[rstest]
    #[case::not_an_object(json!(42))]
    #[case::missing_quiescent(json!({"health":"ok"}))]
    #[case::invalid_quiescent(json!({"quiescent":"true","health":"ok"}))]
    #[case::missing_health(json!({"quiescent":true}))]
    #[case::numeric_health(json!({"quiescent":true,"health":7}))]
    #[case::null_health(json!({"quiescent":true,"health":null}))]
    #[case::unknown_health(json!({"quiescent":true,"health":"unknown"}))]
    #[case::invalid_loading_health(json!({"quiescent":false,"health":[]}))]
    #[case::invalid_message(json!({"quiescent":true,"health":"warning","message":7}))]
    #[case::invalid_loading_message(json!({"quiescent":false,"health":"ok","message":false}))]
    fn test_malformed_initialization_status(#[case] parameters: Value) {
        let system_under_test = initialization_status(&parameters);

        assert!(system_under_test.is_err(), "Malformed server status was accepted");
    }

    #[test]
    fn test_initialization_workspace_error() {
        const MESSAGE: &str = "Workspace fixture failure";
        let parameters = json!({"quiescent":true,"health":"error","message":MESSAGE});

        let system_under_test = initialization_status(&parameters);

        assert!(
            system_under_test.is_err_and(|error| error.to_string().contains(MESSAGE)),
            "Workspace loading failure was accepted or lost its diagnostic"
        );
    }

    #[rstest]
    #[case::missing(json!({"quiescent":true,"health":"error"}))]
    #[case::null(json!({"quiescent":true,"health":"error","message":null}))]
    fn test_initialization_workspace_error_without_message(#[case] parameters: Value) {
        let system_under_test = initialization_status(&parameters);

        assert!(
            system_under_test.is_err_and(|error| error.to_string().ends_with(MESSAGE_WORKSPACE_LOADING_ERROR)),
            "An absent or null message must preserve the workspace failure and its fallback diagnostic"
        );
    }

    #[rstest]
    #[case::location(false, false)]
    #[case::locations(false, true)]
    #[case::location_link(true, true)]
    fn test_definition_location_coordinates(#[case] link: bool, #[case] array: bool) {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join("lib.rs");
        let uri_encoding = Url::from_file_path(&source);
        let uri = assert_ok!(uri_encoding, "The definition fixture path must encode as a file URI");
        let range = json!({"start": {"line": 7, "character": 13}, "end": {"line": 7, "character": 17}});
        let response = if link {
            json!({
                "targetUri": uri.as_str(),
                "targetSelectionRange": range,
                "targetRange": {"start": {"line": 0, "character": 0}, "end": {"line": 20, "character": 0}}
            })
        } else {
            json!({"uri": uri.as_str(), "range": range})
        };
        let response = if array { json!([response]) } else { response };

        let locations_parse = definition_locations(&response);
        let system_under_test = assert_ok!(locations_parse, "The definition response must decode its location coordinates");
        let location = system_under_test.first().map(|location| (location.file(), *location.line(), *location.column()));

        assert_eq!(system_under_test.len(), 1);
        assert_eq!(location, Some((&source, 7, 13)));
    }

    #[rstest]
    #[case::missing_uri(json!({}))]
    #[case::remote_uri(json!({"uri":"https://example.com/file.rs", "range":{"start":{"line":0,"character":0}}}))]
    #[case::negative_line(json!({"uri":"file:///C:/fixture.rs", "range":{"start":{"line":-1,"character":0}}}))]
    #[case::negative_column(json!({"uri":"file:///C:/fixture.rs", "range":{"start":{"line":0,"character":-1}}}))]
    #[case::overflowing_line(json!({
        "uri":"file:///C:/fixture.rs", "range":{"start":{"line":u64::from(u32::MAX) + 1,"character":0}}
    }))]
    #[case::overflowing_column(json!({
        "uri":"file:///C:/fixture.rs", "range":{"start":{"line":0,"character":u64::from(u32::MAX) + 1}}
    }))]
    fn test_invalid_definition_locations(#[case] response: Value) {
        let system_under_test = definition_locations(&response);

        assert!(system_under_test.is_err(), "Invalid semantic definition was accepted");
    }

    #[rstest]
    #[case(json!(null))]
    #[case(json!([]))]
    fn test_empty_definition_locations(#[case] response: Value) {
        let result = definition_locations(&response);
        let system_under_test = assert_ok!(result, "An empty definition response must be accepted");

        assert!(system_under_test.is_empty(), "An empty response must not invent definition locations");
    }

    #[rstest]
    #[case::first_position(0, 0, ":1:1")]
    #[case::nonzero_position(7, 13, ":8:14")]
    #[case::maximum_position(u32::MAX, u32::MAX, ":4294967296:4294967296")]
    fn test_canonical_definition_file_error_context(#[case] line: u32, #[case] column: u32, #[case] expected_suffix: &str) {
        let result = tempdir();
        let fixture = assert_ok!(result, "The missing definition fixture directory must be created");
        let location = DefinitionLocation { file: fixture.path().join("missing.rs"), line, column };
        let expected = format!("{MESSAGE_DEFINITION_FILE_RESOLUTION_ERROR} {}{expected_suffix}", location.file().display());

        let system_under_test = canonical_definition_file(&location);

        let error = system_under_test.as_ref().err();
        let cause = error.and_then(|error| error.downcast_ref::<io::Error>());
        assert_eq!(error.map(ToString::to_string), Some(expected), "Definition error lost its path or one-based coordinates");
        assert_eq!(cause.map(io::Error::kind), Some(io::ErrorKind::NotFound), "Canonicalization lost its filesystem error");
    }

    #[rstest]
    #[case::direct(false)]
    #[case::alias(true)]
    fn test_canonical_definition_file(#[case] alias: bool) {
        const FILE_NAME: &str = "definition.rs";
        let fixture_creation = tempdir();
        let fixture = assert_ok!(fixture_creation, "The canonical definition fixture directory must be created");
        let directory_resolution = dunce::canonicalize(fixture.path());
        let directory = assert_ok!(directory_resolution, "The definition fixture directory must have a canonical path");
        let expected = directory.join(FILE_NAME);
        let definition_write = fs::write(&expected, b"");
        assert_ok!(definition_write, "The definition fixture file must be written");
        let nested = directory.join("nested");
        let nested_creation = fs::create_dir_all(&nested);
        assert_ok!(nested_creation, "The nested directory for the definition path alias must be created");
        let file = if alias { nested.join("..").join(FILE_NAME) } else { expected.clone() };
        let location = DefinitionLocation { file, line: 7, column: 13 };

        let definition_resolution = canonical_definition_file(&location);
        let system_under_test = assert_ok!(definition_resolution, "The definition file must resolve to its canonical path");

        assert_eq!(system_under_test, expected);
    }

    #[test]
    fn test_definition_configuration_preserves_analysis_settings() {
        let fixture_creation = NamedTempFile::new();
        let fixture = assert_ok!(fixture_creation, "The definition configuration fixture file must be created");
        let configuration = json!({"cfg":{"setTest":false}, "cargo":{"features":["feature"]}, "cachePriming":{"numThreads":3}});
        let configuration_encoding = serde_json::to_vec(&configuration);
        let configuration_bytes = assert_ok!(configuration_encoding, "The fixture configuration must serialize to JSON");
        let configuration_write = fs::write(fixture.path(), configuration_bytes);
        assert_ok!(configuration_write, "The fixture analyzer configuration must be written before loading");

        let configuration_read = definition_configuration(Some(fixture.path()));
        let system_under_test = assert_ok!(configuration_read, "Definition lookup must load the existing analyzer configuration");

        assert_eq!(
            system_under_test,
            json!({
                "cfg":{"setTest":false},
                "cargo":{"features":["feature"]},
                "cachePriming":{"numThreads":3,"enable":false},
                "diagnostics":{"enable":false},
                "checkOnSave":false
            })
        );
    }

    #[test]
    fn test_workspace_configuration_sections() {
        let fixture = json!({"cfg":{"setTest":false}, "cargo":{"features":["feature"]}});
        let request = json!({"items":[
            {}, {"section":""}, {"section":"rust-analyzer"}, {"section":"rust-analyzer.cfg.setTest"},
            {"section":"cargo.features"}, {"section":"missing"}
        ]});

        let result = configuration_response(&fixture, &request);
        let system_under_test = assert_ok!(result, "A valid workspace configuration request must resolve its sections");

        assert_eq!(system_under_test, json!([fixture, fixture, fixture, false, ["feature"], null]));
    }

    #[rstest]
    #[case::not_an_object(json!(42))]
    #[case::missing_items(json!({}))]
    #[case::invalid_items(json!({"items":{}}))]
    #[case::numeric_item(json!({"items":[42]}))]
    #[case::null_item(json!({"items":[null]}))]
    #[case::numeric_section(json!({"items":[{"section":7}]}))]
    #[case::null_section(json!({"items":[{"section":null}]}))]
    #[case::boolean_section(json!({"items":[{"section":false}]}))]
    fn test_malformed_workspace_configuration(#[case] request: Value) {
        let system_under_test = configuration_response(&json!({"cfg":{"setTest":false}}), &request);

        assert!(system_under_test.is_err(), "Malformed configuration request was treated as whole configuration");
    }

    #[rstest]
    #[case::notification(Message::Notification(Notification::new("$/progress".into(), json!({}))), None)]
    #[case::other_response(Message::Response(Response::new_ok(RequestId::from(2), json!("other"))), None)]
    #[case::requested_response(
        Message::Response(Response::new_ok(RequestId::from(1), json!("definition"))), Some(json!("definition"))
    )]
    fn test_protocol_response_selection(#[case] message: Message, #[case] expected: Option<Value>) {
        let result = matching_response(&message, &RequestId::from(1));
        let system_under_test = assert_ok!(result, "Selecting a response must accept valid responses and unrelated messages");

        assert_eq!(system_under_test, expected);
    }

    #[test]
    fn test_protocol_error_response() {
        let fixture = Message::Response(Response::new_err(RequestId::from(1), -32_603, "Workspace loading failed".into()));

        let system_under_test = matching_response(&fixture, &RequestId::from(1));

        assert!(system_under_test.is_err(), "Definition error response was accepted");
    }
}
