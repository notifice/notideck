// SPDX-License-Identifier: GPL-3.0-or-later
//! The host's long-lived TLS identity (ADR 0004 §2): an ECDSA P-256 self-signed certificate plus
//! a stable host id, stored in the config directory.

use std::io::Write;
use std::path::Path;

use rcgen::{date_time_ymd, CertificateParams, DistinguishedName, DnType, KeyPair};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};

use crate::error::{Result, TransportError};
use crate::pairing::Fingerprint;

/// File holding the certificate and PKCS#8 private key (PEM, user-only permissions).
pub const IDENTITY_FILE: &str = "host-identity.pem";
/// File holding the host id (UUID v4).
pub const HOST_ID_FILE: &str = "host-id";

/// Common name of the host certificate.
const HOST_CN: &str = "Notideck Host";

/// The host's certificate, private key and id.
#[derive(Clone)]
pub struct HostIdentity {
    host_id: String,
    cert_der: Vec<u8>,
    key_der: Vec<u8>,
    fingerprint: Fingerprint,
}

impl std::fmt::Debug for HostIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostIdentity")
            .field("host_id", &self.host_id)
            .field("fingerprint", &self.fingerprint.short())
            .finish_non_exhaustive()
    }
}

impl HostIdentity {
    /// Load the identity from `dir`, generating (and persisting) whatever is missing or corrupt.
    pub fn load_or_create(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir)?;
        let (cert_der, key_der) = match load_pair(&dir.join(IDENTITY_FILE)) {
            Ok(pair) => pair,
            Err(reason) => {
                if dir.join(IDENTITY_FILE).exists() {
                    tracing::warn!(%reason, "host identity file is unusable; generating a new one");
                } else {
                    tracing::info!("generating host identity");
                }
                let (cert, key, pem) = generate_p256(HOST_CN, true)?;
                write_private(&dir.join(IDENTITY_FILE), pem.as_bytes())?;
                (cert, key)
            }
        };
        let host_id = match std::fs::read_to_string(dir.join(HOST_ID_FILE))
            .ok()
            .and_then(|s| uuid::Uuid::parse_str(s.trim()).ok())
        {
            Some(id) => id.to_string(),
            None => {
                let id = uuid::Uuid::new_v4().to_string();
                write_private(&dir.join(HOST_ID_FILE), id.as_bytes())?;
                id
            }
        };
        let fingerprint = Fingerprint::of_der(&cert_der);
        Ok(Self { host_id, cert_der, key_der, fingerprint })
    }

    /// Stable host id (UUID), the `h` of the pairing URI and `Welcome.host_id`.
    pub fn host_id(&self) -> &str {
        &self.host_id
    }

    /// SHA-256 of the certificate DER.
    pub fn fingerprint(&self) -> Fingerprint {
        self.fingerprint
    }

    pub fn cert_der(&self) -> &[u8] {
        &self.cert_der
    }

    /// PKCS#8 DER private key. Handle with care; never log.
    pub fn key_der(&self) -> &[u8] {
        &self.key_der
    }

    pub(crate) fn rustls_cert(&self) -> CertificateDer<'static> {
        CertificateDer::from(self.cert_der.clone())
    }
}

/// Parse a PEM file holding one certificate and one PKCS#8 key, checking they belong together
/// and that the key is P-256.
fn load_pair(path: &Path) -> std::result::Result<(Vec<u8>, Vec<u8>), String> {
    let pem = std::fs::read(path).map_err(|e| e.to_string())?;
    let cert = CertificateDer::from_pem_slice(&pem).map_err(|e| format!("certificate: {e}"))?;
    let key = PrivatePkcs8KeyDer::from_pem_slice(&pem).map_err(|e| format!("private key: {e}"))?;
    let pair = KeyPair::try_from(key.secret_pkcs8_der()).map_err(|e| format!("private key: {e}"))?;
    if !crate::tls::is_p256_cert(&cert) {
        return Err("certificate is not an ECDSA P-256 certificate".into());
    }
    let spki = crate::tls::p256_public_key(&cert).ok_or("certificate public key unreadable")?;
    if pair.public_key_raw() != spki.as_slice() {
        return Err("certificate and private key do not match".into());
    }
    Ok((cert.as_ref().to_vec(), key.secret_pkcs8_der().to_vec()))
}

