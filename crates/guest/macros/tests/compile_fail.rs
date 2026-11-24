// scenario: selium-guest-macros/result-entrypoint-error-type-validation/non-error-error-type-rejected
// scenario: selium-guest-macros/context-aware-entrypoint-parameter/unsupported-parameter-type-rejected
// scenario: schema-wire-generation/error-on-generic-struct-without-wire-parameter/generic-struct-without-wire-fails-with-clear-error
#[test]
fn ui() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/*.rs");
}
// scenario: selium-guest-macros/result-entrypoint-error-type-validation/error-typed-error-type-accepted
// scenario: selium-guest-macros/context-aware-entrypoint-parameter/macro-injects-context-into-entrypoint
// scenario: selium-guest-macros/context-aware-entrypoint-parameter/macro-forwards-multiple-integer-arguments
// scenario: selium-guest-macros/context-aware-entrypoint-parameter/context-combines-with-trailing-arguments

#[test]
fn ui_pass() {
    let t = trybuild::TestCases::new();
    t.pass("tests/ui-pass/*.rs");
}
