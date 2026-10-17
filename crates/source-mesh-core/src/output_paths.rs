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
#[expect(clippy::panic_in_result_fn, reason = "Tests use assertions for expectations while returning Result for fallible setup.")]
mod tests {
    use std::fs;
    use std::fs::File;
    use std::io::Error;
    use std::io::ErrorKind;
    use std::io::Seek as _;
    use std::io::SeekFrom;
    use std::path::Path;
    use std::path::PathBuf;

    use anyhow::Result;
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
    fn test_input_read() -> Result<()> {
        const CONTENTS: &[u8] = b"input\0bytes";
        let fixture = tempdir()?;
        let path = fixture.path().join("index.scip");
        fs::write(&path, CONTENTS)?;
        let mut system_under_test = InputFile::open(&path)?;

        let contents = system_under_test.read()?;

        assert_eq!(contents, CONTENTS);
        Ok(())
    }

    #[test]
    fn test_retained_input_identity() -> Result<()> {
        const ORIGINAL: &[u8] = b"original input";
        const REPLACEMENT: &[u8] = b"replacement input";
        let fixture = tempdir()?;
        let path = fixture.path().join("index.scip");
        let renamed = fixture.path().join("renamed.scip");
        fs::write(&path, ORIGINAL)?;
        let mut system_under_test = InputFile::open(&path)?;
        fs::rename(&path, &renamed)?;
        fs::write(&path, REPLACEMENT)?;

        let contents = system_under_test.read()?;
        let output = OutputFiles::open(&[FileOutput::new(&renamed, b"overwrite")], &[system_under_test]);
        let original = fs::read(&renamed)?;
        let replacement = fs::read(&path)?;

        assert_eq!(contents, ORIGINAL, "Reading reopened the input path");
        let conflict = output.is_err_and(|error| error.to_string().starts_with(OUTPUT_PATH_CONFLICT));
        assert!(conflict, "The retained input object was not protected after renaming");
        assert_eq!(original, ORIGINAL);
        assert_eq!(replacement, REPLACEMENT);
        Ok(())
    }

    #[rstest]
    #[case::missing(PathBuf::from("missing.scip"))]
    #[case::directory(PathBuf::new())]
    #[case::missing_parent(Path::new("missing").join("index.scip"))]
    #[case::invalid(PathBuf::from("\0"))]
    fn test_input_io_errors(#[case] name: PathBuf) -> Result<()> {
        let fixture = tempdir()?;
        let path = fixture.path().join(name);

        let system_under_test = InputFile::open(&path).and_then(|mut input| input.read());

        let has_path_and_io_error = system_under_test.is_err_and(|error| {
            error.downcast_ref::<Error>().is_some() && error.to_string().contains(&path.display().to_string())
        });
        assert!(has_path_and_io_error, "Input failures must retain the path and underlying I/O error");
        Ok(())
    }

    #[test]
    fn test_input_read_error() -> Result<()> {
        let fixture = tempdir()?;
        let path = fixture.path().join("write-only.scip");
        let mut system_under_test = InputFile { handle: Handle::from_file(File::create(&path)?)?, path: path.clone() };

        let result = system_under_test.read();

        let has_path_and_io_error = result.is_err_and(|error| {
            error.downcast_ref::<Error>().is_some() && error.to_string().contains(&path.display().to_string())
        });
        assert!(has_path_and_io_error, "A failed handle read must retain its path and I/O error");
        Ok(())
    }

    #[rstest]
    #[case::lexical(false)]
    #[case::hard_link(true)]
    fn test_output_aliases(#[case] hard_link: bool) -> Result<()> {
        const EXISTING_CONTENT: &[u8] = b"original";
        let fixture = tempdir()?;
        let earlier = fixture.path().join("diagram.md");
        let first = fixture.path().join("cycles.md");
        let second = if hard_link { fixture.path().join("alias.md") } else { fixture.path().join(".").join("cycles.md") };
        fs::write(&earlier, EXISTING_CONTENT)?;
        fs::write(&first, EXISTING_CONTENT)?;
        if hard_link {
            fs::hard_link(&first, &second)?;
        }

        let outputs = [
            FileOutput::new(&earlier, b"new diagram"),
            FileOutput::new(&first, b"new cycles"),
            FileOutput::new(&second, b"new alias"),
        ];
        let system_under_test = OutputFiles::open(&outputs, &[]).and_then(OutputFiles::write);
        let earlier_contents = fs::read(&earlier)?;
        let first_contents = fs::read(&first)?;
        let second_contents = fs::read(&second)?;

        let conflict = system_under_test.is_err_and(|error| error.to_string().starts_with(OUTPUT_PATH_CONFLICT));
        assert!(conflict, "Aliased outputs were accepted");
        assert_eq!(earlier_contents, EXISTING_CONTENT, "An earlier output was written before validation completed");
        assert_eq!(first_contents, EXISTING_CONTENT);
        assert_eq!(second_contents, EXISTING_CONTENT);
        Ok(())
    }

