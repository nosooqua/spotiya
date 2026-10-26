//! Alice's own ring and clock animations, read from the firmware so Spotify gestures look
//! the same as the native player's. Quasar offers no way to play one on request, so the
//! controls driver plays the frames itself on the same LEDs.

use serde::Deserialize;
use std::{fs, io::Read, path::Path};

/// Mirrors `struct ring_frame` in controls/input_config.h.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RingFrame {
    /// Red, green and blue for segments 1..=6, in that order.
    pub level: [u8; 18],
    pub ms: u16,
}

/// Mirrors `struct display_frame` in controls/input_config.h.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockFrame {
    /// Driver order: digit * 7 + segment A..G, then PLUS1..4, MINUS, DEGREE.
    pub level: [u8; CLOCK_LEDS],
    pub ms: u16,
}
const CLOCK_LEDS: usize = 34;
/// Quasar stores each digit as G, A, B, C, D, E, F; the driver uses A..G.
const DIGIT_SEGMENT: [usize; 7] = [6, 0, 1, 2, 3, 4, 5];
/// After the digits Quasar keeps MINUS first. MINUS was matched against what Quasar draws;
/// the colon pair follows it, as `ringing` lights them together.
const EXTRA_LED: [usize; 6] = [32, 28, 29, 30, 31, 33];

const MAX_FRAMES: usize = 512;
const MAX_FILE: u64 = 64 * 1024;

/// Quasar's per-body colour correction (`ledconfigs/*.json`).
#[derive(Deserialize)]
struct ColorConfig {
    r#type: String,
    matrix: [[f64; 3]; 3],
}
pub struct Correction([[f64; 3]; 3]);
impl Correction {
    pub fn load(path: &Path) -> Option<Self> {
        let config: ColorConfig = serde_json::from_slice(&read(path)?).ok()?;
        (config.r#type == "matrix" && config.matrix.iter().flatten().all(|v| v.is_finite()))
            .then_some(Self(config.matrix))
    }
}
/// What Quasar writes for a pattern colour, as measured on the ring: gamma 2.8 for patterns
/// marked `gammacorrection`, the body-colour matrix, then a common scale when a channel
/// overflows so the hue survives, truncated to whole levels.
fn shade(rgb: [u8; 3], correction: Option<&Correction>, gamma: bool) -> [u8; 3] {
    let linear = rgb.map(|v| {
        let v = f64::from(v);
        if gamma {
            255.0 * (v / 255.0).powf(2.8)
        } else {
            v
        }
    });
    let mixed = correction.map_or(linear, |c| {
        c.0.map(|row| {
            row.iter()
                .zip(linear)
                .map(|(m, v)| m * v)
                .sum::<f64>()
                .max(0.0)
        })
    });
    let peak = mixed.iter().copied().fold(0.0, f64::max);
    let scale = if peak > 255.0 { 255.0 / peak } else { 1.0 };
    mixed.map(|v| (v * scale).min(255.0) as u8)
}

fn read(path: &Path) -> Option<Vec<u8>> {
    let mut data = vec![];
    fs::File::open(path)
        .ok()?
        .take(MAX_FILE + 1)
        .read_to_end(&mut data)
        .ok()?;
    (data.len() as u64 <= MAX_FILE).then_some(data)
}

/// Parses a pattern: each line holds six `RRGGBB` segment colours and a frame length in
/// milliseconds. One-shot patterns take no directives; the volume palette is a looping,
/// gamma-corrected list of level colours, so `palette` accepts exactly those two.
fn parse(text: &str, correction: Option<&Correction>, palette: bool) -> Option<Vec<RingFrame>> {
    let mut frames = vec![];
    let mut gamma = false;
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        match line {
            "gammacorrection" if palette && frames.is_empty() => {
                gamma = true;
                continue;
            }
            "loop" if palette && frames.is_empty() => continue,
            _ => {}
        }
        let fields: Vec<&str> = line.split_ascii_whitespace().collect();
        let [colors @ .., ms] = fields.as_slice() else {
            return None;
        };
        if colors.len() != 6 || frames.len() == MAX_FRAMES {
            return None;
        }
        let ms = ms
            .parse::<u16>()
            .ok()
            .filter(|ms| (1..=5000).contains(ms))?;
        let mut level = [0; 18];
        for (segment, color) in colors.iter().enumerate() {
            if color.len() != 6 {
                return None;
            }
            let value = u32::from_str_radix(color, 16).ok()?;
            let rgb = [(value >> 16) as u8, (value >> 8) as u8, value as u8];
            let rgb = shade(rgb, correction, gamma);
            for (channel, v) in rgb.into_iter().enumerate() {
                level[channel * 6 + segment] = v;
            }
        }
        frames.push(RingFrame { level, ms });
    }
    (!frames.is_empty()).then_some(frames)
}

