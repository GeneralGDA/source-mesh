#[cfg(test)]
mod workspace_tests {
    #![expect(
        clippy::panic_in_result_fn,
        reason = "Tests use assertions for expectations while returning Result for fallible setup."
    )]

    use std::path::Path;
    use std::process::Command;

    use anyhow::Context as _;
    use anyhow::Result;
    use pretty_assertions::assert_eq;
    use serde_json::Value;

    #[test]
    fn test_workspace_dependency_boundary() -> Result<()> {
        const CORE_PACKAGE_NAME: &str = "source-mesh-core";
        let repository_directory = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent().and_then(Path::parent)
            .context("CLI crate must be located under the workspace crates directory")?;
        let mut system_under_test = Command::new(env!("CARGO"));
        system_under_test.current_dir(repository_directory)
            .args(["metadata", "--format-version", "1", "--no-deps", "--locked", "--offline"]);

        let output = system_under_test.output()?;

        assert!(output.status.success(), "Cannot inspect workspace: {}", String::from_utf8_lossy(&output.stderr));
        let metadata: Value = serde_json::from_slice(&output.stdout)?;
        let members = metadata.get("workspace_members").and_then(Value::as_array)
            .context("Cargo metadata omitted workspace members")?;
        let default_members = metadata.get("workspace_default_members").and_then(Value::as_array)
            .context("Cargo metadata omitted default workspace members")?;
        let packages = metadata.get("packages").and_then(Value::as_array)
            .context("Cargo metadata omitted packages")?;
        let frontend = packages.iter()
            .find(|package| package.get("name").and_then(Value::as_str) == Some(env!("CARGO_PKG_NAME")))
            .context("Workspace has no CLI package")?;
        let core = packages.iter()
            .find(|package| package.get("name").and_then(Value::as_str) == Some(CORE_PACKAGE_NAME))
            .context("Workspace has no library package")?;
        let frontend_manifest = frontend.get("manifest_path").and_then(Value::as_str)
            .context("CLI package has no manifest path")?;
        let core_manifest = core.get("manifest_path").and_then(Value::as_str)
            .context("Library package has no manifest path")?;
        let crates_directory = repository_directory.join("crates");
        let frontend_targets = frontend.get("targets").and_then(Value::as_array)
            .context("CLI package has no targets")?;
        let core_targets = core.get("targets").and_then(Value::as_array)
            .context("Library package has no targets")?;
        let frontend_dependencies = frontend.get("dependencies").and_then(Value::as_array)
            .context("CLI package has no dependency metadata")?;
        let core_dependencies = core.get("dependencies").and_then(Value::as_array)
            .context("Library package has no dependency metadata")?;

        assert_eq!(members.len(), 2, "Only the CLI and library belong to the workspace");
        assert_eq!(default_members.len(), members.len(), "Default checks must cover every workspace member");
        assert!(members.iter().all(|member| default_members.contains(member)), "Default checks must cover both crates");
        assert_eq!(Path::new(frontend_manifest), crates_directory.join(env!("CARGO_PKG_NAME")).join("Cargo.toml"),
            "The CLI must be a sibling crate, not the workspace root package");
        assert_eq!(Path::new(core_manifest), crates_directory.join(CORE_PACKAGE_NAME).join("Cargo.toml"),
            "The library must be a sibling of the CLI crate");
        assert!(frontend_targets.iter().any(|target|
            target.get("name").and_then(Value::as_str) == Some(env!("CARGO_PKG_NAME"))
                && target.get("kind") == Some(&serde_json::json!(["bin"]))),
            "The installable source-mesh executable is missing");
        assert!(!frontend_targets.iter().any(|target| target.get("kind") == Some(&serde_json::json!(["lib"]))),
            "The CLI package must not own a library implementation");
        assert!(core_targets.iter().any(|target| target.get("kind") == Some(&serde_json::json!(["lib"]))),
            "The core package must expose a library");
        assert!(!core_targets.iter().any(|target| target.get("kind") == Some(&serde_json::json!(["bin"]))),
            "The core package must not own the CLI");
        assert!(frontend_dependencies.iter().any(|dependency|
            dependency.get("name").and_then(Value::as_str) == Some(CORE_PACKAGE_NAME)
                && dependency.get("req").and_then(Value::as_str) != Some("*")
                && dependency.get("path").is_some()),
            "The CLI needs a versioned local dependency that Cargo can publish");
        assert!(!core_dependencies.iter().any(|dependency|
            matches!(dependency.get("name").and_then(Value::as_str), Some("clap"))
                || dependency.get("name").and_then(Value::as_str) == Some(env!("CARGO_PKG_NAME"))),
            "The library must not depend on the CLI or its argument parser");
        Ok(())
    }
}
