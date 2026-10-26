# ADR 0004: LAN mode, pairing and session security

Status: accepted (2026-10-12). Refines SPEC §4.2–§4.4.

## Context
Until now the phone could only reach the PC through `adb reverse`, which needs USB debugging and a
cable (or fragile wireless debugging). M3 adds Wi-Fi. Opening the control port to the network means
anyone on the LAN could try to inject keystrokes, so the connection must be authenticated. A
second, existing risk: any app on the phone can open `127.0.0.1:47800` while `adb reverse` is
active and drive the host. So USB needs authentication too.

## Decision
1. **Every control session is TLS with mutual authentication**, over USB and LAN alike.
   - One listener on `0.0.0.0:<control port>` serves both. A loopback peer means USB (`adb reverse`).
   - TLS 1.3 everywhere. Android 9 (minSdk 28) has no platform TLS 1.3, so the app bundles
     **Conscrypt** as its TLS provider.
2. **Keys: ECDSA P-256 self-signed certs on both sides** (not Ed25519: the Android KeyStore only has
   Ed25519 from API 33).
   - Host: generated on first run (`rcgen`), stored as `host-identity.pem` in the config dir
     (user-only permissions).
   - Phone: generated in the **AndroidKeyStore** (non-exportable).
   - Identity = SHA-256 of the DER certificate ("fingerprint").
3. **Pinning:**
   - The host keeps `devices.json` (device_id, name, cert fingerprint, paired_at, last_seen,
     transport_hint). Unknown client certs only get the pairing handshake.
   - The phone keeps the host's id, name, fingerprint and last addresses.
4. **Pairing over Wi-Fi (QR):**
   - The Devices tab shows a QR code for
     `notideck://pair?v=1&h=<host_id>&n=<host_name>&a=<ip,ip>&p=<port>&f=<host_fp_b64url>&s=<secret_b64url>&e=<unix_expiry>`.
     The secret is 128-bit random, single use, and expires after 5 minutes.
   - The phone scans the code with the in-app scanner, or with the system camera via an Android
     deep link for `notideck://pair`.
   - The phone connects with TLS, pinning `f`, and presenting its own cert. It sends
     `PairRequest{ proof = HMAC-SHA256(secret, "notideck-pair-v1" || client_fp || host_fp), device_name }`.
     The host verifies the proof, pins the client fp and answers `PairResult`.
   - The proof binds the secret to both certificates, so a MITM can't relay it.
5. **Pairing over USB (no camera needed):**
   - An unknown phone connecting over loopback gets a pending state. The PC app shows "New phone:
     Xperia XZ wants to connect — Allow / Deny" (Devices tab + tray notification).
   - Allow pins its cert; the phone pins the host cert on first USB contact (trust on first use;
     acceptable because the channel is the adb tunnel).
   - After that, the same phone also connects over Wi-Fi with no QR.
6. **6-digit code pairing is deferred.** Doing it safely needs a PAKE (SPAKE2), and Android has no
   ready Java API for that. QR and USB cover the cases for now.
7. **Discovery:**
   - The host advertises `_notideck._tcp` with mDNS (`mdns-sd`), TXT `id`, `name`, `proto`, `fp8`
     (first 8 hex chars of the fingerprint).
   - The phone resolves through `NsdManager`, plus the last known addresses. Manual IP entry is
     allowed only for an already paired host, still pinned.
8. **Failover:**
   - The phone tries USB (`127.0.0.1:47800`) first, then the last paired host's addresses, then
     mDNS.
   - While on LAN it probes USB every 5 s and switches over when it becomes available.
   - The host keeps one session per device: a new USB session supersedes the LAN one
     (`GOODBYE_REASON_SUPERSEDED`).
9. **Off switch:** setting `lan_enabled` (default on). Off = bind loopback only.
   - The Windows Firewall prompt appears on the first LAN bind. The app explains it in Settings
     and the Devices tab, and never edits firewall rules itself (that needs admin).
10. **Revocation:** removing a device in the app deletes its pin and closes its session at once.
11. **Protocol:** `PROTO_VERSION = 2`. v1 phones (plaintext) are refused with a clear Goodbye
    message ("update the Notideck app"). Pre-1.0, so no compatibility shim.

## Consequences
- Old APKs stop working with the new host; both sides ship together.
- The fake phone and the integration tests must speak TLS and pairing.
- APK grows by about 4 MB (Conscrypt + ZXing core); there is no Google Play Services dependency, so
  F-Droid stays possible.
