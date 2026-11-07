//! HTTPS for the panel. Browsers only let a page use the microphone (for
//! push to talk) when it comes over HTTPS, or from localhost.
//!
//! Use a real certificate (`--tls-cert`, `--tls-key`), or let the panel
//! make a self-signed one (`--tls-self-signed`): browsers warn about it
//! once, then work. It's kept beside the config, so it stays the same.

use anyhow::{bail, Context, Result};
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_rustls::rustls;
use tokio_rustls::rustls::pki_types::pem::PemObject;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::server::TlsStream;
use tokio_rustls::TlsAcceptor;

/// A certificate (chain) and its private key, as PEM files.
#[derive(Debug, Clone, PartialEq)]
pub struct TlsFiles {
    pub cert: PathBuf,
    pub key: PathBuf,
}

/// How long a client gets to finish the TLS handshake.
const HANDSHAKE: Duration = Duration::from_secs(10);

/// The panel's own self-signed certificate in `dir`, made the first time.
/// It names this machine (its host name, `.local` name and address),
/// `localhost`, `listen`'s address if it's a specific one, and `extra`.
pub fn self_signed(dir: &Path, listen: SocketAddr, extra: &[String]) -> Result<TlsFiles> {
    let files = TlsFiles {
        cert: dir.join("intercom-cert.pem"),
        key: dir.join("intercom-key.pem"),
    };
    if files.cert.exists() && files.key.exists() {
        return Ok(files);
    }
    let mut names: Vec<String> = vec!["localhost".into(), "127.0.0.1".into(), "::1".into()];
    if let Some(host) = host_name() {
        names.push(format!("{host}.local"));
        names.push(host);
    }
    if let Some(ip) = local_ip() {
        names.push(ip.to_string());
    }
    if !listen.ip().is_unspecified() {
        names.push(listen.ip().to_string());
    }
    names.extend(extra.iter().cloned());
    names.dedup();
    let made = rcgen::generate_simple_self_signed(names.clone())
        .context("making a self-signed certificate")?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    write_private(&files.key, made.key_pair.serialize_pem().as_bytes())?;
    std::fs::write(&files.cert, made.cert.pem())
        .with_context(|| format!("writing {}", files.cert.display()))?;
    eprintln!(
        "made a self-signed certificate for {} in {}",
        names.join(", "),
        files.cert.display()
    );
    Ok(files)
}

fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)
        .and_then(|mut f| f.write_all(data))
        .with_context(|| format!("writing {}", path.display()))
}

fn host_name() -> Option<String> {
    let name = std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .or_else(|| std::env::var("HOSTNAME").ok())
        .or_else(|| std::env::var("COMPUTERNAME").ok())?;
    let name = name.trim().to_string();
    (!name.is_empty()).then_some(name)
}

/// The address this machine reaches the network from (no packet is sent).
fn local_ip() -> Option<IpAddr> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("192.0.2.1:9").ok()?;
    s.local_addr().ok().map(|a| a.ip())
}

fn server_config(files: &TlsFiles) -> Result<Arc<rustls::ServerConfig>> {
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(&files.cert)
        .and_then(|c| c.collect())
        .with_context(|| format!("reading certificates from {}", files.cert.display()))?;
    if certs.is_empty() {
        bail!("no certificate in {}", files.cert.display());
    }
    let key = PrivateKeyDer::from_pem_file(&files.key)
        .with_context(|| format!("reading the private key from {}", files.key.display()))?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut cfg = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .context("the certificate and key don't go together")?;
    cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(cfg))
}

/// Accepts connections and does their TLS handshakes in the background,
/// so a slow client doesn't hold up the others.
pub struct TlsListener {
    addr: SocketAddr,
    ready: mpsc::Receiver<(TlsStream<TcpStream>, SocketAddr)>,
}

impl TlsListener {
    pub async fn bind(listen: SocketAddr, files: &TlsFiles) -> Result<Self> {
        let acceptor = TlsAcceptor::from(server_config(files)?);
        let tcp = tokio::net::TcpListener::bind(listen)
            .await
            .with_context(|| format!("listening on {listen}"))?;
        let addr = tcp.local_addr()?;
        let (tx, ready) = mpsc::channel(64);
        tokio::spawn(async move {
            loop {
                let (stream, peer) = match tcp.accept().await {
                    Ok(c) => c,
                    Err(_) => {
                        // Out of file descriptors and the like: wait a moment.
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                };
                if tx.is_closed() {
                    return;
                }
                let (acceptor, tx) = (acceptor.clone(), tx.clone());
                tokio::spawn(async move {
                    // A client that doesn't finish (or speaks plain HTTP) is dropped.
                    if let Ok(Ok(tls)) =
                        tokio::time::timeout(HANDSHAKE, acceptor.accept(stream)).await
                    {
                        let _ = tx.send((tls, peer)).await;
                    }
                });
            }
        });
        Ok(Self { addr, ready })
    }
}

impl axum::serve::Listener for TlsListener {
    type Io = TlsStream<TcpStream>;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        match self.ready.recv().await {
            Some(c) => c,
            // The accept loop is gone: nothing more will come.
            None => std::future::pending().await,
        }
    }

    fn local_addr(&self) -> std::io::Result<SocketAddr> {
        Ok(self.addr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn makes_a_certificate_once_and_loads_it() {
        let dir = std::env::temp_dir().join(format!("mcast-intercom-tls-{}", std::process::id()));
        let listen: SocketAddr = "0.0.0.0:8651".parse().unwrap();
        let files = self_signed(&dir, listen, &["panel.example".into()]).unwrap();
        let first = std::fs::read(&files.cert).unwrap();
        server_config(&files).unwrap();
        // Kept: the next start uses the same one.
        assert_eq!(self_signed(&dir, listen, &[]).unwrap(), files);
        assert_eq!(std::fs::read(&files.cert).unwrap(), first);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&files.key).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "the key is private");
        }
        // A key that isn't the certificate's is refused.
        let other = rcgen::generate_simple_self_signed(vec!["x".into()]).unwrap();
        std::fs::write(&files.key, other.key_pair.serialize_pem()).unwrap();
        assert!(server_config(&files).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
