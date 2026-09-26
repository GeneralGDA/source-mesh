#[test]
fn test_generated_setter() {
    let mut system_under_test = crate::factory::make();

    system_under_test.set_value(11);

    assert_eq!(*system_under_test.value(), 11);
}
