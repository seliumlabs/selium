use super::{certificate_builder::CertificateBuilder, key_pair::KeyPair};
use crate::cli::GenCertsArgs;
use anyhow::Result;
use colored::*;
use rcgen::Certificate;
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

fn generate_ca_cert() -> Result<Certificate> {
    let cert = CertificateBuilder::ca()
        .country_name("AU")
        .organization_name("Selium")
        .valid_for_days(5)
        .build()?;

    Ok(cert)
}

fn write_file(filename: &Path, contents: &[u8]) -> Result<()> {
    File::create(filename)?.write_all(contents)?;

    println!(
        "{}",
        format!("Successfully created {}", filename.display()).green()
    );

    Ok(())
}

pub struct CertGen {
    pub ca: Vec<u8>,
    pub client: KeyPair,
    pub server: KeyPair,
}

impl CertGen {
    pub fn generate() -> Result<Self> {
        println!("Generating certificates...");

        let ca = generate_ca_cert()?;
        let client = KeyPair::client(&ca)?;
        let server = KeyPair::server(&ca)?;
        let ca = ca.serialize_der()?;

        Ok(Self { ca, client, server })
    }

    pub fn output(&self, args: GenCertsArgs) -> Result<()> {
        println!("Writing certs to filesystem...");

        self.write_to_filesystem(&args.client_out_path, &self.client)?;
        self.write_to_filesystem(&args.server_out_path, &self.server)?;

        Ok(())
    }

    fn write_to_filesystem(&self, path: &Path, keypair: &KeyPair) -> Result<()> {
        fs::create_dir_all(path)?;

        write_file(&path.join("ca.der"), &self.ca)?;
        write_file(&path.join("localhost.der"), &keypair.0)?;
        write_file(&path.join("localhost.key.der"), &keypair.1)?;

        Ok(())
    }
}
