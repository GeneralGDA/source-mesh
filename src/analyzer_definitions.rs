use std::fs;
use std::io::BufReader;
use std::io::copy;
use std::io::stderr;
use std::io::Read as _;
use std::io::Seek as _;
use std::io::SeekFrom;
use std::path::Path;
use std::path::PathBuf;
use std::process::Child;
use std::process::ChildStdin;
use std::process::Command;
use std::process::Stdio;
use std::sync::mpsc;
use std::sync::mpsc::Receiver;
use std::thread;
use std::time::Duration;
use std::time::Instant;

use anyhow::Context as _;
use anyhow::Result;
use anyhow::anyhow;
use anyhow::bail;
use anyhow::ensure;
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

const INITIALIZATION_TIMEOUT: Duration = Duration::from_mins(10);
const DEFINITION_TIMEOUT: Duration = Duration::from_mins(5);
const SERVER_STATUS: &str = "experimental/serverStatus";

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
    messages: Receiver<Result<Message>>,
    configuration: Value,
    next_request: i32,
}

struct AnalyzerProcess {
    child: Child,
    diagnostics: NamedTempFile,
    verbose: bool,
}

impl DefinitionResolver {
    pub(crate) fn start(project: &Path, config: Option<&Path>, verbose: bool) -> Result<Self> {
        ensure!(project.is_dir(), "Definition lookup needs a project directory: {}", project.display());
        let project = dunce::canonicalize(project).context("Cannot resolve definition lookup project")?;
        let configuration = definition_configuration(config)?;
        let diagnostics = NamedTempFile::new().context("Cannot capture rust-analyzer definition diagnostics")?;
        let child = Command::new("rust-analyzer")
            .current_dir(&project)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(diagnostics.reopen().context("Cannot open definition diagnostics output")?)
            .spawn()
            .context("Cannot start rust-analyzer for semantic definition lookup")?;
        let mut process = AnalyzerProcess { child, diagnostics, verbose };
        let input = process.child.stdin.take().context("rust-analyzer definition input is unavailable")?;
        let output = process.child.stdout.take().context("rust-analyzer definition output is unavailable")?;
        let (sender, messages) = mpsc::channel();
        thread::Builder::new()
            .name("rust-analyzer-definitions".into())
            .spawn(move || {
                let mut output = BufReader::new(output);
                loop {
                    match Message::read(&mut output) {
                        Ok(Some(message)) => {
                            if sender.send(Ok(message)).is_err() {
                                break;
                            }
                        }
                        Ok(None) => break,
                        Err(error) => {
                            if sender.send(Err(error.into())).is_err() {
                                return;
                            }
                            break;
                        }
                    }
                }
            })
            .context("Cannot read rust-analyzer definition responses")?;
        let mut resolver = Self { process, input, messages, configuration, next_request: 0 };
        if verbose {
            eprintln!("Loading rust-analyzer semantic definitions for {}...", project.display());
        }
        resolver.initialize(&project).with_context(|| resolver.process.failure_context())?;
        Ok(resolver)
    }

    pub(crate) fn definitions(&mut self, file: &Path, line: u32, column: u32) -> Result<Vec<DefinitionLocation>> {
        ensure!(file.is_absolute(), "Definition lookup needs an absolute source path: {}", file.display());
        let url = Url::from_file_path(file).map_err(|()| anyhow!("Cannot encode source path {} as a file URI", file.display()))?;
        let started = Instant::now();
        if self.process.verbose {
            eprintln!("Looking up definition at {}:{}:{}...", file.display(), line + 1, column + 1);
        }
        let result = self.request(
            "textDocument/definition",
            json!({"textDocument": {"uri": url.as_str()}, "position": {"line": line, "character": column}}),
            DEFINITION_TIMEOUT,
        ).with_context(|| self.process.failure_context())?;
        if self.process.verbose {
            eprintln!("Definition lookup completed in {:.2}s.", started.elapsed().as_secs_f64());
        }
        definition_locations(&result)
    }

