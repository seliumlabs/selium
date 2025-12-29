use flatbuffers_build::BuilderOptions;
use flatc::flatc;

const SCHEMAS: [&str; 1] = ["schemas/echo.fbs"];

fn main() {
    println!("cargo::rerun-if-changed=schemas/");

    BuilderOptions::new_with_files(SCHEMAS)
        .set_output_path("src/fbs/")
        .set_compiler(flatc().to_str().expect("Non UTF-8 path to flatc binary"))
        .compile()
        .expect("flatbuffer compilation failed");
}
