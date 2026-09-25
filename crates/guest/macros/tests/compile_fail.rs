#[test]
fn ui() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/*.rs");
}

#[test]
fn ui_pass() {
    let t = trybuild::TestCases::new();
    t.pass("tests/ui-pass/*.rs");
}
