// SPDX-License-Identifier: GPL-3.0-or-later
//! `NOTIDECK_FAKE_MEDIA=1` (dev / demo screenshots only): a fake player with a made-up track, generated
//! cover art and a moving position. Its player id is `Spotify.exe`, so `{by: media}` matches the
//! `NOTIDECK_FAKE_MIXER` Spotify app.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use nd_platform::{MediaCommand, MediaInfo, NowPlaying, PlatformError, RepeatMode, Result};

const LENGTH_S: f64 = 225.0;
const TRACKS: [(&str, &str); 3] =
    [("Northern Lights", "Notideck Demo"), ("Paper Planes", "Notideck Demo"), ("Low Tide", "Notideck Demo")];

struct State {
    track: usize,
    playing: bool,
    /// Position at `since`.
    pos: f64,
    since: Instant,
    shuffle: bool,
    repeat: RepeatMode,
}

pub struct FakeMedia {
    state: Mutex<State>,
    art: Vec<Arc<[u8]>>,
}

impl FakeMedia {
    pub fn new() -> Self {
        let state = State {
            track: 0,
            playing: true,
            pos: 83.0,
            since: Instant::now(),
            shuffle: false,
            repeat: RepeatMode::List,
        };
        let art = (0..TRACKS.len()).map(|i| Arc::from(cover(i))).collect();
        Self { state: Mutex::new(state), art }
    }
}

impl Default for FakeMedia {
    fn default() -> Self {
        Self::new()
    }
}

impl State {
    fn position(&self) -> f64 {
        let p = if self.playing { self.pos + self.since.elapsed().as_secs_f64() } else { self.pos };
        p % LENGTH_S
    }
    fn seek(&mut self, p: f64) {
        self.pos = p.clamp(0.0, LENGTH_S);
        self.since = Instant::now();
    }
    fn skip(&mut self, by: isize) {
        self.track = (self.track as isize + by).rem_euclid(TRACKS.len() as isize) as usize;
        self.seek(0.0);
    }
}

impl MediaInfo for FakeMedia {
    fn backend_name(&self) -> &'static str {
        "fake"
    }

    fn now_playing(&self) -> Result<Option<NowPlaying>> {
        let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let pos = s.position();
        let (title, artist) = TRACKS[s.track];
        Ok(Some(NowPlaying {
            title: title.into(),
            artist: artist.into(),
            album: "Demo Album".into(),
            playing: s.playing,
            progress: Some((pos / LENGTH_S) as f32),
            position_s: Some(pos),
            length_s: Some(LENGTH_S),
            app: "Spotify.exe".into(),
            art: Some(self.art[s.track].clone()),
            shuffle: Some(s.shuffle),
            repeat: Some(s.repeat),
            can_seek: true,
        }))
    }

    fn control(&self, cmd: MediaCommand) -> Result<()> {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let pos = s.position();
        match cmd {
            MediaCommand::PlayPause => {
                s.playing = !s.playing;
                s.seek(pos);
            }
            MediaCommand::Play => {
                s.playing = true;
                s.seek(pos);
            }
            MediaCommand::Pause | MediaCommand::Stop => {
                s.playing = false;
                s.seek(pos);
            }
            MediaCommand::Next => s.skip(1),
            MediaCommand::Prev => s.skip(-1),
            MediaCommand::Seek { position_s } if position_s.is_finite() => s.seek(position_s),
            MediaCommand::SeekBy { delta_s } if delta_s.is_finite() => s.seek(pos + delta_s),
            MediaCommand::Seek { .. } | MediaCommand::SeekBy { .. } => {
                return Err(PlatformError::Other("invalid seek position".into()))
            }
            MediaCommand::Shuffle { on } => s.shuffle = on,
            MediaCommand::Repeat { mode } => s.repeat = mode,
        }
        Ok(())
    }
}

/// 256x256 PNG cover: a diagonal two-colour gradient with soft rings, one palette per track.
fn cover(i: usize) -> Vec<u8> {
    const N: u32 = 256;
    let palettes = [([38, 70, 140], [236, 120, 90]), ([30, 110, 90], [240, 210, 110]), ([90, 40, 110], [80, 190, 220])];
    let (a, b) = palettes[i % palettes.len()];
    let mut px = Vec::with_capacity((N * N * 3) as usize);
    for y in 0..N {
        for x in 0..N {
            let t = (x + y) as f32 / (2 * N - 2) as f32;
            let (dx, dy) = (x as f32 - 170.0, y as f32 - 90.0);
            let ring = ((dx * dx + dy * dy).sqrt() / 14.0).sin() * 0.5 + 0.5;
            let glow = 1.0 + 0.18 * ring * (1.0 - t);
            for c in 0..3 {
                let v = (a[c] as f32 * (1.0 - t) + b[c] as f32 * t) * glow;
                px.push(v.clamp(0.0, 255.0) as u8);
            }
        }
    }
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, N, N);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header().expect("png header");
    w.write_image_data(&px).expect("png data");
    w.finish().expect("png finish");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_player_reports_and_obeys() {
        let m = FakeMedia::new();
        let np = m.now_playing().unwrap().unwrap();
        assert_eq!(np.title, "Northern Lights");
        assert_eq!(&np.art.as_ref().unwrap()[1..4], b"PNG");
        m.control(MediaCommand::Next).unwrap();
        m.control(MediaCommand::Seek { position_s: 10.0 }).unwrap();
        m.control(MediaCommand::Shuffle { on: true }).unwrap();
        let np = m.now_playing().unwrap().unwrap();
        assert_eq!(np.title, "Paper Planes");
        assert!(np.position_s.unwrap() >= 10.0 && np.position_s.unwrap() < 12.0);
        assert_eq!(np.shuffle, Some(true));
    }
}
