use trybuild::TestCases;

#[test]
fn schema_macro_shape() {
    let t = TestCases::new();
    t.pass("tests/schema/pass/*.rs");
    t.compile_fail("tests/schema/fail/*.rs");
}
