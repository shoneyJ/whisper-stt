//! Voice detection and mic level for a live capture.
//!
//! Replaces the old fixed-threshold check (`mean |s| < 0.003`), which had no
//! idea how loud *this* microphone or room is: steady background noise above
//! 0.003 read as endless speech, so no pause was ever found and nothing was
//! committed until stop. This tracker learns the input's noise floor instead,
//! and ignores clicks and DC offset.
//!
//! The algorithm follows Canonical Myna's voice tracker (`myna-audio/src/
//! voice.rs`, itself from murmure's `AdaptiveVad`); this is an independent
//! implementation of the same rules, not a copy:
//!
//! - audio is measured in 20 ms frames, after an 80 Hz high-pass that only
//!   feeds the measurement (DC offset or bias drift would otherwise read as
//!   one endless word); the PCM sent to Whisper is untouched
//! - an adaptive noise floor drops fast and rises slowly
//! - voice starts above `5 × floor` and continues above `3 × floor`, both
//!   clamped so a near-silent or very loud input still behaves
//! - only 200 ms of continuous voice counts, so a keystroke or cough doesn't
//!   reset the silence timer

/// Samples per analysis frame: 20 ms at 16 kHz.
pub const FRAME_SAMPLES: usize = 320;
const FRAME_MS: u64 = 20;

/// Voice must stay above the continue-threshold this long to count.
const SUSTAIN_MS: u64 = 200;

const HIGH_PASS_HZ: f32 = 80.0;

const INITIAL_FLOOR: f32 = 0.003;
/// Clamp for the start threshold. The continue threshold uses 0.6 × these.
const SPEECH_MIN: f32 = 0.004;
const SPEECH_MAX: f32 = 0.08;
const CONTINUE_RATIO: f32 = 0.6;

/// One capture session's voice state. Feed it PCM in order with [`push`].
///
/// [`push`]: VoiceTracker::push
#[derive(Debug, Clone)]
pub struct VoiceTracker {
    floor: f32,
    smoothed: Option<f32>,
    started: bool,
    active_ms: u64,
    position_ms: u64,
    last_voice_ms: Option<u64>,
    high_pass: HighPass,
    /// Samples of a partial frame carried to the next `push`.
    pending: Vec<f32>,
}

impl Default for VoiceTracker {
    fn default() -> Self {
        Self {
            floor: INITIAL_FLOOR,
            smoothed: None,
            started: false,
            active_ms: 0,
            position_ms: 0,
            last_voice_ms: None,
            high_pass: HighPass::new(16_000),
            pending: Vec::with_capacity(FRAME_SAMPLES),
        }
    }
}

impl VoiceTracker {
    /// Measure newly captured samples (16 kHz mono).
    pub fn push(&mut self, samples: &[i16]) {
        for &s in samples {
            let x = self.high_pass.filter(s as f32 / 32768.0);
            self.pending.push(x);
            if self.pending.len() == FRAME_SAMPLES {
                let rms = rms(&self.pending);
                self.pending.clear();
                self.observe_frame(rms);
            }
        }
    }

    fn observe_frame(&mut self, rms: f32) {
        self.position_ms += FRAME_MS;

        // Floor: follow quiet frames quickly, louder ones very slowly, and
        // ignore frames far above it (that's speech, not noise).
        if rms < self.floor {
            self.floor = 0.2 * rms + 0.8 * self.floor;
        } else if rms <= 10.0 * self.floor.max(SPEECH_MIN / 5.0) {
            self.floor = 0.005 * rms + 0.995 * self.floor;
        }

        let smoothed = self.smoothed.map_or(rms, |prev| 0.3 * rms + 0.7 * prev);
        self.smoothed = Some(smoothed);

        let start = (5.0 * self.floor).clamp(SPEECH_MIN, SPEECH_MAX);
        let keep =
            (3.0 * self.floor).clamp(SPEECH_MIN * CONTINUE_RATIO, SPEECH_MAX * CONTINUE_RATIO);
        if smoothed > start {
            self.started = true;
        }
        if self.started && smoothed >= keep {
            self.active_ms += FRAME_MS;
            if self.active_ms >= SUSTAIN_MS {
                self.last_voice_ms = Some(self.position_ms);
            }
        } else {
            self.active_ms = 0;
        }
    }

    /// Audio measured so far, in ms.
    pub fn position_ms(&self) -> u64 {
        self.position_ms
    }

    /// Capture position at the end of the last sustained voice, if any.
    pub fn last_voice_ms(&self) -> Option<u64> {
        self.last_voice_ms
    }
}