    fn initialize(&mut self, project: &Path) -> Result<()> {
        let root = Url::from_directory_path(project)
            .map_err(|()| anyhow!("Cannot encode project directory {} as a file URI", project.display()))?;
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
            if let Message::Notification(notification) = message
                && notification.method == SERVER_STATUS
                && notification.params.get("quiescent").and_then(Value::as_bool) == Some(true)
            {
                ensure!(
                    notification.params.get("health").and_then(Value::as_str) != Some("error"),
                    "rust-analyzer cannot load the project for definition lookup: {}",
                    notification.params.get("message").and_then(Value::as_str).unwrap_or("unknown workspace loading error")
                );
                if self.process.verbose && notification.params.get("health").and_then(Value::as_str) == Some("warning") {
                    let warning = notification.params.get("message").and_then(Value::as_str)
                        .unwrap_or("Workspace loading reported a warning; run with --verbose for analyzer diagnostics")
                        .split_whitespace().collect::<Vec<_>>().join(" ");
                    let warning: String = warning.chars().take(512).collect();
                    eprintln!("warning: Project loading: {warning}");
                }
                return Ok(());
            }
        }
    }

    fn request(&mut self, method: &str, parameters: Value, timeout: Duration) -> Result<Value> {
        self.next_request = self.next_request.checked_add(1).context("Too many definition lookup requests")?;
        let request_id = RequestId::from(self.next_request);
        Message::Request(Request { id: request_id.clone(), method: method.into(), params: parameters })
            .write(&mut self.input)
            .with_context(|| format!("Cannot send rust-analyzer {method} request"))?;
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(result) = matching_response(&self.receive(deadline)?, &request_id)? {
                return Ok(result);
            }
        }
    }

    fn receive(&mut self, deadline: Instant) -> Result<Message> {
        let remaining = deadline.checked_duration_since(Instant::now()).context("rust-analyzer definition lookup exceeded its timeout")?;
        let message = self.messages.recv_timeout(remaining)
            .context("rust-analyzer definition lookup stopped responding or exceeded its timeout")??;
        if let Message::Request(ref request) = message {
            let response = match request.method.as_str() {
                "workspace/configuration" => Response::new_ok(request.id.clone(), configuration_response(&self.configuration, &request.params)?),
                "window/workDoneProgress/create" | "client/registerCapability" => Response::new_ok(request.id.clone(), Value::Null),
                _ => Response::new_err(request.id.clone(), -32_601, format!("Unsupported client request: {}", request.method)),
            };
            Message::Response(response).write(&mut self.input).context("Cannot answer rust-analyzer client request")?;
        }
        Ok(message)
    }
}

impl AnalyzerProcess {
    fn failure_context(&self) -> String {
        if !self.verbose {
            return "Dependency resolution failed. Use --verbose for diagnostic details.".into();
        }
        let details = (|| -> Result<String> {
            let mut file = self.diagnostics.reopen()?;
            let length = file.metadata()?.len();
            file.seek(SeekFrom::Start(length.saturating_sub(8_192)))?;
            let mut output = Vec::new();
            file.take(8_192).read_to_end(&mut output)?;
            Ok(String::from_utf8_lossy(&output).trim().to_owned())
        })();
        match details {
            Ok(details) if !details.is_empty() => format!("Semantic definition lookup failed. rust-analyzer diagnostics:\n{details}"),
            Ok(_) | Err(_) => "Semantic definition lookup failed".into(),
        }
    }
}

impl Drop for AnalyzerProcess {
    fn drop(&mut self) {
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            match self.child.kill() {
                Ok(()) => {
                    if let Err(error) = self.child.wait() {
                        eprintln!("warning: Cannot collect rust-analyzer definition lookup process: {error}");
                    }
                }
                Err(error) => {
                    if !matches!(self.child.try_wait(), Ok(Some(_))) {
                        eprintln!("warning: Cannot stop rust-analyzer definition lookup: {error}");
                    }
                }
            }
        }
        if self.verbose {
            let output = self.diagnostics.reopen().and_then(|mut file| copy(&mut file, &mut stderr().lock()));
            if let Err(error) = output {
                eprintln!("warning: Cannot display rust-analyzer definition diagnostics: {error}");
            }
        }
    }
}

fn definition_configuration(path: Option<&Path>) -> Result<Value> {
    let mut configuration = if let Some(path) = path {
        serde_json::from_slice(&fs::read(path).with_context(|| format!("Cannot read rust-analyzer configuration {}", path.display()))?)?
    } else {
        json!({})
    };
    let settings = configuration.as_object_mut().context("rust-analyzer configuration must be a JSON object")?;
    settings.insert("checkOnSave".into(), Value::Bool(false));
    for group in ["cachePriming", "diagnostics"] {
        settings.entry(group).or_insert_with(|| json!({})).as_object_mut()
            .with_context(|| format!("rust-analyzer {group} settings must be a JSON object"))?
            .insert("enable".into(), Value::Bool(false));
    }
    Ok(configuration)
}

fn configuration_response(configuration: &Value, parameters: &Value) -> Result<Value> {
    let items = parameters.get("items").and_then(Value::as_array).context("Invalid rust-analyzer configuration request")?;
    Ok(Value::Array(items.iter().map(|item| {
        match item.get("section").and_then(Value::as_str) {
            None | Some("" | "rust-analyzer") => configuration.clone(),
            Some(section) => section.strip_prefix("rust-analyzer.").unwrap_or(section)
                .split('.').try_fold(configuration, |value, name| value.get(name))
                .cloned().unwrap_or(Value::Null),
        }
    }).collect()))
}

