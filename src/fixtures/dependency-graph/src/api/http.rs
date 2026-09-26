use crate::domain::request::parse;
use crate::domain::request::validate;
use crate::domain::request::Request;
use crate::persistence::load;

#[must_use]
pub const fn get() -> u32 { parse() + validate() + load() }
pub const fn typed(_: Request) {}

