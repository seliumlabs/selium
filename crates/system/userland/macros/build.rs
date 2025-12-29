fn main() {
    println!(
        "cargo:rustc-env=SEL_USERLAND_MACROS_DIR={}",
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR missing")
    );
}
