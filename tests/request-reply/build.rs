use std::{io, process};

use flatbuffers_build::BuilderOptions;
use flatc::flatc;

const SCHEMAS: [&str; 1] = ["schemas/request_reply.fbs"];

fn main() {
    println!("cargo::rerun-if-changed=schemas/");

    if let Err(err) = compile_schemas() {
        eprintln!("failed to generate flatbuffer bindings: {err}");
        process::exit(1);
    }
}

fn compile_schemas() -> Result<(), Box<dyn std::error::Error>> {
    let compiler = flatc();
    let compiler = compiler.to_str().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "non-UTF8 path to flatc binary")
    })?;

    BuilderOptions::new_with_files(SCHEMAS)
        .set_output_path("src/fbs/")
        .set_compiler(compiler)
        .compile()?;

    Ok(())
}
