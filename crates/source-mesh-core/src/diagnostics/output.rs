use std::fmt::Arguments;

pub(crate) const MESSAGE_VERBOSE_DIAGNOSTICS_HINT: &str = "Use --verbose for diagnostic details.";

pub(crate) fn emit(message: Arguments<'_>) {
    eprintln!("{message}");
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::process::Command;

    use claims::assert_ok;
    use claims::assert_some;
    use pretty_assertions::assert_eq;

    use super::emit;

    const ENV_EMIT_FIXTURE: &str = "SOURCE_MESH_EMIT_FIXTURE";
    const LABEL_DIAGNOSTIC: &str = "fixture diagnostic \u{1f980}";

    #[test]
    fn test_emit() {
        let module = module_path!().split_once("::");
        let (_, module) = assert_some!(module, "The output fixture must be nested in the library test module");
        let filter = format!("{module}::{}", stringify!(test_emit_peer));
        let executable_lookup = env::current_exe();
        let executable = assert_ok!(executable_lookup, "The diagnostic output fixture must locate its test executable");
        let mut fixture = Command::new(executable);
        fixture.args(["--exact", &filter, "--ignored", "--nocapture"]);
        fixture.env(ENV_EMIT_FIXTURE, "1");

        let peer_execution = fixture.output();
        let output = assert_ok!(peer_execution, "The diagnostic output peer must launch and its output must be collected");

        assert!(output.status.success(), "The diagnostic output peer must exit successfully: {output:?}");
        assert_eq!(output.stderr, format!("{LABEL_DIAGNOSTIC}: 7\n").as_bytes(), "emit must preserve diagnostic bytes on stderr");
        assert!(!String::from_utf8_lossy(&output.stdout).contains(LABEL_DIAGNOSTIC), "Diagnostics must not pollute stdout");
    }

    #[test]
    #[ignore = "Executed by the diagnostic output test in a subprocess"]
    fn test_emit_peer() {
        if env::var_os(ENV_EMIT_FIXTURE).is_some() {
            let system_under_test = emit;
            system_under_test(format_args!("{LABEL_DIAGNOSTIC}: {}", 7));
        }
    }
}
