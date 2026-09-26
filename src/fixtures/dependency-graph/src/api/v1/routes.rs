use crate::domain::request::parse;
use crate::persistence::load;

#[must_use]
pub const fn route() -> u32 { parse() + load() }

