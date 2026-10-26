// SPDX-License-Identifier: GPL-3.0-or-later
//! rustls configuration for the control channel (ADR 0004 §1–§3).
//!
//! TLS 1.3 only, `ring` provider (never installed globally). Both sides present self-signed
//! ECDSA P-256 certificates; nobody validates chains or expiry, the identity is the SHA-256
//! fingerprint, which the host checks against its [`crate::TrustStore`] and the phone pins.

use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{ring, verify_tls13_signature, CryptoProvider};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{
    ClientConfig, DigitallySignedStruct, DistinguishedName, Error as TlsError, ServerConfig, SignatureScheme,
};

use crate::error::{Result, TransportError};
use crate::identity::HostIdentity;
use crate::pairing::Fingerprint;

const OID_EC_PUBLIC_KEY: &str = "1.2.840.10045.2.1";
const OID_P256: &str = "1.2.840.10045.3.1.7";

fn provider() -> Arc<CryptoProvider> {
    Arc::new(ring::default_provider())
}

/// Parse `der` and return the raw SubjectPublicKey (uncompressed SEC1 point) if the certificate
/// is a well-formed X.509 certificate with an ECDSA P-256 key.
pub(crate) fn p256_public_key(der: &[u8]) -> Option<Vec<u8>> {
    let (rest, cert) = x509_parser::parse_x509_certificate(der).ok()?;
    if !rest.is_empty() {
        return None;
    }
    let spki = &cert.tbs_certificate.subject_pki;
    if spki.algorithm.algorithm.to_id_string() != OID_EC_PUBLIC_KEY {
        return None;
    }
    let curve = spki.algorithm.parameters.as_ref()?.as_oid().ok()?;
    if curve.to_id_string() != OID_P256 {
        return None;
    }
    let key = spki.subject_public_key.data.as_ref();
    if key.len() != 65 || key[0] != 4 {
        return None;
    }
    Some(key.to_vec())
}

/// Whether `der` is a well-formed certificate carrying an ECDSA P-256 key.
pub(crate) fn is_p256_cert(der: &[u8]) -> bool {
    p256_public_key(der).is_some()
}

/// Accepts any well-formed certificate with a P-256 key and verifies the handshake signature.
#[derive(Debug)]
struct AnyP256ClientVerifier {
    provider: Arc<CryptoProvider>,
}

impl ClientCertVerifier for AnyP256ClientVerifier {
    fn offer_client_auth(&self) -> bool {
        true
    }

    fn client_auth_mandatory(&self) -> bool {
        true
    }

    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> std::result::Result<ClientCertVerified, TlsError> {
        if is_p256_cert(end_entity) {
            Ok(ClientCertVerified::assertion())
        } else {
            Err(TlsError::General("client certificate must be an ECDSA P-256 certificate".into()))
        }
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, TlsError> {
        Err(TlsError::General("TLS 1.2 is not supported".into()))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, TlsError> {
        if dss.scheme != SignatureScheme::ECDSA_NISTP256_SHA256 {
            return Err(TlsError::General("client must sign with ecdsa_secp256r1_sha256".into()));
        }
        verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ECDSA_NISTP256_SHA256]
    }
}

/// Server side of the control channel: TLS 1.3, host identity, mandatory client certificate.
pub fn server_config(identity: &HostIdentity) -> Result<ServerConfig> {
    let p = provider();
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(identity.key_der().to_vec()));
    let cfg = ServerConfig::builder_with_provider(p.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| TransportError::Tls(e.to_string()))?
        .with_client_cert_verifier(Arc::new(AnyP256ClientVerifier { provider: p }))
        .with_single_cert(vec![identity.rustls_cert()], key)
        .map_err(|e| TransportError::Tls(e.to_string()))?;
    Ok(cfg)
}

/// Pins the host fingerprint when `expected` is set, trusts on first use otherwise.
#[derive(Debug)]
struct PinVerifier {
    provider: Arc<CryptoProvider>,
    expected: Option<Fingerprint>,
}

