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

const INITIALIZATION_TIMEOUT: Duration = Duration::from_mins(10);
const DEFINITION_TIMEOUT: Duration = Duration::from_mins(5);
const READER_POLL_INTERVAL: Duration = Duration::from_millis(10);
const SERVER_STATUS: &str = "experimental/serverStatus";
const WORKSPACE_LOADING_WARNING: &str = "Workspace loading reported a warning; run with --verbose for analyzer diagnostics";
const WORKSPACE_LOADING_ERROR: &str = "unknown workspace loading error";

#[derive(Getters)]
#[getset(get = "pub(crate)")]
pub(crate) struct DefinitionLocation {
    file: PathBuf,
    line: u32,
    column: u32,
}

pub(crate) struct DefinitionResolver {
    /* Field drop order stops the child before releasing its captured stdout. */
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
        let result = self.request("textDocument/definition", parameters, DEFINITION_TIMEOUT);
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
            INITIALIZATION_TIMEOUT,
        )?;
        ensure!(
            initialization.pointer("/capabilities/positionEncoding").and_then(Value::as_str) == Some("utf-8"),
            "rust-analyzer did not negotiate UTF-8 positions required for SCIP definition lookup; update rust-analyzer"
        );
        Message::Notification(Notification::new("initialized".into(), json!({}))).write(&mut self.input)?;
        let deadline = Instant::now() + INITIALIZATION_TIMEOUT;
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
            thread::sleep(READER_POLL_INTERVAL.min(remaining));
        }
    }

    fn consume(&mut self, amount: usize) {
        self.output.consume(amount);
    }
}