    #[rstest]
    #[case::lexical(false)]
    #[case::hard_link(true)]
    fn test_input_overwrite(#[case] hard_link: bool) -> Result<()> {
        const INPUT: &[u8] = b"protected input";
        const EXISTING_OUTPUT: &[u8] = b"existing diagram";
        let fixture = tempdir()?;
        let input = fixture.path().join("index.scip");
        let earlier = fixture.path().join("diagram.md");
        let output = if hard_link { fixture.path().join("report.md") } else { fixture.path().join(".").join("index.scip") };
        fs::write(&input, INPUT)?;
        fs::write(&earlier, EXISTING_OUTPUT)?;
        if hard_link {
            fs::hard_link(&input, &output)?;
        }
        let inputs = [InputFile::open(&input)?];

        let outputs = [FileOutput::new(&earlier, b"new diagram"), FileOutput::new(&output, b"overwrite")];
        let system_under_test = OutputFiles::open(&outputs, &inputs).and_then(OutputFiles::write);
        let input_contents = fs::read(&input)?;
        let earlier_contents = fs::read(&earlier)?;

        let conflict = system_under_test.is_err_and(|error| error.to_string().starts_with(OUTPUT_PATH_CONFLICT));
        assert!(conflict, "An output could overwrite a protected input");
        assert_eq!(input_contents, INPUT);
        assert_eq!(earlier_contents, EXISTING_OUTPUT);
        Ok(())
    }

    #[test]
    fn test_duplicate_names_before_creation() -> Result<()> {
        let fixture = tempdir()?;
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
        Ok(())
    }

    #[test]
    fn test_protected_input_name_before_creation() -> Result<()> {
        const CONTENTS: &[u8] = b"protected input";
        let fixture = tempdir()?;
        let input = fixture.path().join("index.scip");
        let renamed = fixture.path().join("renamed.scip");
        let earlier = fixture.path().join("diagram.md");
        fs::write(&input, CONTENTS)?;
        let inputs = [InputFile::open(&input)?];
        fs::rename(&input, &renamed)?;

        let outputs = [FileOutput::new(&earlier, b"diagram"), FileOutput::new(&input, b"overwrite")];
        let system_under_test = OutputFiles::open(&outputs, &inputs);
        let earlier_contents = fs::read(&earlier);
        let input_contents = fs::read(&input);
        let renamed_contents = fs::read(&renamed)?;

        let conflict = system_under_test.is_err_and(|error| error.to_string().starts_with(OUTPUT_PATH_CONFLICT));
        assert!(conflict, "The lexical input name was not protected after renaming");
        assert!(earlier_contents.is_err_and(|error| error.kind() == ErrorKind::NotFound));
        assert!(input_contents.is_err_and(|error| error.kind() == ErrorKind::NotFound));
        assert_eq!(renamed_contents, CONTENTS);
        Ok(())
    }

    #[rstest]
    #[case::case_variant("report.md", "REPORT.md")]
    #[case::distinct("folders.md", "files.md")]
    fn test_new_output_identity(#[case] first_name: &str, #[case] second_name: &str) -> Result<()> {
        let fixture = tempdir()?;
        let first = fixture.path().join(first_name);
        let second = fixture.path().join(second_name);

        let outputs = [FileOutput::new(&first, b"first"), FileOutput::new(&second, b"second")];
        let system_under_test = OutputFiles::open(&outputs, &[]);
        let first_handle = Handle::from_file(File::open(&first)?)?;
        let second_handle = Handle::from_file(File::open(&second)?)?;
        let first_contents = fs::read(&first)?;
        let second_contents = fs::read(&second)?;

        let conflict = system_under_test.is_err();
        let aliases = first_handle == second_handle;
        assert_eq!(conflict, aliases, "Case handling must follow actual file identity, not an operating-system assumption");
        assert!(first_contents.is_empty());
        assert!(second_contents.is_empty());
        Ok(())
    }

