//! TLS for the uplinks that want it (rtmps://, an https origin): rustls with
//! ring's cryptography, the server's certificate checked against the public
//! roots (Mozilla's, as webpki-roots carries them) and, where the operator names
//! one, a PEM file of their own authority for a server behind a private one.

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::sync::Arc;

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, ServerName};
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};

/// The wire: the socket bare, or inside TLS.
pub enum Wire {
    Plain(TcpStream),
    Tls(Box<StreamOwned<ClientConnection, TcpStream>>),
}

impl Wire {
    pub fn socket(&self) -> &TcpStream {
        match self {
            Wire::Plain(s) => s,
            Wire::Tls(t) => t.get_ref(),
        }
    }
}

impl Read for Wire {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Wire::Plain(s) => s.read(buf),
            Wire::Tls(t) => t.read(buf),
        }
    }
}

impl Write for Wire {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Wire::Plain(s) => s.write(buf),
            Wire::Tls(t) => t.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Wire::Plain(s) => s.flush(),
            Wire::Tls(t) => t.flush(),
        }
    }
}

/// The roots a server's certificate may chain to: the public ones (Mozilla's, as
/// carried by webpki-roots), and those of the operator's PEM file when one is named.
pub fn roots(ca_file: &str) -> Result<RootCertStore, String> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    if !ca_file.trim().is_empty() {
        let path = Path::new(ca_file.trim());
        let mut added = 0;
        for cert in
            CertificateDer::pem_file_iter(path).map_err(|e| format!("{}: {e}", path.display()))?
        {
            let cert = cert.map_err(|e| format!("{}: {e}", path.display()))?;
            roots
                .add(cert)
                .map_err(|e| format!("{}: {e}", path.display()))?;
            added += 1;
        }
        if added == 0 {
            return Err(format!("{}: no certificate in it", path.display()));
        }
    }
    Ok(roots)
}

/// The socket wrapped in TLS and the handshake done, the server's certificate
/// checked against the roots and the host name.
pub fn wrap_tls(sock: TcpStream, host: &str, ca_file: &str) -> Result<Wire, String> {
    let config = ClientConfig::builder()
        .with_root_certificates(roots(ca_file)?)
        .with_no_client_auth();
    let name = ServerName::try_from(host.to_string())
        .map_err(|_| format!("{host}: not a name TLS takes"))?;
    let conn = ClientConnection::new(Arc::new(config), name).map_err(|e| format!("tls: {e}"))?;
    let mut tls = StreamOwned::new(conn, sock);
    while tls.conn.is_handshaking() {
        tls.conn
            .complete_io(&mut tls.sock)
            .map_err(|e| format!("tls handshake with {host}: {e}"))?;
    }
    Ok(Wire::Tls(Box::new(tls)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_roots_are_the_public_ones_and_the_operators_file() {
        assert!(roots("").unwrap().len() > 100, "the public roots");
        assert!(roots("/nonexistent/ca.pem").unwrap_err().contains("ca.pem"));
        let dir = std::env::temp_dir().join(format!("outcast-roots-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let empty = dir.join("empty.pem");
        std::fs::write(&empty, "nothing here\n").unwrap();
        assert!(roots(empty.to_str().unwrap())
            .unwrap_err()
            .contains("no certificate"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