/// Generate a self-signed ECDSA P-256 certificate. Returns `(cert_der, pkcs8_key_der, pem)`
/// where `pem` is the certificate followed by the private key.
pub(crate) fn generate_p256(cn: &str, long_lived: bool) -> Result<(Vec<u8>, Vec<u8>, String)> {
    let tls = |e: rcgen::Error| TransportError::Tls(format!("certificate generation: {e}"));
    let key_pair = KeyPair::generate().map_err(tls)?;
    let mut params = CertificateParams::new(Vec::<String>::new()).map_err(tls)?;
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, cn);
    params.distinguished_name = dn;
    // Nobody checks validity (identity is the fingerprint), but a wide window avoids trouble
    // with phones whose clock is wrong.
    params.not_before = date_time_ymd(2020, 1, 1);
    params.not_after = if long_lived { date_time_ymd(2046, 1, 1) } else { date_time_ymd(2040, 1, 1) };
    let cert = params.self_signed(&key_pair).map_err(tls)?;
    let pem = format!("{}{}", cert.pem(), key_pair.serialize_pem());
    Ok((cert.der().to_vec(), key_pair.serialize_der(), pem))
}

/// Write `data` atomically (tmp + rename), user-only permissions on unix.
fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    let mut tmp = path.to_path_buf();
    let mut name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(format!(".{}.tmp", std::process::id()));
    tmp.set_file_name(name);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let res = (|| {
        let mut f = opts.open(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    Ok(res?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("nd-identity-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn create_then_reload_is_stable() {
        let dir = tmpdir();
        let a = HostIdentity::load_or_create(&dir).unwrap();
        let b = HostIdentity::load_or_create(&dir).unwrap();
        assert_eq!(a.fingerprint(), b.fingerprint());
        assert_eq!(a.host_id(), b.host_id());
        assert_eq!(a.fingerprint(), Fingerprint::of_der(a.cert_der()));
        assert!(uuid::Uuid::parse_str(a.host_id()).is_ok());
        assert!(crate::tls::is_p256_cert(a.cert_der()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join(IDENTITY_FILE)).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn corrupt_files_are_regenerated() {
        let dir = tmpdir();
        let a = HostIdentity::load_or_create(&dir).unwrap();
        std::fs::write(dir.join(IDENTITY_FILE), b"garbage").unwrap();
        std::fs::write(dir.join(HOST_ID_FILE), b"not-a-uuid").unwrap();
        let b = HostIdentity::load_or_create(&dir).unwrap();
        assert_ne!(a.fingerprint(), b.fingerprint());
        assert_ne!(a.host_id(), b.host_id());
        let c = HostIdentity::load_or_create(&dir).unwrap();
        assert_eq!(b.fingerprint(), c.fingerprint());
        assert_eq!(b.host_id(), c.host_id());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn mismatched_key_is_regenerated() {
        let dir = tmpdir();
        let a = HostIdentity::load_or_create(&dir).unwrap();
        let (_, _, other) = generate_p256("x", false).unwrap();
        // cert of `a`, key of another pair
        let pem = std::fs::read_to_string(dir.join(IDENTITY_FILE)).unwrap();
        let cert_part = &pem[..pem.find("-----BEGIN PRIVATE KEY").unwrap()];
        let key_part = &other[other.find("-----BEGIN PRIVATE KEY").unwrap()..];
        std::fs::write(dir.join(IDENTITY_FILE), format!("{cert_part}{key_part}")).unwrap();
        let b = HostIdentity::load_or_create(&dir).unwrap();
        assert_ne!(a.fingerprint(), b.fingerprint());
        std::fs::remove_dir_all(dir).ok();
    }
}