    #[rstest]
    #[case::directory(PathBuf::new())]
    #[case::missing_parent(Path::new("missing").join("report.md"))]
    #[case::file_parent(Path::new("existing.md").join("report.md"))]
    fn test_output_open_failure(#[case] name: PathBuf) -> Result<()> {
        const CONTENTS: &[u8] = b"existing output";
        let fixture = tempdir()?;
        let existing = fixture.path().join("existing.md");
        let created = fixture.path().join("created.md");
        let invalid = fixture.path().join(name);
        fs::write(&existing, CONTENTS)?;

        let outputs = [
            FileOutput::new(&existing, b"overwrite"),
            FileOutput::new(&created, b"new output"),
            FileOutput::new(&invalid, b"invalid output"),
        ];
        let system_under_test = OutputFiles::open(&outputs, &[]).and_then(OutputFiles::write);
        let existing_contents = fs::read(&existing)?;
        let created_contents = fs::read(&created)?;

        let has_path_and_io_error = system_under_test.is_err_and(|error| {
            error.downcast_ref::<Error>().is_some() && error.to_string().contains(&invalid.display().to_string())
        });
        assert!(has_path_and_io_error, "Output-opening failures must retain the path and underlying I/O error");
        assert_eq!(existing_contents, CONTENTS, "An opening failure truncated an existing output");
        assert!(created_contents.is_empty(), "Opening must not write newly created outputs");
        Ok(())
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
    fn test_output_writes(#[case] contents: &[u8], #[values(false, true)] existing: bool) -> Result<()> {
        const ORIGINAL: &[u8] = b"old contents longer than the replacement";
        const SECOND: &[u8] = b"second output";
        let fixture = tempdir()?;
        let first = fixture.path().join("first.md");
        let second = fixture.path().join("second.md");
        if existing {
            fs::write(&first, ORIGINAL)?;
            fs::write(&second, ORIGINAL)?;
        }

        let outputs = [FileOutput::new(&first, contents), FileOutput::new(&second, SECOND)];
        let mut system_under_test = OutputFiles::open(&outputs, &[])?;
        let first_before = fs::read(&first)?;
        let second_before = fs::read(&second)?;
        for file in &mut system_under_test.files {
            file.handle.as_file_mut().seek(SeekFrom::End(0))?;
        }
        system_under_test.write()?;
        let first_after = fs::read(&first)?;
        let second_after = fs::read(&second)?;

        let before = if existing { ORIGINAL } else { b"" };
        assert_eq!(first_before, before, "Opening truncated an output");
        assert_eq!(second_before, before, "Opening truncated an output");
        assert_eq!(first_after, contents, "Writing must truncate and seek to the start");
        assert_eq!(second_after, SECOND);
        Ok(())
    }

    #[rstest]
    fn test_output_path_replacement(#[values(false, true)] existing: bool) -> Result<()> {
        const INPUT: &[u8] = b"protected input";
        const OUTPUT: &[u8] = b"new report";
        let fixture = tempdir()?;
        let input = fixture.path().join("index.scip");
        let output = fixture.path().join("report.md");
        let renamed = fixture.path().join("renamed.md");
        fs::write(&input, INPUT)?;
        if existing {
            fs::write(&output, b"original report contents")?;
        }
        let inputs = [InputFile::open(&input)?];
        let system_under_test = OutputFiles::open(&[FileOutput::new(&output, OUTPUT)], &inputs)?;

        fs::rename(&output, &renamed)?;
        fs::hard_link(&input, &output)?;
        system_under_test.write()?;
        let input_contents = fs::read(&input)?;
        let output_contents = fs::read(&output)?;
        let renamed_contents = fs::read(&renamed)?;

        assert_eq!(input_contents, INPUT, "Writing followed a replacement path into the protected input");
        assert_eq!(output_contents, INPUT);
        assert_eq!(renamed_contents, OUTPUT, "Writing did not target the original opened output");
        Ok(())
    }

    #[test]
    fn test_output_write_error() -> Result<()> {
        const CONTENTS: &[u8] = b"read-only handle";
        let fixture = tempdir()?;
        let path = fixture.path().join("report.md");
        fs::write(&path, CONTENTS)?;
        let system_under_test = OutputFiles {
            files: vec![OpenedOutput {
                output: FileOutput::new(&path, b"overwrite"),
                handle: Handle::from_file(File::open(&path)?)?,
            }],
        };

        let result = system_under_test.write();
        let contents = fs::read(&path)?;

        let has_path_and_io_error = result.is_err_and(|error| {
            error.downcast_ref::<Error>().is_some() && error.to_string().contains(&path.display().to_string())
        });
        assert!(has_path_and_io_error, "A failed handle write must retain its path and I/O error rather than reopening the path");
        assert_eq!(contents, CONTENTS);
        Ok(())
    }

    #[test]
    fn test_empty_output_plan() -> Result<()> {
        let system_under_test = OutputFiles::open(&[], &[])?;

        system_under_test.write()?;
        Ok(())
    }
}
