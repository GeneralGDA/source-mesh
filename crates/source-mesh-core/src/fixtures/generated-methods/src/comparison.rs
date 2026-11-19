#[must_use]
pub fn equal_values() -> bool {
    crate::factory::make() /* Ignore == and /* != */ inside
        comments. */ == crate::factory::make()
}

#[must_use]
pub fn unequal_values() -> bool {
    crate::factory::make() // Ignore != inside comments.
        != crate::factory::make()
}
