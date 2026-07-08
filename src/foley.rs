//! Procedural dice foley: the simulation's collision events, made audible.
//!
//! No samples, no assets — every sound is synthesized from the physics that
//! caused it: impact speed sets loudness, die size sets pitch (a d4 clicks, a
//! d20 thunks), the cup rattle follows the sway, and a natural 20 gets one
//! bright ring. Synthesis is pure (`synth`) so it's unit-testable; only the
//! thin [`Foley`] wrapper touches the audio output, and it degrades to
//! silence when there isn't one (ssh, CI, `--mute`).
//!
//! On macOS with a duplex default output (a USB interface with mic inputs),
//! the OS raises a one-time *microphone* prompt for any process that starts
//! playback — even Apple's `afplay`. That's the OS, not this code (no input
//! path exists here); the README's Sound section documents it, and the lazy
//! init in `main.rs::run` keeps `--mute` sessions from ever asking.

use rodio::buffer::SamplesBuffer;
use rodio::{DeviceSinkBuilder, MixerDeviceSink};

use crate::app::SoundEvent;

const RATE: u32 = 44_100;
/// Master gain: dice on a table, not a drum kit.
const MASTER: f32 = 0.38;
/// The speed at which an impact reaches full loudness (arena cells/second).
const SPEED_FULL: f32 = 90.0;

/// A live connection to the default audio output.
pub struct Foley {
    /// Keeping the sink alive keeps the device open; drop = silence.
    sink: MixerDeviceSink,
}

impl Foley {
    /// Open the default output device, or `None` when there isn't one — the
    /// dice just roll quietly.
    ///
    /// Deliberately NOT `open_default_sink()`: its fallback enumerates every
    /// audio device (microphones included), which is its own way to draw the
    /// macOS microphone prompt, and it eprintln!s over the TUI on failure.
    /// If the one default output device won't open, silence is the correct
    /// fallback.
    pub fn new() -> Option<Foley> {
        let mut sink = DeviceSinkBuilder::from_default_device()
            .ok()?
            .open_stream()
            .ok()?;
        // rodio logs to stderr on drop by default; in a TUI that's garbage
        // sprayed over the restored terminal.
        sink.log_on_drop(false);
        Some(Foley { sink })
    }

    /// Fire-and-forget: mix this event's sound into whatever is playing.
    pub fn play(&self, ev: SoundEvent) {
        let samples = synth(ev);
        if !samples.is_empty() {
            let mono = std::num::NonZero::<u16>::MIN; // 1 channel
            let rate = std::num::NonZero::new(RATE).expect("RATE is non-zero");
            self.sink
                .mixer()
                .add(SamplesBuffer::new(mono, rate, samples));
        }
    }
}

/// A die's voice: smaller dice click higher, big dice knock deeper.
fn pitch(sides: u32) -> f32 {
    1500.0 / (sides.max(2) as f32).powf(0.45)
}

/// Impact speed → loudness, saturating: past SPEED_FULL everything is a slam.
fn loudness(speed: f32) -> f32 {
    (speed / SPEED_FULL).clamp(0.12, 1.0)
}

/// Render one event to mono f32 samples at [`RATE`]. Pure and deterministic.
pub fn synth(ev: SoundEvent) -> Vec<f32> {
    match ev {
        SoundEvent::Impact { sides, speed } => knock(pitch(sides), loudness(speed), 0.05),
        // Dice striking each other are brighter than dice on the felt.
        SoundEvent::Knock { sides, speed } => knock(pitch(sides) * 1.35, loudness(speed), 0.04),
        SoundEvent::Settle { sides } => knock(pitch(sides) * 0.8, 0.28, 0.06),
        SoundEvent::Rattle { power } => rattle(0.25 + 0.55 * power),
        SoundEvent::Throw { power } => whoosh(0.4 + 0.6 * power),
        SoundEvent::Crit => chime(&[(1318.5, 1.0), (1975.5, 0.5)], 0.5, 0.7),
        SoundEvent::Fumble => thud(),
        SoundEvent::Success => melody(&[(660.0, 0.09), (880.0, 0.12)], 0.5),
        SoundEvent::Failure => melody(&[(233.1, 0.11), (185.0, 0.16)], 0.45),
    }
}

/// Deterministic white-ish noise (xorshift) so synthesis needs no RNG state.
struct Noise(u32);
impl Noise {
    fn next(&mut self) -> f32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        (x as f32 / u32::MAX as f32) * 2.0 - 1.0
    }
}

fn seconds(dur: f32) -> usize {
    (dur * RATE as f32) as usize
}

/// A die hitting something: a few ms of noise transient over a damped sine.
fn knock(freq: f32, gain: f32, dur: f32) -> Vec<f32> {
    let n = seconds(dur);
    let mut noise = Noise(0x9e3779b9);
    (0..n)
        .map(|i| {
            let t = i as f32 / RATE as f32;
            let tone = (std::f32::consts::TAU * freq * t).sin() * 0.75;
            let click = noise.next() * (-t * 900.0).exp() * 0.5;
            (tone + click) * (-t * 55.0).exp() * gain * MASTER
        })
        .collect()
}

/// One tick of the cup: two tiny clicks a few ms apart (dice against dice
/// against cup wall), brighter and louder as the shake powers up.
fn rattle(gain: f32) -> Vec<f32> {
    let n = seconds(0.05);
    let mut noise = Noise(0x2545f491);
    (0..n)
        .map(|i| {
            let t = i as f32 / RATE as f32;
            // Two decaying bursts: at 0ms and ~18ms.
            let a = (-t * 500.0).exp();
            let b = if t > 0.018 {
                (-(t - 0.018) * 500.0).exp()
            } else {
                0.0
            };
            let body = (std::f32::consts::TAU * 2600.0 * t).sin() * 0.3 + noise.next() * 0.7;
            body * (a + b * 0.8) * gain * MASTER * 0.6
        })
        .collect()
}

