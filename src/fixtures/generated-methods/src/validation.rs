#[test]
fn test_generated_setter() {
    let mut measurement = crate::factory::make();

    measurement.set_value(11);

    assert_eq!(*measurement.value(), 11);
}
