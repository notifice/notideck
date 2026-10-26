# ADR 0005: Media channel and Windows virtual camera

Status: accepted (2026-10-12). Refines SPEC §4.5 and §7 for M7 (and the shared parts of M6).

## Context
The phone streams its camera as H.264. On Windows 11 the stream must appear as a normal webcam
("Notideck Camera") in Zoom, Teams, Discord, OBS and the Camera app. Windows 11 has
`MFCreateVirtualCamera`, but the media source behind it is a COM DLL that the **Windows Camera Frame
Server** service loads, not our process. The developer works in WSL: Windows code is cross-compiled
(`x86_64-pc-windows-gnu`) and its tests run natively through WSL interop, but nothing that needs
admin rights or a click can be automated.

## Decision
1. **Media connection = a second TLS connection to the same port.** After the TLS handshake (same
   client cert as the control session) the phone sends `MediaAttach{session_id}` instead of `Hello`.
   The host checks that a live session with that id has the same cert fingerprint, answers
   `MediaAttached{ok}`, and from then on the connection carries only phone→host media packets
   (12-byte header + H.264 Annex-B, `nd_proto::media`). One port means one `adb reverse` and one
   firewall rule. A new attach replaces the old media connection of that session.
2. **Flow:** host sends `CameraStart` → phone reports `CameraStatus{STARTING}`, opens the media
   connection, sends a CONFIG packet (SPS+PPS) and frames, then `CameraStatus{STREAMING}`. Keyframe
   every 2 s and on `CameraControl.request_keyframe` (the host asks after attach and after decoder
   errors). `CameraStop` closes the media connection. PROTO_VERSION stays 2 (additive; the camera is
   announced by `Capability.CAMERA` and `CameraStatus.supported_modes`).
3. **Decode on the host in the app process**, not in the camera DLL:
   - Windows: the OS's Media Foundation H.264 decoder MFT (licensed by the OS, hardware-capable),
     NV12 output.
   - Linux (M6) and tests: `openh264` (built from source) behind the same `nd_media::H264Decoder` trait.
   - The app scales/crops the decoded NV12 to the size the camera consumer selected.
4. **Virtual camera DLL (`nd-vcam-dll`, Rust cdylib, `windows` crate)** implements the media source
   (`IMFMediaSourceEx` + `IMFMediaStream2` + `IKsControl` + attributes), modelled on Microsoft's
   VirtualCamera sample / smourier's VCamSample. It is deliberately dumb: it advertises NV12
   1920×1080, 1280×720 and 640×360 at 30 fps, and serves frames from shared memory. With no fresh frame
   (producer heartbeat older than 1 s) it serves a built-in "Phone not connected" placeholder, so
   apps keep their camera selection.
5. **Shared memory between app and DLL** (`nd_vcam::shm`, layout below). The DLL runs as
   *Local Service*, which may create `Global\` objects, while a non-elevated user app may not. So
   **the DLL creates** `Global\NotideckVirtualCamera-v1` with a DACL that grants SYSTEM, Local Service
   and Interactive Users read/write, and **the app opens it**. A single frame slot guarded by a
   seqlock (`frame_seq` odd while writing); the reader copies and retries/skips on a torn read.
   - The DLL sets `consumer_active` on stream Start/Stop and bumps `consumer_heartbeat_ms` each sample.
     The app polls for the mapping at 4 Hz; a consumer becoming active is what auto-starts the phone
     camera (SPEC §7.3), and it stops 5 s after the consumer goes away.
6. **Registration needs admin once.** The Frame Server reads COM registrations from HKLM and must be
   able to read the DLL file (user profile folders are not readable by Local Service). So
   `Notideck.exe --install-vcam` (started elevated through a UAC prompt by the app's "Fix" button)
   copies `notideck_vcam.dll` to `%ProgramFiles%\Notideck\VirtualCamera\` and registers
   `HKLM\SOFTWARE\Classes\CLSID\{CLSID}\InprocServer32` (ThreadingModel `Both`).
   `--uninstall-vcam` reverses it. Without it, the app shows a "Webcam not installed" problem.
   A newer DLL in the app folder than the installed one is also reported ("update the webcam").
7. **Camera lifetime:** the running app calls `MFCreateVirtualCamera(SoftwareCameraSource, Session,
   CurrentUser, "Notideck Camera", {CLSID})` + `Start` and keeps the object. The camera exists while
   the app runs (autostart covers the normal case). A system-lifetime camera is post-v1.
8. **Testing from WSL:** the DLL is tested in-process by a Windows test exe that calls
   `DllGetClassObject`, creates the source, selects a type, starts it and pulls samples (placeholder
   first, then frames written through `nd_vcam::shm`), run through interop. The only manual steps for
   the user: approve the UAC prompt once, and check the camera in a real app.

## Shared memory layout (v1)
`#[repr(C)]`, little-endian, defined once in `host/crates/nd-vcam/src/shm.rs` and used by both sides.
Header (256 bytes reserved), then one NV12 frame slot of `MAX_FRAME_BYTES` (1920×1080×3/2).

| Field | Written by | Meaning |
|---|---|---|
| `magic` `u32` = `0x4356_444E` ("NDVC") / `version` `u32` = 1 | DLL | |
| `width`, `height`, `fps` `u32` | DLL | media type the consumer selected |
| `consumer_active` `u32` | DLL | 1 between stream Start and Stop |
| `consumer_heartbeat_ms` `u64` | DLL | unix ms, updated on every sample request |
| `producer_heartbeat_ms` `u64` | app | unix ms, updated on every frame (and at 1 Hz when idle) |
| `frame_seq` `u64` | app | seqlock: odd while writing, even when complete |
| `frame_width`, `frame_height` `u32` | app | must equal `width`/`height`, else the DLL ignores the frame |
| `frame_pts_us` `u64` | app | phone PTS, for diagnostics |

## Consequences
- Windows N editions lack the H.264 decoder ("Media Feature Pack"); the app reports it as a problem.
- One UAC prompt per install/update of the DLL.
- Latency budget (<150 ms USB): phone encode ~30 ms + transport + MF decode ~10 ms + one frame of
  buffering in the source. No extra queues anywhere: latest frame wins.