/// Decodes a `.binscled` clock animation: frames of 34 levels and a length in milliseconds.
fn parse_clock(data: &[u8]) -> Option<Vec<ClockFrame>> {
    if data.is_empty() || !data.len().is_multiple_of(CLOCK_LEDS + 1) {
        return None;
    }
    let frames: Vec<ClockFrame> = data
        .chunks(CLOCK_LEDS + 1)
        .map(|chunk| {
            let mut level = [0; CLOCK_LEDS];
            for (index, value) in chunk[..CLOCK_LEDS].iter().enumerate() {
                let led = if index < 28 {
                    index / 7 * 7 + DIGIT_SEGMENT[index % 7]
                } else {
                    EXTRA_LED[index - 28]
                };
                level[led] = *value;
            }
            ClockFrame {
                level,
                ms: u16::from(chunk[CLOCK_LEDS]),
            }
        })
        .collect();
    (frames.len() <= MAX_FRAMES && frames.iter().all(|f| f.ms > 0)).then_some(frames)
}
fn unpack(path: &Path) -> Option<Vec<u8>> {
    let packed = read(path)?;
    let mut decoder = ruzstd::StreamingDecoder::new(&packed[..]).ok()?;
    let mut data = vec![];
    (&mut decoder)
        .take(MAX_FILE + 1)
        .read_to_end(&mut data)
        .ok()?;
    (data.len() as u64 <= MAX_FILE).then_some(data)
}
#[derive(Default)]
pub struct Clock {
    pub next: Vec<ClockFrame>,
    pub previous: Vec<ClockFrame>,
    pub pause: Vec<ClockFrame>,
}
impl Clock {
    pub fn load(directory: &Path) -> Self {
        let animation = |name: &str| {
            unpack(&directory.join(format!("{name}.binscled.zst")))
                .and_then(|data| parse_clock(&data))
                .unwrap_or_default()
        };
        let clock = Self {
            next: animation("player_next_track"),
            previous: animation("player_previous_track"),
            pause: animation("player_pause"),
        };
        let loaded = [&clock.next, &clock.previous, &clock.pause]
            .iter()
            .filter(|frames| !frames.is_empty())
            .count();
        eprintln!("controls: {loaded} of 3 Alice clock animations loaded");
        clock
    }
}

