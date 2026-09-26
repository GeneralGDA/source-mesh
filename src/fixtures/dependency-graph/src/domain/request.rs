use super::rules::check;

pub struct Request;
#[must_use]
pub const fn parse() -> u32 { 1 }
#[must_use]
pub const fn validate() -> u32 { check() }
