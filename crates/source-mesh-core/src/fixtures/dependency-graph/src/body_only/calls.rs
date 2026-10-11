#[must_use]
pub const fn call() -> u32 { crate::persistence::load() }
