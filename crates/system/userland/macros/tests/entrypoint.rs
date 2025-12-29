use trybuild::TestCases;

#[test]
fn entrypoint_macro_shape() {
    let t = TestCases::new();
    t.pass("tests/entrypoint/pass/*.rs");
    t.compile_fail("tests/entrypoint/fail/*.rs");
}
