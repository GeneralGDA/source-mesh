#[must_use]
pub fn read_value() -> usize {
    *crate::factory::make().value()
}
