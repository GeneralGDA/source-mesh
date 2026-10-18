use std::fmt::Arguments;

pub(crate) const MESSAGE_VERBOSE_DIAGNOSTICS_HINT: &str = "Use --verbose for diagnostic details.";

pub(crate) fn emit(message: Arguments<'_>) {
    eprintln!("{message}");
}
