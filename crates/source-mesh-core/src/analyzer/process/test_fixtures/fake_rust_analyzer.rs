#![cfg(test)]

use std::env;
use std::error::Error;
use std::fs;
use std::io;
use std::io::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> Result<ExitCode, Box<dyn Error>> {
    let test_directory = PathBuf::from(env::var_os("SOURCE_MESH_RUST_ANALYZER_TEST_DIRECTORY").ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "Missing rust-analyzer test directory")
    })?);
    let arguments: Vec<_> = env::args_os().skip(1).collect();
    let recorded_arguments: Vec<_> = arguments.iter().map(|argument| argument.to_string_lossy()).collect();
    fs::write(test_directory.join("arguments"), recorded_arguments.join("\n"))?;
    fs::write(test_directory.join("working-directory"), env::current_dir()?.to_string_lossy().as_bytes())?;
    let index_file = arguments.windows(2).find_map(|pair| match pair {
        [flag, path] if flag == "--output" => Some(PathBuf::from(path)),
        _ => None,
    });
    let index_file = index_file.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Missing --output argument"))?;
    match fs::read_to_string(test_directory.join("output-kind"))?.as_str() {
        "file" => {
            fs::copy(test_directory.join("index"), index_file)?;
        }
        "directory" => fs::create_dir_all(index_file)?,
        "missing" => {}
        _ => return Err(io::Error::new(io::ErrorKind::InvalidInput, "Unknown test analyzer output kind").into()),
    }
    io::stdout().write_all(&fs::read(test_directory.join("stdout"))?)?;
    io::stderr().write_all(&fs::read(test_directory.join("stderr"))?)?;
    let exit_code = fs::read_to_string(test_directory.join("exit-code"))?.parse::<u8>()?;
    Ok(ExitCode::from(exit_code))
}
