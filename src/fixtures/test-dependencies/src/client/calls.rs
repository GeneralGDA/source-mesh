#[must_use]
pub const fn production() -> u32 { crate::shared::value::read() }

#[cfg(test)]
mod tests {
    #[test]
    fn test_dependencies() {
        let system_under_test = crate::shared::value::read();
        let fixture = crate::test_only::value::read();

        assert_eq!(system_under_test, fixture);
    }
}
