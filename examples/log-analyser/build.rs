use std::{error::Error, io};

use flatbuffers_build::BuilderOptions;
use flatc_fork::flatc;

const SCHEMAS: [&str; 1] = ["schemas/log.fbs"];

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo::rerun-if-changed=schemas/");

    let compiler = flatc();
    let compiler = compiler.to_str().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "non-utf8 path to flatc binary")
    })?;

    BuilderOptions::new_with_files(SCHEMAS)
        .set_output_path("src/fbs/")
        .set_compiler(compiler)
        .compile()?;

    Ok(())
}
