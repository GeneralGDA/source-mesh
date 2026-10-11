use crate::persistence::load;
use crate::persistence::Value;

#[must_use]
pub const fn check() -> u32 { load() }
pub const fn typed(_: Value) {}

