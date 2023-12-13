use super::{certificate_builder::CertificateBuilder, key_pair::KeyPair};
use anyhow::Result;
use colored::*;
use rcgen::Certificate;
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

fn generate_ca_cert(no_expiry: bool) -> Result<Certificate> {
    let mut builder = CertificateBuilder::ca()
        .country_name("AU")
        .organization_name("Selium");

    if !no_expiry {
        builder = builder.valid_for_days(5);
    }

    let cert = builder.build()?;

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
    pub fn generate(no_expiry: bool) -> Result<Self> {
        println!("Generating certificates...");

        let ca = generate_ca_cert(no_expiry)?;
        let client = KeyPair::client(&ca, no_expiry)?;
        let server = KeyPair::server(&ca, no_expiry)?;
        let ca = ca.serialize_der()?;

        Ok(Self { ca, client, server })
    }

    pub fn output(&self, client_out_path: &Path, server_out_path: &Path) -> Result<()> {
        println!("Writing certs to filesystem...");

        self.write_to_filesystem(client_out_path, &self.client)?;
        self.write_to_filesystem(server_out_path, &self.server)?;

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
