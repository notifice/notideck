// SPDX-License-Identifier: GPL-3.0-or-later
//! Pairing primitives shared by the host and the fake phone (ADR 0004 §4).
//!
//! * [`Fingerprint`]: SHA-256 of a certificate's DER bytes.
//! * [`PairingUri`]: the `notideck://pair?...` link shown as a QR code.
//! * [`compute_proof`] / [`verify_proof`]: the HMAC that binds the one-time secret to both certs.
//!
//! Test vectors: `docs/pairing-vectors.json` (the Android app tests against the same file).

use std::fmt;
use std::net::IpAddr;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// Domain separation prefix of the pairing proof.
pub const PROOF_CONTEXT: &[u8] = b"notideck-pair-v1";
/// Raw length of the one-time pairing secret.
pub const SECRET_LEN: usize = 16;
/// Lifetime of a pairing ticket.
pub const TICKET_TTL_SECS: u64 = 300;
/// How long an unknown LAN client may take to send `PairRequest` after `Hello`.
pub const PAIR_REQUEST_TIMEOUT_SECS: u64 = 30;
/// How long an unknown USB client waits for Allow/Deny on the PC.
pub const USB_APPROVAL_TIMEOUT_SECS: u64 = 120;
/// mDNS service type advertised by the host.
pub const MDNS_SERVICE_TYPE: &str = "_notideck._tcp.local.";

/// SHA-256 of a DER certificate. `Display` is lowercase hex.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Fingerprint(pub [u8; 32]);

impl Fingerprint {
    pub fn of_der(cert_der: &[u8]) -> Self {
        Fingerprint(Sha256::digest(cert_der).into())
    }

    pub fn to_hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }

    pub fn from_hex(s: &str) -> Option<Self> {
        if s.len() != 64 || !s.is_ascii() {
            return None;
        }
        let mut out = [0u8; 32];
        for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
            out[i] = u8::from_str_radix(std::str::from_utf8(chunk).ok()?, 16).ok()?;
        }
        Some(Fingerprint(out))
    }

    /// base64url without padding (the `f` URI parameter).
    pub fn to_b64url(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.0)
    }

    pub fn from_b64url(s: &str) -> Option<Self> {
        let v = URL_SAFE_NO_PAD.decode(s).ok()?;
        Some(Fingerprint(v.try_into().ok()?))
    }

    /// First 8 hex chars, for the mDNS TXT `fp8` field and short UI display.
    pub fn short(&self) -> String {
        self.to_hex()[..8].to_string()
    }
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Fingerprint({})", self.short())
    }
}

impl serde::Serialize for Fingerprint {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}

impl<'de> serde::Deserialize<'de> for Fingerprint {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Fingerprint::from_hex(&s).ok_or_else(|| serde::de::Error::custom("bad fingerprint hex"))
    }
}

/// `HMAC-SHA256(secret, "notideck-pair-v1" || client_fp || host_fp)`.
pub fn compute_proof(secret: &[u8; SECRET_LEN], client_fp: &Fingerprint, host_fp: &Fingerprint) -> [u8; 32] {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(secret).expect("HMAC takes any key length");
    mac.update(PROOF_CONTEXT);
    mac.update(&client_fp.0);
    mac.update(&host_fp.0);
    mac.finalize().into_bytes().into()
}

/// Constant-time check of a `PairRequest.proof`.
pub fn verify_proof(secret: &[u8; SECRET_LEN], client_fp: &Fingerprint, host_fp: &Fingerprint, proof: &[u8]) -> bool {
    let expected = compute_proof(secret, client_fp, host_fp);
    proof.len() == expected.len() && bool::from(expected.ct_eq(proof))
}

/// A fresh random pairing secret.
pub fn new_secret() -> [u8; SECRET_LEN] {
    rand::random()
}

/// Errors from [`PairingUri::parse`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UriError {
    #[error("not a notideck://pair link")]
    Scheme,
    #[error("unsupported pairing link version {0}")]
    Version(String),
    #[error("missing parameter `{0}`")]
    Missing(&'static str),
    #[error("bad parameter `{0}`")]
    Bad(&'static str),
}

/// `notideck://pair?v=1&h=<host_id>&n=<host_name>&a=<ip,ip>&p=<port>&f=<fp_b64url>&s=<secret_b64url>&e=<unix_expiry>`
///
/// Values are percent-encoded (RFC 3986 unreserved characters stay as they are); the
/// addresses in `a` are encoded one by one and joined with a literal `,`.
/// [`PairingUri::to_uri`] always writes the parameters in the order above; the parser
/// accepts any order and ignores unknown parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingUri {
    pub host_id: String,
    pub host_name: String,
    pub addrs: Vec<IpAddr>,
    pub port: u16,
    pub host_fp: Fingerprint,
    pub secret: [u8; SECRET_LEN],
    /// Unix seconds.
    pub expires_at: u64,
}

impl PairingUri {
    pub const PREFIX: &'static str = "notideck://pair?";

    pub fn to_uri(&self) -> String {
        let addrs: Vec<String> = self.addrs.iter().map(|a| pct_encode(&a.to_string())).collect();
        format!(
            "{}v=1&h={}&n={}&a={}&p={}&f={}&s={}&e={}",
            Self::PREFIX,
            pct_encode(&self.host_id),
            pct_encode(&self.host_name),
            addrs.join(","),
            self.port,
            self.host_fp.to_b64url(),
            URL_SAFE_NO_PAD.encode(self.secret),
            self.expires_at
        )
    }

