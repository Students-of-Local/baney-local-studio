use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use anyhow::{ensure, Context};

pub fn test_ca(root: &Path) -> anyhow::Result<PathBuf> {
    let dir = root.join("tls");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(
        dir.join("leaf.ext"),
        "subjectAltName=DNS:localhost,IP:127.0.0.1\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\n",
    )?;
    let ca: &[&str] = &[
        "req",
        "-x509",
        "-newkey",
        "rsa:2048",
        "-nodes",
        "-keyout",
        "ca.key",
        "-out",
        "ca.pem",
        "-days",
        "2",
        "-subj",
        "/CN=smoke test CA",
        "-addext",
        "basicConstraints=critical,CA:TRUE",
        "-addext",
        "keyUsage=critical,keyCertSign,cRLSign",
    ];
    let request: &[&str] =
        &["req", "-newkey", "rsa:2048", "-nodes", "-keyout", "leaf.key", "-out", "leaf.csr", "-subj", "/CN=localhost"];
    let sign: &[&str] = &[
        "x509",
        "-req",
        "-in",
        "leaf.csr",
        "-CA",
        "ca.pem",
        "-CAkey",
        "ca.key",
        "-CAcreateserial",
        "-out",
        "leaf.pem",
        "-days",
        "2",
        "-extfile",
        "leaf.ext",
    ];
    for args in [ca, request, sign] {
        let output = Command::new("openssl").args(args).current_dir(&dir).output().context("run openssl")?;
        ensure!(output.status.success(), "openssl {args:?}: {}", String::from_utf8_lossy(&output.stderr));
    }
    Ok(dir)
}

pub fn acceptor(tls: &Path) -> anyhow::Result<tokio_rustls::TlsAcceptor> {
    use tokio_rustls::rustls;
    use tokio_rustls::rustls::pki_types::pem::PemObject;
    let certs =
        rustls::pki_types::CertificateDer::pem_file_iter(tls.join("leaf.pem"))?.collect::<Result<Vec<_>, _>>()?;
    let key = rustls::pki_types::PrivateKeyDer::from_pem_file(tls.join("leaf.key"))?;
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(certs, key)?;
    Ok(tokio_rustls::TlsAcceptor::from(Arc::new(config)))
}