impl ServerCertVerifier for PinVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, TlsError> {
        if let Some(expected) = self.expected {
            if Fingerprint::of_der(end_entity) != expected {
                return Err(TlsError::General("host certificate fingerprint mismatch".into()));
            }
        }
        if !is_p256_cert(end_entity) {
            return Err(TlsError::General("host certificate must be an ECDSA P-256 certificate".into()));
        }
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, TlsError> {
        Err(TlsError::General("TLS 1.2 is not supported".into()))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, TlsError> {
        verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ECDSA_NISTP256_SHA256]
    }
}

/// Client side, for tests and the fake phone. `expected_host_fp = None` means trust on first use.
/// Fails only if the supplied key/cert are unusable. Use any server name when connecting.
pub fn client_config(
    client_cert_der: Vec<u8>,
    client_key_der: Vec<u8>,
    expected_host_fp: Option<Fingerprint>,
) -> Result<ClientConfig> {
    let p = provider();
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(client_key_der));
    ClientConfig::builder_with_provider(p.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| TransportError::Tls(e.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinVerifier { provider: p, expected: expected_host_fp }))
        .with_client_auth_cert(vec![CertificateDer::from(client_cert_der)], key)
        .map_err(|e| TransportError::Tls(e.to_string()))
}

/// Generate a phone-style identity: `(cert_der, pkcs8_key_der)`, ECDSA P-256, self-signed.
pub fn generate_client_identity(name: &str) -> Result<(Vec<u8>, Vec<u8>)> {
    let (cert, key, _) = crate::identity::generate_p256(name, false)?;
    Ok((cert, key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn p256_detection() {
        let (cert, _) = generate_client_identity("phone").unwrap();
        assert!(is_p256_cert(&cert));
        assert!(!is_p256_cert(b"not a cert"));
        assert!(!is_p256_cert(&cert[..cert.len() - 1]));
        let mut trailing = cert.clone();
        trailing.push(0);
        assert!(!is_p256_cert(&trailing));
    }

    #[test]
    fn p256_detection_rejects_ed25519() {
        let kp = rcgen::KeyPair::generate_for(&rcgen::PKCS_ED25519).unwrap();
        let params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        let cert = params.self_signed(&kp).unwrap();
        assert!(!is_p256_cert(cert.der()));
    }

    #[test]
    fn verifier_wants_mandatory_client_auth() {
        let v = AnyP256ClientVerifier { provider: provider() };
        assert!(v.client_auth_mandatory() && v.offer_client_auth());
        assert_eq!(v.supported_verify_schemes(), vec![SignatureScheme::ECDSA_NISTP256_SHA256]);
        let (ed, _) = {
            let kp = rcgen::KeyPair::generate_for(&rcgen::PKCS_ED25519).unwrap();
            let c = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap().self_signed(&kp).unwrap();
            (c.der().to_vec(), ())
        };
        let now = UnixTime::now();
        assert!(v.verify_client_cert(&CertificateDer::from(ed), &[], now).is_err());
        let (ok, _) = generate_client_identity("p").unwrap();
        assert!(v.verify_client_cert(&CertificateDer::from(ok), &[], now).is_ok());
    }

    #[test]
    fn pin_verifier_checks_fingerprint() {
        let (cert, _) = generate_client_identity("h").unwrap();
        let der = CertificateDer::from(cert);
        let name = ServerName::try_from("notideck").unwrap();
        let now = UnixTime::now();
        let tofu = PinVerifier { provider: provider(), expected: None };
        assert!(tofu.verify_server_cert(&der, &[], &name, &[], now).is_ok());
        let good = PinVerifier { provider: provider(), expected: Some(Fingerprint::of_der(&der)) };
        assert!(good.verify_server_cert(&der, &[], &name, &[], now).is_ok());
        let bad = PinVerifier { provider: provider(), expected: Some(Fingerprint([0; 32])) };
        assert!(bad.verify_server_cert(&der, &[], &name, &[], now).is_err());
    }
}