    pub fn parse(uri: &str) -> Result<Self, UriError> {
        let query = uri.strip_prefix(Self::PREFIX).ok_or(UriError::Scheme)?;
        let (mut v, mut h, mut n, mut a, mut p, mut f, mut s, mut e) = (None, None, None, None, None, None, None, None);
        for pair in query.split('&').filter(|x| !x.is_empty()) {
            let (k, val) = pair.split_once('=').unwrap_or((pair, ""));
            match k {
                "v" => v = Some(val),
                "h" => h = Some(val),
                "n" => n = Some(val),
                "a" => a = Some(val),
                "p" => p = Some(val),
                "f" => f = Some(val),
                "s" => s = Some(val),
                "e" => e = Some(val),
                _ => {}
            }
        }
        let v = v.ok_or(UriError::Missing("v"))?;
        if v != "1" {
            return Err(UriError::Version(v.to_string()));
        }
        let host_id = pct_decode(h.ok_or(UriError::Missing("h"))?).ok_or(UriError::Bad("h"))?;
        let host_name = pct_decode(n.ok_or(UriError::Missing("n"))?).ok_or(UriError::Bad("n"))?;
        let addrs = a
            .ok_or(UriError::Missing("a"))?
            .split(',')
            .filter(|x| !x.is_empty())
            .map(|x| pct_decode(x).and_then(|d| d.parse::<IpAddr>().ok()))
            .collect::<Option<Vec<_>>>()
            .ok_or(UriError::Bad("a"))?;
        let port = p.ok_or(UriError::Missing("p"))?.parse::<u16>().map_err(|_| UriError::Bad("p"))?;
        let host_fp = Fingerprint::from_b64url(f.ok_or(UriError::Missing("f"))?).ok_or(UriError::Bad("f"))?;
        let secret: [u8; SECRET_LEN] = URL_SAFE_NO_PAD
            .decode(s.ok_or(UriError::Missing("s"))?)
            .ok()
            .and_then(|b| b.try_into().ok())
            .ok_or(UriError::Bad("s"))?;
        let expires_at = e.ok_or(UriError::Missing("e"))?.parse::<u64>().map_err(|_| UriError::Bad("e"))?;
        if host_id.is_empty() {
            return Err(UriError::Bad("h"));
        }
        Ok(PairingUri { host_id, host_name, addrs, port, host_fp, secret, expires_at })
    }
}

fn pct_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn pct_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(serde::Deserialize)]
    struct Vectors {
        proofs: Vec<ProofVector>,
        uris: Vec<UriVector>,
        invalid_uris: Vec<String>,
    }

    #[derive(serde::Deserialize)]
    struct ProofVector {
        secret_b64url: String,
        client_cert_der_hex: String,
        host_cert_der_hex: String,
        client_fp_hex: String,
        host_fp_hex: String,
        proof_hex: String,
    }

    #[derive(serde::Deserialize)]
    struct UriVector {
        uri: String,
        host_id: String,
        host_name: String,
        addrs: Vec<String>,
        port: u16,
        host_fp_hex: String,
        secret_b64url: String,
        expires_at: u64,
    }

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    fn vectors() -> Vectors {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../docs/pairing-vectors.json");
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn proof_vectors() {
        for v in vectors().proofs {
            let client_fp = Fingerprint::of_der(&hex(&v.client_cert_der_hex));
            let host_fp = Fingerprint::of_der(&hex(&v.host_cert_der_hex));
            assert_eq!(client_fp.to_hex(), v.client_fp_hex);
            assert_eq!(host_fp.to_hex(), v.host_fp_hex);
            let secret: [u8; 16] = URL_SAFE_NO_PAD.decode(&v.secret_b64url).unwrap().try_into().unwrap();
            let proof = compute_proof(&secret, &client_fp, &host_fp);
            assert_eq!(hex(&v.proof_hex), proof);
            assert!(verify_proof(&secret, &client_fp, &host_fp, &proof));
            assert!(!verify_proof(&secret, &host_fp, &client_fp, &proof), "fp order matters");
            assert!(!verify_proof(&secret, &client_fp, &host_fp, &proof[..31]));
        }
    }

    #[test]
    fn uri_vectors() {
        for v in vectors().uris {
            let parsed = PairingUri::parse(&v.uri).unwrap();
            assert_eq!(parsed.host_id, v.host_id);
            assert_eq!(parsed.host_name, v.host_name);
            assert_eq!(parsed.addrs.iter().map(|a| a.to_string()).collect::<Vec<_>>(), v.addrs);
            assert_eq!(parsed.port, v.port);
            assert_eq!(parsed.host_fp.to_hex(), v.host_fp_hex);
            assert_eq!(URL_SAFE_NO_PAD.encode(parsed.secret), v.secret_b64url);
            assert_eq!(parsed.expires_at, v.expires_at);
            assert_eq!(parsed.to_uri(), v.uri, "canonical form round-trips");
        }
    }

    #[test]
    fn invalid_uris() {
        for u in vectors().invalid_uris {
            assert!(PairingUri::parse(&u).is_err(), "{u}");
        }
    }

    #[test]
    fn fingerprint_formats() {
        let fp = Fingerprint::of_der(b"x");
        assert_eq!(Fingerprint::from_hex(&fp.to_hex()), Some(fp));
        assert_eq!(Fingerprint::from_b64url(&fp.to_b64url()), Some(fp));
        assert_eq!(fp.short().len(), 8);
        assert_eq!(serde_json::to_string(&fp).unwrap(), format!("\"{}\"", fp.to_hex()));
    }
}
