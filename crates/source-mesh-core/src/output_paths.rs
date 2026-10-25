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
use same_file::Handle;

const OUTPUT_PATH_CONFLICT: &str = "Output files must use distinct paths and must not overwrite input files";

pub(crate) struct InputFile {
    path: PathBuf,
    handle: Handle,
}

impl InputFile {
    pub(crate) fn open(path: &Path) -> Result<Self> {
        let path = absolute(path).with_context(|| format!("Cannot resolve input path {}", path.display()))?;
        let file = File::open(&path).with_context(|| format!("Cannot open input file {}", path.display()))?;
        let handle = Handle::from_file(file).with_context(|| format!("Cannot identify input file {}", path.display()))?;
        Ok(Self { path, handle })
    }

    pub(crate) fn read(&mut self) -> Result<Vec<u8>> {
        let mut contents = Vec::new();
        let read_result = self.handle.as_file_mut().read_to_end(&mut contents);
        read_result.with_context(|| format!("Cannot read input file {}", self.path.display()))?;
        Ok(contents)
    }
}

#[derive(new)]
pub(crate) struct FileOutput<'a> {
    path: &'a Path,
    contents: &'a [u8],
}

struct OpenedOutput<'a> {
    output: FileOutput<'a>,
    handle: Handle,
}

pub(crate) struct OutputFiles<'a> {
    files: Vec<OpenedOutput<'a>>,
}

