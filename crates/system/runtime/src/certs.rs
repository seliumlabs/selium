use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use rcgen::{
    BasicConstraints, Certificate, CertificateParams, DistinguishedName, DnType,
    ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair, KeyUsagePurpose,
};

/// Generated certificate material.
struct Generated {
    cert_pem: String,
    key_pem: String,
}

struct CaMaterial {
    cert: Certificate,
    key: KeyPair,
    params: CertificateParams,
}

/// Create a new CA plus server/client certificates and write them to disk.
pub fn generate_certificates(
    output_dir: &Path,
    ca_common_name: &str,
    server_name: &str,
    client_name: &str,
) -> Result<()> {
    fs::create_dir_all(output_dir).context("create certificate output directory")?;

    let ca = generate_ca(ca_common_name)?;
    write_pair(output_dir, "ca", &ca.cert.pem(), &ca.key.serialize_pem())?;

    let server = generate_leaf(server_name, LeafUsage::Server, &ca)?;
    write_pair(output_dir, "server", &server.cert_pem, &server.key_pem)?;

    let client = generate_leaf(client_name, LeafUsage::Client, &ca)?;
    write_pair(output_dir, "client", &client.cert_pem, &client.key_pem)?;

    println!("Wrote certificates to {}", output_dir.display());

    Ok(())
}

fn write_pair(dir: &Path, prefix: &str, cert_pem: &str, key_pem: &str) -> Result<()> {
    let cert_path = build_path(dir, prefix, "crt");
    let key_path = build_path(dir, prefix, "key");
    fs::write(&cert_path, cert_pem)
        .with_context(|| format!("write certificate {}", cert_path.display()))?;
    fs::write(&key_path, key_pem)
        .with_context(|| format!("write private key {}", key_path.display()))?;
    Ok(())
}

fn build_path(dir: &Path, prefix: &str, ext: &str) -> PathBuf {
    let mut path = dir.to_path_buf();
    path.push(format!("{prefix}.{ext}"));
    path
}

fn generate_ca(common_name: &str) -> Result<CaMaterial> {
    let mut params = CertificateParams::new(vec![]).context("build CA parameters")?;
    params.distinguished_name = dn(common_name);
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    let key = KeyPair::generate().context("generate CA key")?;
    let cert = params
        .self_signed(&key)
        .context("sign CA certificate with its key")?;
    Ok(CaMaterial { cert, key, params })
}

enum LeafUsage {
    Server,
    Client,
}

fn generate_leaf(name: &str, usage: LeafUsage, ca: &CaMaterial) -> Result<Generated> {
    let mut params =
        CertificateParams::new(vec![name.to_owned()]).context("build leaf parameters")?;
    params.distinguished_name = dn(name);
    params.is_ca = IsCa::ExplicitNoCa;
    params.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyEncipherment,
    ];
    params.extended_key_usages = match usage {
        LeafUsage::Server => vec![ExtendedKeyUsagePurpose::ServerAuth],
        LeafUsage::Client => vec![ExtendedKeyUsagePurpose::ClientAuth],
    };

    let key = KeyPair::generate().context("generate leaf key")?;
    let issuer = Issuer::from_params(&ca.params, &ca.key);
    let cert = params
        .signed_by(&key, &issuer)
        .context("sign leaf certificate with CA")?;
    let cert_pem = cert.pem();
    let key_pem = key.serialize_pem();
    Ok(Generated { cert_pem, key_pem })
}

fn dn(common_name: &str) -> DistinguishedName {
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, common_name);
    dn
}
