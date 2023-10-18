mod cert_gen;
mod certificate_builder;
mod key_pair;
mod validity_range;

use crate::cli::GenCertsArgs;
use crate::commands::gen_certs::cert_gen::CertGen;
use crate::traits::CommandRunner;
use anyhow::Result;
use colored::*;

pub struct GenCertsRunner {
    args: GenCertsArgs,
}

impl From<GenCertsArgs> for GenCertsRunner {
    fn from(args: GenCertsArgs) -> Self {
        Self { args }
    }
}

impl CommandRunner for GenCertsRunner {
    fn run(self) -> Result<()> {
        eprintln!(
            "{}",
            "Warning! Using a self-signed certificate does not protect from person-in-the-middle attacks.".yellow()
        );

        let cert_gen = CertGen::generate()?;
        cert_gen.output(self.args)?;

        Ok(())
    }
}
