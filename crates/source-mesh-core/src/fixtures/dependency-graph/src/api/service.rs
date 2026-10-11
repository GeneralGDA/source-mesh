use super::http::get;
use super::v1::routes::route;

#[must_use]
pub const fn serve() -> u32 { get() + route() }
