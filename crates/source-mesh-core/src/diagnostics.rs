use std::fmt::Arguments;

pub(crate) fn emit(message: Arguments<'_>) {
    eprintln!("{message}");
}