/// One-pole high-pass, starting from the first sample so an offset present
/// from the start doesn't ring through as a step.
#[derive(Debug, Clone)]
struct HighPass {
    pole: f32,
    prev: Option<(f32, f32)>,
}

impl HighPass {
    fn new(sample_rate: u32) -> Self {
        Self {
            pole: (-2.0 * std::f32::consts::PI * HIGH_PASS_HZ / sample_rate as f32).exp(),
            prev: None,
        }
    }

    fn filter(&mut self, x: f32) -> f32 {
        let (prev_in, prev_out) = self.prev.unwrap_or((x, 0.0));
        let y = self.pole * (prev_out + x - prev_in);
        self.prev = Some((x, y));
        y
    }
}

fn rms(frame: &[f32]) -> f32 {
    if frame.is_empty() {
        return 0.0;
    }
    (frame.iter().map(|x| x * x).sum::<f32>() / frame.len() as f32).sqrt()
}

/// RMS and peak of raw PCM, linear full scale, for the level meter.
pub fn level(samples: &[i16]) -> (f32, f32) {
    if samples.is_empty() {
        return (0.0, 0.0);
    }
    let mut sum = 0.0f32;
    let mut peak = 0.0f32;
    for &s in samples {
        let x = s as f32 / 32768.0;
        sum += x * x;
        peak = peak.max(x.abs());
    }
    ((sum / samples.len() as f32).sqrt().min(1.0), peak.min(1.0))
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// `ms` of a sine at `amp` (full scale), 16 kHz.
    pub fn tone(ms: u64, hz: f32, amp: f32) -> Vec<i16> {
        let n = (ms * 16) as usize;
        (0..n)
            .map(|i| {
                let t = i as f32 / 16_000.0;
                (amp * (2.0 * std::f32::consts::PI * hz * t).sin() * 32767.0) as i16
            })
            .collect()
    }

    pub fn silence(ms: u64) -> Vec<i16> {
        vec![0; (ms * 16) as usize]
    }

    #[test]
    fn digital_silence_is_never_voice() {
        let mut v = VoiceTracker::default();
        v.push(&silence(3_000));
        assert_eq!(v.last_voice_ms(), None);
        assert_eq!(v.position_ms(), 3_000);
    }

    #[test]
    fn sustained_speech_level_tone_counts_as_voice() {
        let mut v = VoiceTracker::default();
        v.push(&silence(500));
        v.push(&tone(1_000, 300.0, 0.1));
        let last = v.last_voice_ms().expect("voice heard");
        assert!(last > 1_000, "voice should last into the tone, got {last}");
    }

    #[test]
    fn a_click_shorter_than_sustain_is_not_voice() {
        let mut v = VoiceTracker::default();
        v.push(&silence(500));
        v.push(&tone(20, 300.0, 0.07)); // one loud 20 ms frame (~0.05 RMS)
        v.push(&silence(1_000));
        assert_eq!(v.last_voice_ms(), None);
    }

    #[test]
    fn a_constant_dc_offset_is_not_voice() {
        let mut v = VoiceTracker::default();
        v.push(&vec![8_000i16; 16_000 * 2]);
        assert_eq!(v.last_voice_ms(), None);
    }

    #[test]
    fn steady_room_noise_is_not_voice_but_speech_over_it_is() {
        // ~0.02 RMS hum: the old fixed threshold called this speech forever.
        let noise = |ms| tone(ms, 300.0, 0.028);
        let mut v = VoiceTracker::default();
        v.push(&noise(3_000));
        // Loud from the first frame, it reads as voice until the floor has
        // adapted (about a second), then no longer.
        let settled = v.last_voice_ms().unwrap_or(0);
        assert!(
            settled < 1_500,
            "noise kept counting as voice until {settled}"
        );
        v.push(&tone(1_000, 300.0, 0.14)); // speech ~0.1 RMS
        v.push(&noise(1_000));
        let last = v.last_voice_ms().expect("speech heard");
        assert!(
            (4_000..4_400).contains(&last),
            "voice should end soon after the speech, got {last}"
        );
    }

    #[test]
    fn level_reports_rms_and_peak() {
        let (rms, peak) = level(&tone(100, 300.0, 0.5));
        assert!((peak - 0.5).abs() < 0.01, "peak {peak}");
        assert!((rms - 0.5 / 2f32.sqrt()).abs() < 0.01, "rms {rms}");
        assert_eq!(level(&[]), (0.0, 0.0));
    }
}