impl AnalyzerProcess {
    #[must_use]
    fn failure_context(&self) -> String {
        const MAXIMUM_DIAGNOSTIC_BYTES: u64 = 8_192;
        const FAILURE_MESSAGE: &str = "Semantic definition lookup failed";

        if !self.verbose {
            return format!("Dependency resolution failed. {MESSAGE_VERBOSE_DIAGNOSTICS_HINT}");
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
            Ok(details) if !details.is_empty() => format!("{FAILURE_MESSAGE}. rust-analyzer diagnostics:\n{details}"),
            Ok(_) => FAILURE_MESSAGE.into(),
            Err(error) => {
                emit(format_args!("warning: Cannot read rust-analyzer definition diagnostics: {error}"));
                FAILURE_MESSAGE.into()
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
                    if !matches!(self.child.try_wait(), Ok(Some(_))) {
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
        (true, "warning") => Ok(InitializationStatus::Warning(message.unwrap_or(WORKSPACE_LOADING_WARNING))),
        (true, "error") => {
            bail!("rust-analyzer cannot load the project for definition lookup: {}", message.unwrap_or(WORKSPACE_LOADING_ERROR))
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

#[cfg(test)]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Tests use assertions for expectations while returning Result for fallible setup."
)]
mod tests {
    use std::fs;
    use std::fs::File;
    use std::io;
    use std::io::BufRead as _;
    use std::io::BufReader;
    use std::io::Cursor;
    use std::io::Read;
    use std::io::Write as _;
    use std::panic::catch_unwind;
    use std::panic::resume_unwind;
    use std::path::Path;
    use std::sync::Arc;
    use std::time::Duration;
    use std::time::Instant;

    use anyhow::Result;
    use anyhow::anyhow;
    use lsp_server::Message;
    use lsp_server::Notification;
    use lsp_server::RequestId;
    use lsp_server::Response;
    use pretty_assertions::assert_eq;
    use rstest::rstest;
    use serde_json::Value;
    use serde_json::json;
    use tempfile::NamedTempFile;
    use url::Url;

    use super::DefinitionReader;
    use super::InitializationStatus;
    use super::WORKSPACE_LOADING_ERROR;
    use super::WORKSPACE_LOADING_WARNING;
    use super::configuration_response;
    use super::definition_configuration;
    use super::definition_locations;
    use super::initialization_status;
    use super::matching_response;

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
    fn test_reader_partial_frames(#[case] split_in_body: bool, #[values(false, true)] process_exited: bool) -> Result<()> {
        const METHOD: &str = "fixture/fragmented";
        let (mut output, mut writer) = definition_output_fixture()?;
        let mut wire = Vec::new();
        Message::Notification(Notification::new(METHOD.into(), json!({}))).write(&mut wire)?;
        let (prefix, suffix) = wire.split_at(if split_in_body { wire.len() - 1 } else { 1 });
        writer.write_all(prefix)?;
        let mut exit_checks = 0;

        let message = {
            let mut system_under_test = DefinitionReader::new(
                &mut output,
                || {
                    exit_checks += 1;
                    writer.write_all(suffix)?;
                    Ok(process_exited)
                },
                Instant::now() + Duration::from_secs(5),
            );
            system_under_test.receive()?
        };

        assert!(
            matches!(message, Message::Notification(notification) if notification.method == METHOD),
            "A partial frame was not completed, including bytes written just before process exit"
        );
        assert_eq!(exit_checks, 1, "Temporary EOF must trigger exactly one exit check for this frame");
        Ok(())
    }

    #[test]
    fn test_reader_buffered_messages() -> Result<()> {
        const METHOD: &str = "fixture/buffered";
        let (mut output, mut writer) = definition_output_fixture()?;
        Message::Notification(Notification::new(METHOD.into(), json!(1))).write(&mut writer)?;
        Message::Notification(Notification::new(METHOD.into(), json!(2))).write(&mut writer)?;
        let deadline = Instant::now() + Duration::from_secs(5);

        let first = {
            let has_exited = || unreachable!("Buffered output must not poll the process");
            let mut system_under_test = DefinitionReader::new(&mut output, has_exited, deadline);
            system_under_test.receive()?
        };
        let second = {
            let has_exited = || unreachable!("The next response must retain bytes buffered by the previous read");
            let mut system_under_test = DefinitionReader::new(&mut output, has_exited, deadline);
            system_under_test.receive()?
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
        Ok(())
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
    fn test_reader_deadline_with_buffered_message() -> Result<()> {
        let mut wire = Vec::new();
        Message::Notification(Notification::new("fixture/expired".into(), json!({}))).write(&mut wire)?;
        let mut output = BufReader::new(Cursor::new(wire));
        let buffered = output.fill_buf()?;
        assert!(!buffered.is_empty(), "The fixture must contain a buffered message");
        let has_exited = || unreachable!("An expired read must not poll the process");
        let mut system_under_test = DefinitionReader::new(&mut output, has_exited, Instant::now());

        let result = system_under_test.receive();

        assert!(
            result.is_err_and(|error| {
                error.downcast_ref::<io::Error>().is_some_and(|cause| cause.kind() == io::ErrorKind::TimedOut)
            }),
            "Buffered bytes must not bypass the operation deadline"
        );
        Ok(())
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
            assert_eq!(empty_read.ok(), Some(0), "An empty read must not access the file or wait");
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
    #[case::loading_error_null_message(
        json!({"quiescent":false,"health":"error","message":null}), InitializationStatus::Loading
    )]
    #[case::ready_null_message(json!({"quiescent":true,"health":"ok","message":null}), InitializationStatus::Ready)]
    #[case::warning_null_message(
        json!({"quiescent":true,"health":"warning","message":null}),
        InitializationStatus::Warning(WORKSPACE_LOADING_WARNING)
    )]
    #[case::warning(
        json!({"quiescent":true,"health":"warning","message":"Workspace warning"}),
        InitializationStatus::Warning("Workspace warning")
    )]
    #[case::warning_without_message(
        json!({"quiescent":true,"health":"warning"}),
        InitializationStatus::Warning(WORKSPACE_LOADING_WARNING)
    )]
    fn test_initialization_status(#[case] parameters: Value, #[case] expected: InitializationStatus<'_>) {
        let system_under_test = initialization_status(&parameters);

        assert_eq!(system_under_test.as_ref().ok(), Some(&expected));
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
            system_under_test.is_err_and(|error| error.to_string().ends_with(WORKSPACE_LOADING_ERROR)),
            "An absent or null message must preserve the workspace failure and its fallback diagnostic"
        );
    }

    #[rstest]
    #[case::location(false, false)]
    #[case::locations(false, true)]
    #[case::location_link(true, true)]
    fn test_definition_location_coordinates(#[case] link: bool, #[case] array: bool) -> Result<()> {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join("lib.rs");
        let uri = Url::from_file_path(&source).map_err(|()| anyhow!("Cannot encode fixture path"))?;
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

        let system_under_test = definition_locations(&response)?;
        let location = system_under_test.first().map(|location| (location.file(), *location.line(), *location.column()));

        assert_eq!(system_under_test.len(), 1);
        assert_eq!(location, Some((&source, 7, 13)));
        Ok(())
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
    fn test_empty_definition_locations(#[case] response: Value) -> Result<()> {
        let system_under_test = definition_locations(&response)?;

        assert!(system_under_test.is_empty());
        Ok(())
    }

    #[test]
    fn test_definition_configuration_preserves_analysis_settings() -> Result<()> {
        let fixture = NamedTempFile::new()?;
        let configuration = json!({"cfg":{"setTest":false}, "cargo":{"features":["feature"]}, "cachePriming":{"numThreads":3}});
        fs::write(fixture.path(), serde_json::to_vec(&configuration)?)?;

        let system_under_test = definition_configuration(Some(fixture.path()))?;

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
        Ok(())
    }

    #[test]
    fn test_workspace_configuration_sections() -> Result<()> {
        let fixture = json!({"cfg":{"setTest":false}, "cargo":{"features":["feature"]}});
        let request = json!({"items":[
            {}, {"section":""}, {"section":"rust-analyzer"}, {"section":"rust-analyzer.cfg.setTest"},
            {"section":"cargo.features"}, {"section":"missing"}
        ]});

        let system_under_test = configuration_response(&fixture, &request)?;

        assert_eq!(system_under_test, json!([fixture, fixture, fixture, false, ["feature"], null]));
        Ok(())
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
    fn test_protocol_response_selection(#[case] message: Message, #[case] expected: Option<Value>) -> Result<()> {
        let system_under_test = matching_response(&message, &RequestId::from(1))?;

        assert_eq!(system_under_test, expected);
        Ok(())
    }

    #[test]
    fn test_protocol_error_response() {
        let fixture = Message::Response(Response::new_err(RequestId::from(1), -32_603, "Workspace loading failed".into()));

        let system_under_test = matching_response(&fixture, &RequestId::from(1));

        assert!(system_under_test.is_err(), "Definition error response was accepted");
    }
}
