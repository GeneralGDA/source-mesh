#[test]
fn test_out_of_line_module() {
    let system_under_test = crate::test_only::value::read();

    assert_eq!(system_under_test, 7);
}
