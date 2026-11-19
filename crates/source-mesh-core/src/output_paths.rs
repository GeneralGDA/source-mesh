use std::error::Error;
use std::fmt;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::Read as _;
use std::io::Seek as _;
use std::io::Write as _;
use std::path::Path;
use std::path::PathBuf;
use std::path::absolute;

use anyhow::Context as _;
use anyhow::Result;
use anyhow::ensure;
use derive_new::new;
use getset::CopyGetters;
use same_file::Handle;

const OUTPUT_PATH_CONFLICT: &str = "Output files must use distinct paths and must not overwrite input files";

#[derive(Debug, new)]
pub(crate) struct OutputPathConflict {
    path: PathBuf,
    other_path: PathBuf,
}

impl fmt::Display for OutputPathConflict {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{OUTPUT_PATH_CONFLICT}: {} and {}", self.path.display(), self.other_path.display())
    }
}

impl Error for OutputPathConflict {}

#[derive(Debug)]
enum OutputIoError {
    Resolve(PathBuf),
    Open(PathBuf),
    Identify(PathBuf),
    Truncate(PathBuf),
    Seek(PathBuf),
    Write(PathBuf),
}

impl fmt::Display for OutputIoError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Resolve(ref path) => write!(formatter, "Cannot resolve output path {}", path.display()),
            Self::Open(ref path) => write!(formatter, "Cannot open output file {}", path.display()),
            Self::Identify(ref path) => write!(formatter, "Cannot identify output file {}", path.display()),
            Self::Truncate(ref path) => write!(formatter, "Cannot truncate output file {}", path.display()),
            Self::Seek(ref path) => write!(formatter, "Cannot seek output file {}", path.display()),
            Self::Write(ref path) => write!(formatter, "Cannot write output file {}", path.display()),
        }
    }
}

impl Error for OutputIoError {}


pub struct InputFile {
    path: PathBuf,
    handle: Handle,
}

impl InputFile {
    pub fn open(path: &Path) -> Result<Self> {
        let path = absolute(path).with_context(|| format!("Cannot resolve input path {}", path.display()))?;
        let file = File::open(&path).with_context(|| format!("Cannot open input file {}", path.display()))?;
        let handle = Handle::from_file(file).with_context(|| format!("Cannot identify input file {}", path.display()))?;
        Ok(Self { path, handle })
    }


    pub fn read(&mut self) -> Result<Vec<u8>> {
        let mut contents = Vec::new();
        let read_result = self.handle.as_file_mut().read_to_end(&mut contents);
        read_result.with_context(|| format!("Cannot read input file {}", self.path.display()))?;
        Ok(contents)
    }
}

#[derive(Clone, Copy, CopyGetters, new)]
pub(crate) struct FileOutput<'a> {
    #[getset(get_copy = "pub(crate)")]
    path: &'a Path,
    contents: &'a [u8],
}

struct OpenedOutput<'a> {
    output: FileOutput<'a>,
    handle: Handle,
}

impl OpenedOutput<'_> {
    fn write(&mut self, contents: &[u8]) -> Result<()> {
        let path = self.output.path;
        let output = self.handle.as_file_mut();
        output.set_len(0).with_context(|| OutputIoError::Truncate(path.to_path_buf()))?;
        output.rewind().with_context(|| OutputIoError::Seek(path.to_path_buf()))?;
        output.write_all(contents).with_context(|| OutputIoError::Write(path.to_path_buf()))
    }
}

pub(crate) struct OutputFiles<'a> {
    files: Vec<OpenedOutput<'a>>,
}