impl<'a> OutputFiles<'a> {
    /**
    Newly created empty files may remain on failure: removing their paths could delete replacement
    files, so no path-based rollback is attempted.
    */
    pub(crate) fn open(outputs: &[FileOutput<'a>], inputs: &[InputFile]) -> Result<Self> {
        let paths = outputs.iter().map(|output| {
            absolute(output.path).with_context(|| format!("Cannot resolve output path {}", output.path.display()))
        }).collect::<Result<Vec<_>>>()?;
        for (index, path) in paths.iter().enumerate() {
            for other in paths.iter().take(index).chain(inputs.iter().map(|input| &input.path)) {
                ensure!(path != other, "{OUTPUT_PATH_CONFLICT}: {} and {}", path.display(), other.display());
            }
        }

        let mut files: Vec<OpenedOutput<'a>> = Vec::with_capacity(outputs.len());
        for (output, path) in outputs.iter().zip(paths) {
            let file = OpenOptions::new().write(true).create(true).truncate(false).open(&path);
            let file = file.with_context(|| format!("Cannot open output file {}", output.path.display()))?;
            let handle = Handle::from_file(file);
            let handle = handle.with_context(|| format!("Cannot identify output file {}", output.path.display()))?;
            let output_identities = files.iter().map(|opened| (&opened.handle, opened.output.path));
            let input_identities = inputs.iter().map(|input| (&input.handle, input.path.as_path()));
            for (other_handle, other_path) in output_identities.chain(input_identities) {
                let distinct = &handle != other_handle;
                ensure!(distinct, "{OUTPUT_PATH_CONFLICT}: {} and {}", output.path.display(), other_path.display());
            }
            files.push(OpenedOutput { output: FileOutput::new(output.path, output.contents), handle });
        }
        Ok(Self { files })
    }

    pub(crate) fn write(self) -> Result<()> {
        for mut file in self.files {
            let output = file.handle.as_file_mut();
            output.set_len(0).with_context(|| format!("Cannot truncate output file {}", file.output.path.display()))?;
            output.rewind().with_context(|| format!("Cannot seek output file {}", file.output.path.display()))?;
            let write_result = output.write_all(file.output.contents);
            write_result.with_context(|| format!("Cannot write output file {}", file.output.path.display()))?;
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

    use claims::assert_ok;
    use pretty_assertions::assert_eq;
    use rstest::rstest;
    use same_file::Handle;
    use tempfile::tempdir;

    use super::FileOutput;
    use super::InputFile;
    use super::OUTPUT_PATH_CONFLICT;
    use super::OpenedOutput;
    use super::OutputFiles;

    #[test]
    fn test_input_read() {
        const CONTENTS: &[u8] = b"input\0bytes";
        let fixture = tempdir();
        let fixture = assert_ok!(fixture, "The input fixture directory must be created");
        let path = fixture.path().join("index.scip");
        let written = fs::write(&path, CONTENTS);
        assert_ok!(written, "The input fixture bytes must be written");
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
        let fixture = tempdir();
        let fixture = assert_ok!(fixture, "The input identity fixture directory must be created");
        let path = fixture.path().join("index.scip");
        let renamed = fixture.path().join("renamed.scip");
        let original_write = fs::write(&path, CONTENTS_ORIGINAL);
        assert_ok!(original_write, "The original input bytes must be written");
        let system_under_test = InputFile::open(&path);
        let mut system_under_test = assert_ok!(system_under_test, "The original input must open before renaming");
        let moved = fs::rename(&path, &renamed);
        assert_ok!(moved, "The opened input must be renamed while its handle is retained");
        let replacement_write = fs::write(&path, CONTENTS_REPLACEMENT);
        assert_ok!(replacement_write, "Replacement bytes must be written at the original input path");

        let contents = system_under_test.read();
        let contents = assert_ok!(contents, "The retained input handle must remain readable after path replacement");
        let output = OutputFiles::open(&[FileOutput::new(&renamed, b"overwrite")], &[system_under_test]);
        let original = fs::read(&renamed);
        let original = assert_ok!(original, "The renamed original input must remain readable");
        let replacement = fs::read(&path);
        let replacement = assert_ok!(replacement, "The replacement input must remain readable");

        assert_eq!(contents, CONTENTS_ORIGINAL, "Reading reopened the input path");
        let conflict = output.is_err_and(|error| error.to_string().starts_with(OUTPUT_PATH_CONFLICT));
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
        let fixture = tempdir();
        let fixture = assert_ok!(fixture, "The input I/O failure fixture directory must be created");
        let path = fixture.path().join(name);

        let system_under_test = InputFile::open(&path).and_then(|mut input| input.read());

        let has_path_and_io_error = system_under_test.is_err_and(|error| {
            error.downcast_ref::<Error>().is_some() && error.to_string().contains(&path.display().to_string())
        });
        assert!(has_path_and_io_error, "Input failures must retain the path and underlying I/O error");
    }

    #[test]
    fn test_input_read_error() {
        let fixture = tempdir();
        let fixture = assert_ok!(fixture, "The input read failure fixture directory must be created");
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
        let fixture = tempdir();
        let fixture = assert_ok!(fixture, "The output alias fixture directory must be created");
        let earlier = fixture.path().join("diagram.md");
        let first = fixture.path().join("cycles.md");
        let second = if hard_link { fixture.path().join("alias.md") } else { fixture.path().join(".").join("cycles.md") };
        let earlier_write = fs::write(&earlier, CONTENTS_EXISTING);
        assert_ok!(earlier_write, "The earlier output fixture must be written");
        let aliased_write = fs::write(&first, CONTENTS_EXISTING);
        assert_ok!(aliased_write, "The aliased output fixture must be written");
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
        let earlier_contents = fs::read(&earlier);
        let earlier_contents = assert_ok!(earlier_contents, "The earlier output must remain readable after alias rejection");
        let first_contents = fs::read(&first);
        let first_contents = assert_ok!(first_contents, "The first aliased output must remain readable");
        let second_contents = fs::read(&second);
        let second_contents = assert_ok!(second_contents, "The second aliased output must remain readable");

        let conflict = system_under_test.is_err_and(|error| error.to_string().starts_with(OUTPUT_PATH_CONFLICT));
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
        let fixture = tempdir();
        let fixture = assert_ok!(fixture, "The input protection fixture directory must be created");
        let input = fixture.path().join("index.scip");
        let earlier = fixture.path().join("diagram.md");
        let output = if hard_link { fixture.path().join("report.md") } else { fixture.path().join(".").join("index.scip") };
        let input_write = fs::write(&input, CONTENTS_INPUT);
        assert_ok!(input_write, "The protected input fixture must be written");
        let earlier_write = fs::write(&earlier, CONTENTS_OUTPUT_EXISTING);
        assert_ok!(earlier_write, "The earlier output fixture must be written");
        if hard_link {
            let linked = fs::hard_link(&input, &output);
            assert_ok!(linked, "The output path must be a hard link to the protected input");
        }
        let opened_input = InputFile::open(&input);
        let opened_input = assert_ok!(opened_input, "The protected input must open before validating outputs");
        let inputs = [opened_input];

        let outputs = [FileOutput::new(&earlier, b"new diagram"), FileOutput::new(&output, b"overwrite")];
        let system_under_test = OutputFiles::open(&outputs, &inputs).and_then(OutputFiles::write);
        let input_contents = fs::read(&input);
        let input_contents = assert_ok!(input_contents, "The protected input must remain readable after conflict rejection");
        let earlier_contents = fs::read(&earlier);
        let earlier_contents = assert_ok!(earlier_contents, "The earlier output must remain readable after conflict rejection");

        let conflict = system_under_test.is_err_and(|error| error.to_string().starts_with(OUTPUT_PATH_CONFLICT));
        assert!(conflict, "An output could overwrite a protected input");
        assert_eq!(input_contents, CONTENTS_INPUT, "Conflict rejection must preserve the protected input");
        assert_eq!(earlier_contents, CONTENTS_OUTPUT_EXISTING, "Conflict rejection must preserve the earlier output");
    }

    #[test]
    fn test_duplicate_names_before_creation() {
        let fixture = tempdir();
        let fixture = assert_ok!(fixture, "The duplicate output name fixture directory must be created");
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

        let conflict = system_under_test.is_err_and(|error| error.to_string().starts_with(OUTPUT_PATH_CONFLICT));
        assert!(conflict, "Duplicate lexical output names were accepted");
        let earlier_missing = earlier_contents.is_err_and(|error| error.kind() == ErrorKind::NotFound);
        assert!(earlier_missing, "Lexical conflict detection created an earlier output");
        let report_missing = report_contents.is_err_and(|error| error.kind() == ErrorKind::NotFound);
        assert!(report_missing, "Lexical conflict detection created a conflicting output");
    }

    #[test]
    fn test_protected_input_name_before_creation() {
        const CONTENTS: &[u8] = b"protected input";
        let fixture = tempdir();
        let fixture = assert_ok!(fixture, "The protected input name fixture directory must be created");
        let input = fixture.path().join("index.scip");
        let renamed = fixture.path().join("renamed.scip");
        let earlier = fixture.path().join("diagram.md");
        let written = fs::write(&input, CONTENTS);
        assert_ok!(written, "The protected input fixture must be written");
        let opened_input = InputFile::open(&input);
        let opened_input = assert_ok!(opened_input, "The protected input must open before its path is renamed");
        let inputs = [opened_input];
        let moved = fs::rename(&input, &renamed);
        assert_ok!(moved, "The input must be renamed while its retained handle protects its original name");

        let outputs = [FileOutput::new(&earlier, b"diagram"), FileOutput::new(&input, b"overwrite")];
        let system_under_test = OutputFiles::open(&outputs, &inputs);
        let earlier_contents = fs::read(&earlier);
        let input_contents = fs::read(&input);
        let renamed_contents = fs::read(&renamed);
        let renamed_contents = assert_ok!(renamed_contents, "The renamed protected input must remain readable");

        let conflict = system_under_test.is_err_and(|error| error.to_string().starts_with(OUTPUT_PATH_CONFLICT));
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
        let fixture = tempdir();
        let fixture = assert_ok!(fixture, "The new output identity fixture directory must be created");
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
        let first_contents = fs::read(&first);
        let first_contents = assert_ok!(first_contents, "The first new output must remain readable");
        let second_contents = fs::read(&second);
        let second_contents = assert_ok!(second_contents, "The second new output must remain readable");

        let conflict = system_under_test.is_err();
        let aliases = first_handle == second_handle;
        assert_eq!(conflict, aliases, "Case handling must follow actual file identity, not an operating-system assumption");
        assert!(first_contents.is_empty(), "Opening must not write the first new output");
        assert!(second_contents.is_empty(), "Opening must not write the second new output");
    }

    #[rstest]
    #[case::directory(PathBuf::new())]
    #[case::missing_parent(Path::new("missing").join("report.md"))]
    #[case::file_parent(Path::new("existing.md").join("report.md"))]
    fn test_output_open_failure(#[case] name: PathBuf) {
        const CONTENTS: &[u8] = b"existing output";
        let fixture = tempdir();
        let fixture = assert_ok!(fixture, "The output opening failure fixture directory must be created");
        let existing = fixture.path().join("existing.md");
        let created = fixture.path().join("created.md");
        let invalid = fixture.path().join(name);
        let written = fs::write(&existing, CONTENTS);
        assert_ok!(written, "The existing output fixture must be written before opening the output plan");

        let outputs = [
            FileOutput::new(&existing, b"overwrite"),
            FileOutput::new(&created, b"new output"),
            FileOutput::new(&invalid, b"invalid output"),
        ];
        let system_under_test = OutputFiles::open(&outputs, &[]).and_then(OutputFiles::write);
        let existing_contents = fs::read(&existing);
        let existing_contents = assert_ok!(existing_contents, "The existing output must remain readable after opening fails");
        let created_contents = fs::read(&created);
        let created_contents = assert_ok!(created_contents, "The newly created output must remain after opening fails");

        let has_path_and_io_error = system_under_test.is_err_and(|error| {
            error.downcast_ref::<Error>().is_some() && error.to_string().contains(&invalid.display().to_string())
        });
        assert!(has_path_and_io_error, "Output-opening failures must retain the path and underlying I/O error");
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
    fn test_output_writes(#[case] contents: &[u8], #[values(false, true)] existing: bool) {
        const CONTENTS_ORIGINAL: &[u8] = b"old contents longer than the replacement";
        const CONTENTS_SECOND: &[u8] = b"second output";
        let fixture = tempdir();
        let fixture = assert_ok!(fixture, "The output writing fixture directory must be created");
        let first = fixture.path().join("first.md");
        let second = fixture.path().join("second.md");
        if existing {
            let first_write = fs::write(&first, CONTENTS_ORIGINAL);
            assert_ok!(first_write, "The first existing output fixture must be written");
            let second_write = fs::write(&second, CONTENTS_ORIGINAL);
            assert_ok!(second_write, "The second existing output fixture must be written");
        }

        let outputs = [FileOutput::new(&first, contents), FileOutput::new(&second, CONTENTS_SECOND)];
        let system_under_test = OutputFiles::open(&outputs, &[]);
        let mut system_under_test = assert_ok!(system_under_test, "Distinct output paths must open without truncation");
        let first_before = fs::read(&first);
        let first_before = assert_ok!(first_before, "The first output must be readable before writing");
        let second_before = fs::read(&second);
        let second_before = assert_ok!(second_before, "The second output must be readable before writing");
        for file in &mut system_under_test.files {
            let position = file.handle.as_file_mut().seek(SeekFrom::End(0));
            assert_ok!(position, "The output handle must seek to its end before testing rewind and truncation");
        }
        let plan_write = system_under_test.write();
        assert_ok!(plan_write, "The output plan must write through its retained handles");
        let first_after = fs::read(&first);
        let first_after = assert_ok!(first_after, "The first output must be readable after writing");
        let second_after = fs::read(&second);
        let second_after = assert_ok!(second_after, "The second output must be readable after writing");

        let before = if existing { CONTENTS_ORIGINAL } else { b"" };
        assert_eq!(first_before, before, "Opening truncated an output");
        assert_eq!(second_before, before, "Opening truncated an output");
        assert_eq!(first_after, contents, "Writing must truncate and seek to the start");
        assert_eq!(second_after, CONTENTS_SECOND, "The second output must receive its planned contents");
    }

    #[rstest]
    fn test_output_path_replacement(#[values(false, true)] existing: bool) {
        const CONTENTS_INPUT: &[u8] = b"protected input";
        const CONTENTS_OUTPUT: &[u8] = b"new report";
        let fixture = tempdir();
        let fixture = assert_ok!(fixture, "The output path replacement fixture directory must be created");
        let input = fixture.path().join("index.scip");
        let output = fixture.path().join("report.md");
        let renamed = fixture.path().join("renamed.md");
        let input_write = fs::write(&input, CONTENTS_INPUT);
        assert_ok!(input_write, "The protected input fixture must be written");
        if existing {
            let existing_write = fs::write(&output, b"original report contents");
            assert_ok!(existing_write, "The existing output fixture must be written before path replacement");
        }
        let opened_input = InputFile::open(&input);
        let opened_input = assert_ok!(opened_input, "The protected input must open before the output plan");
        let inputs = [opened_input];
        let system_under_test = OutputFiles::open(&[FileOutput::new(&output, CONTENTS_OUTPUT)], &inputs);
        let system_under_test = assert_ok!(system_under_test, "The distinct output must open before path replacement");

        let moved = fs::rename(&output, &renamed);
        assert_ok!(moved, "The opened output must be renamed while its handle is retained");
        let linked = fs::hard_link(&input, &output);
        assert_ok!(linked, "The output path must be replaced with a hard link to the protected input");
        let output_write = system_under_test.write();
        assert_ok!(output_write, "Writing must use the retained output handle after path replacement");
        let input_contents = fs::read(&input);
        let input_contents = assert_ok!(input_contents, "The protected input must remain readable after writing");
        let output_contents = fs::read(&output);
        let output_contents = assert_ok!(output_contents, "The replacement output path must remain readable");
        let renamed_contents = fs::read(&renamed);
        let renamed_contents = assert_ok!(renamed_contents, "The renamed output must remain readable after writing");

        assert_eq!(input_contents, CONTENTS_INPUT, "Writing followed a replacement path into the protected input");
        assert_eq!(output_contents, CONTENTS_INPUT, "Writing must not modify the replacement hard link to the input");
        assert_eq!(renamed_contents, CONTENTS_OUTPUT, "Writing did not target the original opened output");
    }

    #[test]
    fn test_output_write_error() {
        const CONTENTS: &[u8] = b"read-only handle";
        let fixture = tempdir();
        let fixture = assert_ok!(fixture, "The output write failure fixture directory must be created");
        let path = fixture.path().join("report.md");
        let written = fs::write(&path, CONTENTS);
        assert_ok!(written, "The output fixture must be written before opening a read-only handle");
        let file = File::open(&path);
        let file = assert_ok!(file, "The output fixture must open read-only");
        let handle = Handle::from_file(file);
        let handle = assert_ok!(handle, "The read-only output must have a retained identity handle");
        let system_under_test = OutputFiles {
            files: vec![OpenedOutput { output: FileOutput::new(&path, b"overwrite"), handle }],
        };

        let result = system_under_test.write();
        let contents = fs::read(&path);
        let contents = assert_ok!(contents, "The output fixture must remain readable after the handle write fails");

        let has_path_and_io_error = result.is_err_and(|error| {
            error.downcast_ref::<Error>().is_some() && error.to_string().contains(&path.display().to_string())
        });
        assert!(has_path_and_io_error, "A failed handle write must retain its path and I/O error rather than reopening the path");
        assert_eq!(contents, CONTENTS, "A failed handle write must preserve the output contents");
    }

    #[test]
    fn test_empty_output_plan() {
        let system_under_test = OutputFiles::open(&[], &[]);
        let system_under_test = assert_ok!(system_under_test, "An empty output plan must open successfully");

        let written = system_under_test.write();
        assert_ok!(written, "Writing an empty output plan must succeed without creating files");
    }
}