fn matching_response(message: &Message, request_id: &RequestId) -> Result<Option<Value>> {
    if let Message::Response(ref response) = *message
        && &response.id == request_id
    {
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
    locations.iter().map(|location| {
        let (uri, range) = if let Some(uri) = location.get("targetUri") {
            (uri, location.get("targetSelectionRange"))
        } else {
            (location.get("uri").context("Definition response is missing its URI")?, location.get("range"))
        };
        let file = Url::parse(uri.as_str().context("Definition URI must be a string")?)?
            .to_file_path().map_err(|()| anyhow!("rust-analyzer definition does not point to a local file"))?;
        let start = range.and_then(|range| range.get("start")).context("Definition response is missing its start position")?;
        let line = u32::try_from(start.get("line").and_then(Value::as_u64).context("Invalid definition line")?)?;
        let column = u32::try_from(start.get("character").and_then(Value::as_u64).context("Invalid definition column")?)?;
        Ok(DefinitionLocation { file, line, column })
    }).collect()
}

#[cfg(test)]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Tests use assertions for expectations while returning Result for fallible setup."
)]
mod tests {
    use std::path::Path;

    use anyhow::Result;
    use anyhow::Context as _;
    use lsp_server::Message;
    use lsp_server::Notification;
    use lsp_server::RequestId;
    use lsp_server::Response;
    use pretty_assertions::assert_eq;
    use rstest::rstest;
    use serde_json::Value;
    use serde_json::json;
    use url::Url;

    use super::configuration_response;
    use super::definition_configuration;
    use super::definition_locations;
    use super::matching_response;

    #[rstest]
    #[case::location(false, false)]
    #[case::locations(false, true)]
    #[case::location_link(true, true)]
    fn test_definition_location_coordinates(#[case] link: bool, #[case] array: bool) -> Result<()> {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join("main.rs");
        let uri = Url::from_file_path(&source).map_err(|()| anyhow::anyhow!("Cannot encode fixture path"))?;
        let range = json!({"start": {"line": 7, "character": 13}, "end": {"line": 7, "character": 17}});
        let response = if link {
            json!({"targetUri": uri.as_str(), "targetSelectionRange": range, "targetRange": {"start": {"line": 0, "character": 0}, "end": {"line": 20, "character": 0}}})
        } else {
            json!({"uri": uri.as_str(), "range": range})
        };
        let response = if array { json!([response]) } else { response };

        let system_under_test = definition_locations(&response)?;
        let location = system_under_test.first().context("Missing decoded definition")?;

        assert_eq!(system_under_test.len(), 1);
        assert_eq!(location.file(), &source);
        assert_eq!(*location.line(), 7);
        assert_eq!(*location.column(), 13);
        Ok(())
    }

    #[rstest]
    #[case(json!({}))]
    #[case(json!({"uri":"https://example.com/file.rs", "range":{"start":{"line":0,"character":0}}}))]
    #[case(json!({"uri":"file:///fixture.rs", "range":{"start":{"line":-1,"character":0}}}))]
    #[case(json!({"uri":"file:///fixture.rs", "range":{"start":{"line":0,"character":4_294_967_296_u64}}}))]
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
        let fixture = tempfile::NamedTempFile::new()?;
        std::fs::write(fixture.path(), serde_json::to_vec(&json!({"cfg":{"setTest":false}, "cargo":{"features":["feature"]}, "cachePriming":{"numThreads":3}}))?)?;

        let system_under_test = definition_configuration(Some(fixture.path()))?;

        assert_eq!(system_under_test, json!({"cfg":{"setTest":false}, "cargo":{"features":["feature"]}, "cachePriming":{"numThreads":3,"enable":false}, "diagnostics":{"enable":false}, "checkOnSave":false}));
        Ok(())
    }

    #[test]
    fn test_workspace_configuration_sections() -> Result<()> {
        let fixture = json!({"cfg":{"setTest":false}, "cargo":{"features":["feature"]}});
        let request = json!({"items":[{"section":"rust-analyzer"},{"section":"rust-analyzer.cfg.setTest"},{"section":"cargo.features"},{"section":"missing"}]});

        let system_under_test = configuration_response(&fixture, &request)?;

        assert_eq!(system_under_test, json!([fixture, false, ["feature"], null]));
        Ok(())
    }

    #[rstest]
    #[case::notification(Message::Notification(Notification::new("$/progress".into(), json!({}))), None)]
    #[case::other_response(Message::Response(Response::new_ok(RequestId::from(2), json!("other"))), None)]
    #[case::requested_response(Message::Response(Response::new_ok(RequestId::from(1), json!("definition"))), Some(json!("definition")))]
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