/// Stock animations for the transport gestures; a missing or unusable file leaves its
/// slot empty, and the driver falls back to its own effect for that gesture.
#[derive(Default)]
pub struct Ring {
    pub next: Vec<RingFrame>,
    pub previous: Vec<RingFrame>,
    pub pause: Vec<RingFrame>,
    pub resume: Vec<RingFrame>,
    /// One uniform frame per volume level, quietest first.
    pub volume: Vec<RingFrame>,
}
impl Ring {
    pub fn load(directory: &Path, colors: Option<&Path>) -> Self {
        let correction = colors.and_then(Correction::load);
        if colors.is_some() && correction.is_none() {
            eprintln!("controls: ring colour correction unavailable; using pattern colours");
        }
        let pattern = |name: &str, palette: bool| {
            read(&directory.join(format!("{name}.led")))
                .and_then(|data| String::from_utf8(data).ok())
                .and_then(|text| parse(&text, correction.as_ref(), palette))
                .unwrap_or_default()
        };
        let ring = Self {
            next: pattern("player_next_track", false),
            previous: pattern("player_previous_track", false),
            pause: pattern("player_pause", false),
            resume: pattern("player_continue", false),
            volume: pattern("volume", true),
        };
        let loaded = [
            &ring.next,
            &ring.previous,
            &ring.pause,
            &ring.resume,
            &ring.volume,
        ]
        .iter()
        .filter(|frames| !frames.is_empty())
        .count();
        eprintln!("controls: {loaded} of 5 Alice ring animations loaded");
        ring
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Quasar's blue.json, and the levels it wrote for alice_listening's 6b0fff on a Mini 2.
    const BLUE: &str = r#"{"type":"matrix","matrix":[[2.79511386e+00,3.66642088e-01,1.35181910e-02],[6.11462216e-04,9.81815907e-01,2.18246348e-02],[5.74260011e-18,1.48951471e-02,4.98257329e-01]]}"#;
    fn blue() -> Correction {
        let dir = std::env::temp_dir().join(format!("spotiya-colors-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("blue.json"), BLUE).unwrap();
        let correction = Correction::load(&dir.join("blue.json")).unwrap();
        fs::remove_dir_all(dir).unwrap();
        correction
    }
    #[test]
    fn colour_correction_reproduces_what_quasar_writes() {
        let blue = blue();
        assert_eq!(
            shade([0x6b, 0x0f, 0xff], Some(&blue), false),
            [255, 16, 105]
        );
        assert_eq!(
            shade([0x6b, 0x0f, 0xfe], Some(&blue), false),
            [255, 16, 104]
        );
        assert_eq!(shade([0, 0, 0], Some(&blue), false), [0, 0, 0]);
        // Volume palette entries against the levels Quasar wrote (within its rounding).
        for (palette, written) in [
            ([0xe9, 0xec, 0x00], [255, 81, 1]),
            ([0x8d, 0xe5, 0x00], [206, 185, 2]),
            ([0x2d, 0x86, 0x24], [21, 42, 1]),
            ([0xf8, 0x00, 0x00], [255, 0, 0]),
        ] {
            let shaded = shade(palette, Some(&blue), true);
            for (a, b) in shaded.iter().zip(written) {
                assert!(a.abs_diff(b) <= 2, "{palette:?}: {shaded:?} vs {written:?}");
            }
        }
    }
    #[test]
    fn patterns_become_channel_major_frames() {
        let text = "000000 000000 000000 010203 000000 000000 40\nff0000 000000 000000 000000 000000 0000ff 20\n";
        let frames = parse(text, None, false).unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].ms, 40);
        assert_eq!(
            (frames[0].level[3], frames[0].level[9], frames[0].level[15]),
            (1, 2, 3)
        );
        assert_eq!((frames[1].level[0], frames[1].level[17]), (255, 255));
        let corrected = parse(
            "6b0fff 6b0fff 6b0fff 6b0fff 6b0fff 6b0fff 20",
            Some(&blue()),
            false,
        )
        .unwrap();
        assert_eq!(
            &corrected[0].level[..],
            &[[255; 6], [16; 6], [105; 6]].concat()[..]
        );
    }
    #[test]
    fn looping_or_malformed_patterns_are_not_played() {
        for text in [
            "loop\n000000 000000 000000 000000 000000 000000 20",
            "gammacorrection\n116111 116111 116111 116111 116111 116111 1000",
            "000000 000000 000000 000000 000000 20",
            "000000 000000 000000 000000 000000 00000g 20",
            "000000 000000 000000 000000 000000 000000 0",
            "000000 000000 000000 000000 000000 000000 x",
            "",
        ] {
            assert!(parse(text, None, false).is_none(), "{text}");
        }
        let long = "000000 000000 000000 000000 000000 000000 20\n".repeat(MAX_FRAMES + 1);
        assert!(parse(&long, None, false).is_none());
        // The volume palette may start with its two directives, and only those.
        let palette = "gammacorrection\nloop\n116111 116111 116111 116111 116111 116111 1000\nF80000 F80000 F80000 F80000 F80000 F80000 1000";
        let levels = parse(palette, None, true).unwrap();
        assert_eq!(levels.len(), 2);
        assert!((230..0xf8).contains(&levels[1].level[0])); // 0xf8 darkened by gamma 2.8
        assert!(parse(&format!("smooth 5\n{palette}"), None, true).is_none());
        assert!(
            parse(
                &palette
                    .replace("loop\n", "")
                    .replace("1000\nF8", "1000\nloop\nF8"),
                None,
                true
            )
            .is_none()
        );
    }
    #[test]
    fn clock_animations_unpack_into_the_driver_led_order() {
        // Two frames: G1 at 255 with MINUS at 128 for 20 ms, then DEGREE at 7 for 160 ms.
        const PACKED: &[u8] = &[
            0x28, 0xb5, 0x2f, 0xfd, 0x24, 0x46, 0xa5, 0x00, 0x00, 0x60, 0xff, 0x00, 0x80, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x14, 0x00, 0x07, 0xa0, 0x02, 0x00, 0x00, 0xe7, 0x36, 0xc0,
            0x02, 0xfc, 0xd5, 0x39, 0x2f,
        ];
        let dir = std::env::temp_dir().join(format!("spotiya-clock-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("player_next_track.binscled.zst"), PACKED).unwrap();
        fs::write(dir.join("player_pause.binscled.zst"), b"not zstd").unwrap();
        let clock = Clock::load(&dir);
        fs::remove_dir_all(dir).unwrap();
        assert_eq!(clock.next.len(), 2);
        let first = clock.next[0];
        assert_eq!((first.level[6], first.level[32], first.ms), (255, 128, 20));
        assert_eq!(first.level.iter().filter(|v| **v > 0).count(), 2);
        assert_eq!((clock.next[1].level[33], clock.next[1].ms), (7, 160));
        assert!(clock.previous.is_empty() && clock.pause.is_empty());
        // Each digit: file G, A..F becomes driver A..G.
        let mut raw = vec![0; 35];
        raw[7 + 1] = 1; // digit 2, file part 1 = A
        raw[21 + 6] = 2; // digit 4, file part 6 = F
        raw[34] = 20;
        let frame = parse_clock(&raw).unwrap()[0];
        assert_eq!((frame.level[7], frame.level[21 + 5]), (1, 2));
        assert!(parse_clock(&raw[..34]).is_none());
        raw[34] = 0;
        assert!(parse_clock(&raw).is_none());
    }
    #[test]
    fn missing_files_leave_the_gesture_on_the_built_in_effect() {
        let dir = std::env::temp_dir().join(format!("spotiya-ring-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        fs::write(
            dir.join("player_next_track.led"),
            "ff0000 000000 000000 000000 000000 000000 20\n",
        )
        .unwrap();
        fs::write(dir.join("player_pause.led"), "loop\n").unwrap();
        let ring = Ring::load(&dir, Some(&dir.join("missing.json")));
        assert_eq!(ring.next.len(), 1);
        assert_eq!(ring.next[0].level[0], 255);
        assert!(ring.previous.is_empty() && ring.pause.is_empty() && ring.resume.is_empty());
        fs::remove_dir_all(dir).unwrap();
    }
}