impl<'a> OutputFiles<'a> {
    /**
    Newly created empty files may remain on failure: removing their paths could delete replacement
    files, so no path-based rollback is attempted.
    */
    pub(crate) fn open(outputs: &[FileOutput<'a>], inputs: &[&InputFile]) -> Result<Self> {
        let paths = outputs.iter().map(|output| {
            absolute(output.path).with_context(|| OutputIoError::Resolve(output.path.to_path_buf()))
        }).collect::<Result<Vec<_>>>()?;
        for (index, path) in paths.iter().enumerate() {
            for other in paths.iter().take(index).chain(inputs.iter().map(|input| &input.path)) {
                ensure!(path != other, OutputPathConflict::new(path.clone(), other.clone()));
            }
        }

        let mut files: Vec<OpenedOutput<'a>> = Vec::with_capacity(outputs.len());
        for (output, path) in outputs.iter().zip(paths) {
            let file = OpenOptions::new().write(true).create(true).truncate(false).open(&path);
            let file = file.with_context(|| OutputIoError::Open(output.path.to_path_buf()))?;
            let handle = Handle::from_file(file);
            let handle = handle.with_context(|| OutputIoError::Identify(output.path.to_path_buf()))?;
            let output_identities = files.iter().map(|opened| (&opened.handle, opened.output.path));
            let input_identities = inputs.iter().map(|input| (&input.handle, input.path.as_path()));
            for (other_handle, other_path) in output_identities.chain(input_identities) {
                let distinct = &handle != other_handle;
                ensure!(distinct, OutputPathConflict::new(output.path.to_path_buf(), other_path.to_path_buf()));
            }
            files.push(OpenedOutput { output: *output, handle });
        }
        Ok(Self { files })
    }

    pub(crate) fn write_output(&mut self, output: &FileOutput<'_>) -> Result<()> {
        let file = self.files.iter_mut().find(|file| file.output.path == output.path);
        let file = file.with_context(|| format!("Output file was not opened and validated: {}", output.path.display()))?;
        file.write(output.contents)
    }

    pub(crate) fn write(self) -> Result<()> {
        for mut file in self.files {
            file.write(file.output.contents)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::fs::File;
    use std::io::Error;
    use std::io::ErrorKind;
    use std::io::Seek as _;
    use std::io::SeekFrom;
    use std::path::Path;
    use std::path::PathBuf;

    use assertables::assert_contains;
    use claims::assert_ok;
    use claims::assert_some;
    use pretty_assertions::assert_eq;
    use rstest::rstest;
    use same_file::Handle;
    use tempfile::TempDir;
    use tempfile::tempdir;

    use super::FileOutput;
    use super::InputFile;
    use super::OUTPUT_PATH_CONFLICT;
    use super::OpenedOutput;
    use super::OutputFiles;
    use super::OutputIoError;
    use super::OutputPathConflict;

    fn output_paths_test_fixture() -> TempDir {
        let directory = tempdir();
        assert_ok!(directory, "Failed to create the output-path fixture directory")
    }

    fn write_file_test_fixture(path: &Path, contents: &[u8]) {
        let written = fs::write(path, contents);
        assert_ok!(written, "Failed to write fixture file {}", path.display());
    }

    fn read_file_test_fixture(path: &Path) -> Vec<u8> {
        let contents = fs::read(path);
        assert_ok!(contents, "Failed to read fixture file {}", path.display())
    }

    #[test]
    fn test_output_path_conflict_diagnostic() {
        let system_under_test = OutputPathConflict::new(PathBuf::from("report.md"), PathBuf::from("index.scip"));

        let diagnostic = system_under_test.to_string();

        assert_eq!(diagnostic, format!("{OUTPUT_PATH_CONFLICT}: report.md and index.scip"));
    }

    #[rstest]
    #[case::resolve(OutputIoError::Resolve)]
    #[case::open(OutputIoError::Open)]
    #[case::identify(OutputIoError::Identify)]
    #[case::truncate(OutputIoError::Truncate)]
    #[case::seek(OutputIoError::Seek)]
    #[case::write(OutputIoError::Write)]
    fn test_output_io_error_path_diagnostic(#[case] failure: fn(PathBuf) -> OutputIoError) {
        const NAME_OUTPUT: &str = "report with spaces.md";
        let system_under_test = failure(PathBuf::from(NAME_OUTPUT));

        let diagnostic = system_under_test.to_string();

        assert!(diagnostic.ends_with(NAME_OUTPUT), "Output I/O diagnostics must identify the requested file: {diagnostic}");
    }

    #[test]
    fn test_input_read() {
        const CONTENTS: &[u8] = b"input\0bytes";
        let fixture = output_paths_test_fixture();
        let path = fixture.path().join("index.scip");
        write_file_test_fixture(&path, CONTENTS);
        let system_under_test = InputFile::open(&path);
        let mut system_under_test = assert_ok!(system_under_test, "The input fixture must open for reading");

        let contents = system_under_test.read();
        let contents = assert_ok!(contents, "The retained input handle must read the fixture bytes");

        assert_eq!(contents, CONTENTS, "Input reading must preserve every byte");
    }

    #[test]
    fn test_retained_input_identity() {
        const CONTENTS_ORIGINAL: &[u8] = b"original input";
        const CONTENTS_REPLACEMENT: &[u8] = b"replacement input";
        let fixture = output_paths_test_fixture();
        let path = fixture.path().join("index.scip");
        let renamed = fixture.path().join("renamed.scip");
        write_file_test_fixture(&path, CONTENTS_ORIGINAL);
        let system_under_test = InputFile::open(&path);
        let mut system_under_test = assert_ok!(system_under_test, "The original input must open before renaming");
        let moved = fs::rename(&path, &renamed);
        assert_ok!(moved, "The opened input must be renamed while its handle is retained");
        write_file_test_fixture(&path, CONTENTS_REPLACEMENT);

        let contents = system_under_test.read();
        let contents = assert_ok!(contents, "The retained input handle must remain readable after path replacement");
        let output = OutputFiles::open(&[FileOutput::new(&renamed, b"overwrite")], &[&system_under_test]);
        let original = read_file_test_fixture(&renamed);
        let replacement = read_file_test_fixture(&path);

        assert_eq!(contents, CONTENTS_ORIGINAL, "Reading reopened the input path");
        let conflict = output.is_err_and(|error| error.is::<OutputPathConflict>());
        assert!(conflict, "The retained input object was not protected after renaming");
        assert_eq!(original, CONTENTS_ORIGINAL, "Conflict detection must preserve the renamed input");
        assert_eq!(replacement, CONTENTS_REPLACEMENT, "Conflict detection must preserve the replacement input");
    }

    #[rstest]
    #[case::missing(PathBuf::from("missing.scip"))]
    #[case::directory(PathBuf::new())]
    #[case::missing_parent(Path::new("missing").join("index.scip"))]
    #[case::invalid(PathBuf::from("\0"))]
    fn test_input_io_errors(#[case] name: PathBuf) {
        let fixture = output_paths_test_fixture();
        let path = fixture.path().join(name);

        let system_under_test = InputFile::open(&path).and_then(|mut input| input.read());

        let has_path_and_io_error = system_under_test.is_err_and(|error| {
            error.downcast_ref::<Error>().is_some() && error.to_string().contains(&path.display().to_string())
        });
        assert!(has_path_and_io_error, "Input failures must retain the path and underlying I/O error");
    }

    #[test]
    fn test_input_read_error() {
        let fixture = output_paths_test_fixture();
        let path = fixture.path().join("write-only.scip");
        let file = File::create(&path);
        let file = assert_ok!(file, "The write-only input fixture must be created");
        let handle = Handle::from_file(file);
        let handle = assert_ok!(handle, "The write-only input must have a retained identity handle");
        let mut system_under_test = InputFile { handle, path: path.clone() };

        let result = system_under_test.read();

        let has_path_and_io_error = result.is_err_and(|error| {
            error.downcast_ref::<Error>().is_some() && error.to_string().contains(&path.display().to_string())
        });
        assert!(has_path_and_io_error, "A failed handle read must retain its path and I/O error");
    }

    #[rstest]
    #[case::lexical(false)]
    #[case::hard_link(true)]
    fn test_output_aliases(#[case] hard_link: bool) {
        const CONTENTS_EXISTING: &[u8] = b"original";
        let fixture = output_paths_test_fixture();
        let earlier = fixture.path().join("diagram.md");
        let first = fixture.path().join("cycles.md");
        let second = if hard_link { fixture.path().join("alias.md") } else { fixture.path().join(".").join("cycles.md") };
        write_file_test_fixture(&earlier, CONTENTS_EXISTING);
        write_file_test_fixture(&first, CONTENTS_EXISTING);
        if hard_link {
            let linked = fs::hard_link(&first, &second);
            assert_ok!(linked, "The second output path must be a hard link to the first");
        }

        let outputs = [
            FileOutput::new(&earlier, b"new diagram"),
            FileOutput::new(&first, b"new cycles"),
            FileOutput::new(&second, b"new alias"),
        ];
        let system_under_test = OutputFiles::open(&outputs, &[]).and_then(OutputFiles::write);
        let earlier_contents = read_file_test_fixture(&earlier);
        let first_contents = read_file_test_fixture(&first);
        let second_contents = read_file_test_fixture(&second);

        let conflict = system_under_test.is_err_and(|error| error.is::<OutputPathConflict>());
        assert!(conflict, "Aliased outputs were accepted");
        assert_eq!(earlier_contents, CONTENTS_EXISTING, "An earlier output was written before validation completed");
        assert_eq!(first_contents, CONTENTS_EXISTING, "Alias rejection must preserve the first output");
        assert_eq!(second_contents, CONTENTS_EXISTING, "Alias rejection must preserve the second output");
    }

    #[rstest]
    #[case::lexical(false)]
    #[case::hard_link(true)]
    fn test_input_overwrite(#[case] hard_link: bool) {
        const CONTENTS_INPUT: &[u8] = b"protected input";
        const CONTENTS_OUTPUT_EXISTING: &[u8] = b"existing diagram";
        let fixture = output_paths_test_fixture();
        let input = fixture.path().join("index.scip");
        let earlier = fixture.path().join("diagram.md");
        let output = if hard_link { fixture.path().join("report.md") } else { fixture.path().join(".").join("index.scip") };
        write_file_test_fixture(&input, CONTENTS_INPUT);
        write_file_test_fixture(&earlier, CONTENTS_OUTPUT_EXISTING);
        if hard_link {
            let linked = fs::hard_link(&input, &output);
            assert_ok!(linked, "The output path must be a hard link to the protected input");
        }
        let opened_input = InputFile::open(&input);
        let opened_input = assert_ok!(opened_input, "The protected input must open before validating outputs");
        let inputs = [&opened_input];

        let outputs = [FileOutput::new(&earlier, b"new diagram"), FileOutput::new(&output, b"overwrite")];
        let system_under_test = OutputFiles::open(&outputs, &inputs).and_then(OutputFiles::write);
        let input_contents = read_file_test_fixture(&input);
        let earlier_contents = read_file_test_fixture(&earlier);

        let conflict = system_under_test.is_err_and(|error| error.is::<OutputPathConflict>());
        assert!(conflict, "An output could overwrite a protected input");
        assert_eq!(input_contents, CONTENTS_INPUT, "Conflict rejection must preserve the protected input");
        assert_eq!(earlier_contents, CONTENTS_OUTPUT_EXISTING, "Conflict rejection must preserve the earlier output");
    }

    #[test]
    fn test_duplicate_names_before_creation() {
        let fixture = output_paths_test_fixture();
        let earlier = fixture.path().join("diagram.md");
        let first = fixture.path().join("report.md");
        let second = fixture.path().join(".").join("report.md");

        let outputs = [
            FileOutput::new(&earlier, b"diagram"),
            FileOutput::new(&first, b"first"),
            FileOutput::new(&second, b"second"),
        ];
        let system_under_test = OutputFiles::open(&outputs, &[]);
        let earlier_contents = fs::read(&earlier);
        let report_contents = fs::read(&first);

        let conflict = system_under_test.is_err_and(|error| error.is::<OutputPathConflict>());
        assert!(conflict, "Duplicate lexical output names were accepted");
        let earlier_missing = earlier_contents.is_err_and(|error| error.kind() == ErrorKind::NotFound);
        assert!(earlier_missing, "Lexical conflict detection created an earlier output");
        let report_missing = report_contents.is_err_and(|error| error.kind() == ErrorKind::NotFound);
        assert!(report_missing, "Lexical conflict detection created a conflicting output");
    }

    #[test]
    fn test_protected_input_name_before_creation() {
        const CONTENTS: &[u8] = b"protected input";
        let fixture = output_paths_test_fixture();
        let input = fixture.path().join("index.scip");
        let renamed = fixture.path().join("renamed.scip");
        let earlier = fixture.path().join("diagram.md");
        write_file_test_fixture(&input, CONTENTS);
        let opened_input = InputFile::open(&input);
        let opened_input = assert_ok!(opened_input, "The protected input must open before its path is renamed");
        let inputs = [&opened_input];
        let moved = fs::rename(&input, &renamed);
        assert_ok!(moved, "The input must be renamed while its retained handle protects its original name");

        let outputs = [FileOutput::new(&earlier, b"diagram"), FileOutput::new(&input, b"overwrite")];
        let system_under_test = OutputFiles::open(&outputs, &inputs);
        let earlier_contents = fs::read(&earlier);
        let input_contents = fs::read(&input);
        let renamed_contents = read_file_test_fixture(&renamed);

        let conflict = system_under_test.is_err_and(|error| error.is::<OutputPathConflict>());
        assert!(conflict, "The lexical input name was not protected after renaming");
        let earlier_missing = earlier_contents.is_err_and(|error| error.kind() == ErrorKind::NotFound);
        assert!(earlier_missing, "Protecting the input name must not create an earlier output");
        let input_missing = input_contents.is_err_and(|error| error.kind() == ErrorKind::NotFound);
        assert!(input_missing, "Protecting the input name must not recreate the original path");
        assert_eq!(renamed_contents, CONTENTS, "Protecting the input name must preserve the renamed input");
    }

    #[rstest]
    #[case::case_variant("report.md", "REPORT.md")]
    #[case::distinct("folders.md", "files.md")]
    fn test_new_output_identity(#[case] first_name: &str, #[case] second_name: &str) {
        let fixture = output_paths_test_fixture();
        let first = fixture.path().join(first_name);
        let second = fixture.path().join(second_name);

        let outputs = [FileOutput::new(&first, b"first"), FileOutput::new(&second, b"second")];
        let system_under_test = OutputFiles::open(&outputs, &[]);
        let first_file = File::open(&first);
        let first_file = assert_ok!(first_file, "The first output must be created during opening");
        let first_handle = Handle::from_file(first_file);
        let first_handle = assert_ok!(first_handle, "The first output must have a readable identity handle");
        let second_file = File::open(&second);
        let second_file = assert_ok!(second_file, "The second output path must be readable after opening");
        let second_handle = Handle::from_file(second_file);
        let second_handle = assert_ok!(second_handle, "The second output must have a readable identity handle");
        let first_contents = read_file_test_fixture(&first);
        let second_contents = read_file_test_fixture(&second);

        let aliases = first_handle == second_handle;
        if aliases {
            let conflict = system_under_test.is_err_and(|error| error.is::<OutputPathConflict>());
            assert!(conflict, "Aliased file identities must fail with an output path conflict");
        } else {
            assert_ok!(system_under_test, "Distinct file identities must open successfully");
        }
        assert!(first_contents.is_empty(), "Opening must not write the first new output");
        assert!(second_contents.is_empty(), "Opening must not write the second new output");
    }

    #[rstest]
    #[case::directory(PathBuf::new())]
    #[case::missing_parent(Path::new("missing").join("report.md"))]
    #[case::file_parent(Path::new("existing.md").join("report.md"))]
    fn test_output_open_failure(#[case] name: PathBuf) {
        const CONTENTS: &[u8] = b"existing output";
        let fixture = output_paths_test_fixture();
        let existing = fixture.path().join("existing.md");
        let created = fixture.path().join("created.md");
        let invalid = fixture.path().join(name);
        write_file_test_fixture(&existing, CONTENTS);

        let outputs = [
            FileOutput::new(&existing, b"overwrite"),
            FileOutput::new(&created, b"new output"),
            FileOutput::new(&invalid, b"invalid output"),
        ];
        let system_under_test = OutputFiles::open(&outputs, &[]);
        let existing_contents = read_file_test_fixture(&existing);
        let created_contents = read_file_test_fixture(&created);

        let error = assert_some!(system_under_test.err(), "Opening the invalid output must fail");
        let output_error = error.downcast_ref::<OutputIoError>();
        let output_error = assert_some!(output_error, "Output opening must retain its operation and path: {error:#}");
        let opens_invalid_path = matches!(*output_error, OutputIoError::Open(ref path) if path == &invalid);
        assert!(opens_invalid_path, "Opening must fail at the invalid output path: {error:#}");
        assert!(error.downcast_ref::<Error>().is_some(), "Opening failures must retain the original I/O error: {error:#}");
        assert_eq!(existing_contents, CONTENTS, "An opening failure truncated an existing output");
        assert!(created_contents.is_empty(), "Opening must not write newly created outputs");
    }

    #[rstest]
    #[case::empty(Path::new(""))]
    #[case::invalid(Path::new("\0"))]
    fn test_invalid_output_path(#[case] path: &Path) {
        let system_under_test = OutputFiles::open(&[FileOutput::new(path, b"output")], &[]);

        let has_io_error = system_under_test.is_err_and(|error| error.downcast_ref::<Error>().is_some());
        assert!(has_io_error, "An invalid path must fail with an ordinary I/O error");
    }

    #[rstest]
    #[case::replacement(b"new")]
    #[case::empty(b"")]
    fn test_output_writes(#[case] contents: &[u8], #[values(false, true)] existing: bool, #[values(false, true)] selected: bool) {
        const CONTENTS_ORIGINAL: &[u8] = b"old contents longer than the replacement";
        const CONTENTS_SECOND: &[u8] = b"second output";
        let fixture = output_paths_test_fixture();
        let first = fixture.path().join("first.md");
        let second = fixture.path().join("second.md");
        if existing {
            write_file_test_fixture(&first, CONTENTS_ORIGINAL);
            write_file_test_fixture(&second, CONTENTS_ORIGINAL);
        }

        let initial_contents = if selected { b"reserved" } else { contents };
        let outputs = [FileOutput::new(&first, initial_contents), FileOutput::new(&second, CONTENTS_SECOND)];
        let system_under_test = OutputFiles::open(&outputs, &[]);
        let mut system_under_test = assert_ok!(system_under_test, "Opening distinct output paths failed");
        let first_before = read_file_test_fixture(&first);
        let second_before = read_file_test_fixture(&second);
        for file in &mut system_under_test.files {
            let position = file.handle.as_file_mut().seek(SeekFrom::End(0));
            assert_ok!(position, "Failed to move an output handle to EOF for the write test");
        }
        if selected {
            let first_write = system_under_test.write_output(&FileOutput::new(&first, contents));
            assert_ok!(first_write, "Writing the selected output failed");
            let second_reserved = read_file_test_fixture(&second);
            assert_eq!(second_reserved, second_before, "Writing the first output changed the second reserved output");
            let second_write = system_under_test.write_output(&FileOutput::new(&second, CONTENTS_SECOND));
            assert_ok!(second_write, "Writing the second reserved output failed");
        } else {
            let plan_write = system_under_test.write();
            assert_ok!(plan_write, "Writing the opened output plan failed");
        }
        let first_after = read_file_test_fixture(&first);
        let second_after = read_file_test_fixture(&second);

        let before = if existing { CONTENTS_ORIGINAL } else { b"" };
        assert_eq!(first_before, before, "Opening changed the first output's contents");
        assert_eq!(second_before, before, "Opening changed the second output's contents");
        assert_eq!(first_after, contents, "The first output contains incorrect bytes after writing");
        assert_eq!(second_after, CONTENTS_SECOND, "The second output differs from its planned contents");
    }

    #[rstest]
    fn test_output_path_replacement(#[values(false, true)] existing: bool, #[values(false, true)] selected: bool) {
        const CONTENTS_INPUT: &[u8] = b"protected input";
        const CONTENTS_OUTPUT: &[u8] = b"new report";
        let fixture = output_paths_test_fixture();
        let input = fixture.path().join("index.scip");
        let output = fixture.path().join("report.md");
        let renamed = fixture.path().join("renamed.md");
        write_file_test_fixture(&input, CONTENTS_INPUT);
        if existing {
            write_file_test_fixture(&output, b"original report contents");
        }
        let opened_input = InputFile::open(&input);
        let opened_input = assert_ok!(opened_input, "Failed to open the protected input fixture");
        let inputs = [&opened_input];
        let system_under_test = OutputFiles::open(&[FileOutput::new(&output, CONTENTS_OUTPUT)], &inputs);
        let mut system_under_test = assert_ok!(system_under_test, "Failed to open the output before replacing its path");

        let moved = fs::rename(&output, &renamed);
        assert_ok!(moved, "Failed to rename the output while its handle was open");
        let linked = fs::hard_link(&input, &output);
        assert_ok!(linked, "Failed to replace the output path with a hard link to the input");
        let output_write = if selected {
            system_under_test.write_output(&FileOutput::new(&output, CONTENTS_OUTPUT))
        } else {
            system_under_test.write()
        };
        assert_ok!(output_write, "Writing the retained output failed after replacing its path");
        let input_contents = read_file_test_fixture(&input);
        let output_contents = read_file_test_fixture(&output);
        let renamed_contents = read_file_test_fixture(&renamed);

        assert_eq!(input_contents, CONTENTS_INPUT, "Writing modified the protected input's contents");
        assert_eq!(output_contents, CONTENTS_INPUT, "Replacement hard link contents changed after writing");
        assert_eq!(renamed_contents, CONTENTS_OUTPUT, "The renamed output differs from the requested contents");
    }

    #[test]
    fn test_output_truncation_failure() {
        const CONTENTS: &[u8] = b"read-only handle";
        let fixture = output_paths_test_fixture();
        let path = fixture.path().join("report.md");
        write_file_test_fixture(&path, CONTENTS);
        let file = File::open(&path);
        let file = assert_ok!(file, "The output fixture must open read-only");
        let handle = Handle::from_file(file);
        let handle = assert_ok!(handle, "The read-only output must have a retained identity handle");
        let system_under_test = OutputFiles {
            files: vec![OpenedOutput { output: FileOutput::new(&path, b"overwrite"), handle }],
        };

        let result = system_under_test.write();
        let contents = read_file_test_fixture(&path);

        let error = assert_some!(result.err(), "A read-only output handle must fail during truncation");
        let output_error = error.downcast_ref::<OutputIoError>();
        let output_error = assert_some!(output_error, "Output truncation must retain its operation and path: {error:#}");
        let truncates_path = matches!(*output_error, OutputIoError::Truncate(ref failed_path) if failed_path == &path);
        assert!(truncates_path, "Truncation must fail on the retained read-only output handle: {error:#}");
        assert!(error.downcast_ref::<Error>().is_some(), "Truncation failures must retain their I/O error: {error:#}");
        assert_eq!(contents, CONTENTS, "A failed handle truncation must preserve the output contents");
    }

    #[rstest]
    fn test_unplanned_output_write(#[values(false, true)] existing: bool) {
        const CONTENTS_EXISTING: &[u8] = b"existing unplanned output";
        let fixture = output_paths_test_fixture();
        let planned = fixture.path().join("planned.md");
        let unplanned = fixture.path().join("unplanned.md");
        if existing {
            write_file_test_fixture(&unplanned, CONTENTS_EXISTING);
        }
        let opened = OutputFiles::open(&[FileOutput::new(&planned, b"planned")], &[]);
        let mut system_under_test = assert_ok!(opened, "Failed to reserve the planned output");

        let result = system_under_test.write_output(&FileOutput::new(&unplanned, b"unvalidated"));
        let planned_contents = read_file_test_fixture(&planned);
        let unplanned_contents = fs::read(&unplanned);

        let error = assert_some!(result.err(), "Writing an unplanned output must return an error");
        let unplanned_path = unplanned.display().to_string();
        assert_contains!(error.to_string(), &unplanned_path, "The error must identify the unopened output path: {error:#}");
        assert!(planned_contents.is_empty(), "An unplanned write changed the reserved output");
        if existing {
            let contents = assert_ok!(unplanned_contents, "The unplanned output must remain readable after rejection");
            assert_eq!(contents, CONTENTS_EXISTING, "Rejecting an unplanned output must preserve its contents");
        } else {
            let missing = unplanned_contents.is_err_and(|error| error.kind() == ErrorKind::NotFound);
            assert!(missing, "Rejecting an unplanned output must not create it");
        }
    }

    #[test]
    fn test_empty_output_plan() {
        let system_under_test = OutputFiles::open(&[], &[]);
        let system_under_test = assert_ok!(system_under_test, "An empty output plan must open successfully");

        let written = system_under_test.write();
        assert_ok!(written, "Writing an empty output plan must succeed without creating files");
    }
}