/// The release: a short breath of air, louder for a harder throw.
fn whoosh(gain: f32) -> Vec<f32> {
    let n = seconds(0.14);
    let mut noise = Noise(0x1f123bb5);
    let mut low = 0.0f32; // one-pole lowpass so it's a whoosh, not a hiss
    (0..n)
        .map(|i| {
            let t = i as f32 / RATE as f32;
            low += (noise.next() - low) * 0.12;
            // Swells in fast, dies out.
            let env = (t * 30.0).min(1.0) * (-t * 26.0).exp();
            low * env * gain * MASTER * 1.6
        })
        .collect()
}

/// A small bell of the given partials — the natural-20 ring.
fn chime(partials: &[(f32, f32)], dur: f32, gain: f32) -> Vec<f32> {
    let n = seconds(dur);
    (0..n)
        .map(|i| {
            let t = i as f32 / RATE as f32;
            let mut s = 0.0;
            for &(freq, amp) in partials {
                s += (std::f32::consts::TAU * freq * t).sin() * amp;
            }
            s * (-t * 7.0).exp() * gain * MASTER * 0.5
        })
        .collect()
}

/// The natural-1: a low, unglamorous thump.
fn thud() -> Vec<f32> {
    let n = seconds(0.2);
    (0..n)
        .map(|i| {
            let t = i as f32 / RATE as f32;
            // Pitch sags as it decays, like a dropped book.
            let freq = 95.0 - 30.0 * t;
            (std::f32::consts::TAU * freq * t).sin() * (-t * 22.0).exp() * MASTER * 0.9
        })
        .collect()
}

/// Two quick plucked notes — the staked verdict, up for success, down for not.
fn melody(notes: &[(f32, f32)], gain: f32) -> Vec<f32> {
    let mut out = Vec::new();
    for &(freq, dur) in notes {
        let n = seconds(dur);
        out.extend((0..n).map(|i| {
            let t = i as f32 / RATE as f32;
            (std::f32::consts::TAU * freq * t).sin() * (-t * 18.0).exp() * gain * MASTER
        }));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every event renders to a real, bounded buffer.
    #[test]
    fn every_event_synthesizes_in_range() {
        let events = [
            SoundEvent::Impact {
                sides: 6,
                speed: 40.0,
            },
            SoundEvent::Knock {
                sides: 20,
                speed: 25.0,
            },
            SoundEvent::Settle { sides: 12 },
            SoundEvent::Rattle { power: 0.8 },
            SoundEvent::Throw { power: 1.0 },
            SoundEvent::Crit,
            SoundEvent::Fumble,
            SoundEvent::Success,
            SoundEvent::Failure,
        ];
        for ev in events {
            let s = synth(ev);
            assert!(!s.is_empty(), "{ev:?} rendered nothing");
            assert!(
                s.iter().all(|v| v.abs() <= 1.0),
                "{ev:?} clips: max {}",
                s.iter().fold(0.0f32, |m, v| m.max(v.abs()))
            );
        }
    }

    /// Zero crossings ≈ 2·freq·dur: a d4's click must ring higher than a
    /// d100's knock, or the size-to-pitch mapping is broken.
    #[test]
    fn small_dice_click_higher_than_big_dice() {
        let crossings = |s: &[f32]| {
            s.windows(2)
                .filter(|w| w[0].signum() != w[1].signum())
                .count()
        };
        let d4 = synth(SoundEvent::Impact {
            sides: 4,
            speed: 50.0,
        });
        let d100 = synth(SoundEvent::Impact {
            sides: 100,
            speed: 50.0,
        });
        assert!(
            crossings(&d4) > crossings(&d100) * 2,
            "d4 {} vs d100 {} crossings",
            crossings(&d4),
            crossings(&d100)
        );
    }

    /// Not a real assertion — opens the actual output device and plays a short
    /// sequence so a human can hear the palette. Makes noise; run by hand:
    ///   cargo test audible -- --ignored --nocapture
    #[test]
    #[ignore]
    fn audible_smoke_test() {
        let foley = Foley::new().expect("no audio output device");
        let script = [
            SoundEvent::Rattle { power: 0.4 },
            SoundEvent::Rattle { power: 0.9 },
            SoundEvent::Throw { power: 1.0 },
            SoundEvent::Impact {
                sides: 20,
                speed: 70.0,
            },
            SoundEvent::Knock {
                sides: 20,
                speed: 40.0,
            },
            SoundEvent::Impact {
                sides: 6,
                speed: 30.0,
            },
            SoundEvent::Settle { sides: 20 },
            SoundEvent::Crit,
            SoundEvent::Success,
            SoundEvent::Fumble,
            SoundEvent::Failure,
        ];
        for ev in script {
            eprintln!("  ♪ {ev:?}");
            foley.play(ev);
            std::thread::sleep(std::time::Duration::from_millis(350));
        }
        std::thread::sleep(std::time::Duration::from_millis(600));
    }

    /// Harder impacts are louder, saturating at the cap.
    #[test]
    fn loudness_follows_impact_speed() {
        let peak = |s: Vec<f32>| s.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        let soft = peak(synth(SoundEvent::Impact {
            sides: 6,
            speed: 12.0,
        }));
        let hard = peak(synth(SoundEvent::Impact {
            sides: 6,
            speed: 80.0,
        }));
        assert!(
            hard > soft * 2.0,
            "hard {hard} not clearly louder than soft {soft}"
        );
    }
}
