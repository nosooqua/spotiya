//! Foreground effects use Quasar's compositor; background spectrum cooperatively
//! owns only Station 2's panel or Cucumber's ring and yields to native animations.
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::Path,
    sync::{Arc, Mutex, atomic::Ordering},
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Model {
    Station2,
    Cucumber,
}
impl Model {
    fn native_name(self) -> &'static str {
        match self {
            Self::Station2 => "yandexmidi",
            Self::Cucumber => "cucumber",
        }
    }
    fn geometry(self) -> (usize, usize, usize) {
        match self {
            Self::Station2 => (89, 84, 4),
            Self::Cucumber => (24, 24, 3),
        }
    }
    fn patterns(self) -> &'static str {
        match self {
            Self::Station2 => "/system/vendor/quasar/ledpatterns/panel",
            Self::Cucumber => "/system/vendor/quasar/ledpatterns/ring",
        }
    }
    pub fn available(self, root: &Path) -> bool {
        let (total, _, bytes) = self.geometry();
        let read = |name| {
            fs::read_to_string(root.join(format!("sys/class/graphics/fb0/{name}")))
                .unwrap_or_default()
        };
        root.join("dev/fb0").exists()
            && read("virtual_size").trim() == format!("{total},1")
            && read("bits_per_pixel").trim() == (bytes * 8).to_string()
            && read("stride").trim() == (total * bytes).to_string()
            && read("name").trim()
                == match self {
                    Self::Station2 => "YNDX LED screen",
                    Self::Cucumber => "ring",
                }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    Next,
    Previous,
    Pause,
    Resume,
    Volume(u16),
}
#[derive(Default)]
pub struct Feedback(Mutex<Option<(Effect, Instant)>>);
impl Feedback {
    pub fn show(&self, effect: Effect) {
        if let Ok(mut slot) = self.0.lock() {
            *slot = Some((effect, Instant::now()));
        }
    }
    fn take(&self) -> Option<(Effect, Instant)> {
        self.0.lock().ok()?.take()
    }
}
#[derive(Debug)]
struct Frame {
    pixels: Vec<u8>,
    ms: u32,
}
#[derive(Default, Debug)]
struct Pattern {
    frames: Vec<Frame>,
    duration: u32,
}
impl Pattern {
    fn parse(text: &str, pixels: usize, bytes: usize) -> Option<Self> {
        if text.len() > 1024 * 1024 || !matches!(bytes, 3 | 4) || pixels == 0 || pixels > 89 {
            return None;
        }
        let mut pattern = Self::default();
        let mut gamma = false;
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
            match line {
                "no_size_optimization" if pattern.frames.is_empty() => continue,
                "gammacorrection" if pattern.frames.is_empty() => {
                    gamma = true;
                    continue;
                }
                _ => {}
            }
            let fields: Vec<_> = line.split_ascii_whitespace().collect();
            if fields.len() != pixels + 1 || pattern.frames.len() >= 512 {
                return None;
            }
            let ms = fields[pixels]
                .parse::<u32>()
                .ok()
                .filter(|n| (1..=5000).contains(n))?;
            let mut data = Vec::with_capacity(pixels * bytes);
            for color in &fields[..pixels] {
                if color.len() != bytes * 2 || !color.bytes().all(|c| c.is_ascii_hexdigit()) {
                    return None;
                }
                for i in 0..bytes {
                    let v = u8::from_str_radix(&color[i * 2..i * 2 + 2], 16).ok()?;
                    data.push(if gamma {
                        (255.0 * (f64::from(v) / 255.0).powf(2.8)) as u8
                    } else {
                        v
                    });
                }
            }
            pattern.duration += ms;
            pattern.frames.push(Frame { pixels: data, ms });
        }
        (!pattern.frames.is_empty()).then_some(pattern)
    }
    fn load(path: &Path, pixels: usize, bytes: usize) -> Self {
        let result = (|| {
            let mut text = String::new();
            File::open(path)
                .ok()?
                .take(1024 * 1024 + 1)
                .read_to_string(&mut text)
                .ok()?;
            Self::parse(&text, pixels, bytes)
        })();
        if result.is_none() {
            eprintln!("leds: unavailable pattern {}", path.display());
        }
        result.unwrap_or_default()
    }
    #[cfg(test)]
    fn at(&self, elapsed: u32) -> Option<&[u8]> {
        let mut end = 0;
        self.frames
            .iter()
            .find(|f| {
                end += f.ms;
                elapsed < end
            })
            .map(|f| f.pixels.as_slice())
    }
}
struct Patterns {
    next: Pattern,
    previous: Pattern,
    pause: Pattern,
    resume: Pattern,
    volume: Pattern,
}
impl Patterns {
    fn load(model: Model) -> Self {
        let (_, pixels, bytes) = model.geometry();
        Self::directory(model.patterns(), pixels, bytes)
    }
    fn logo(model: Model) -> Option<Self> {
        let read = |name| {
            fs::read_to_string(format!("/sys/class/graphics/fb1/{name}")).unwrap_or_default()
        };
        (matches!(model, Model::Cucumber)
            && read("name").trim() == "logo"
            && read("virtual_size").trim() == "6,1"
            && read("bits_per_pixel").trim() == "24"
            && read("stride").trim() == "18")
            .then(|| Self::directory("/system/vendor/quasar/ledpatterns/oknix", 6, 3))
    }
    fn directory(directory: &str, pixels: usize, bytes: usize) -> Self {
        let load = |name| {
            Pattern::load(
                &Path::new(directory).join(format!("{name}.led")),
                pixels,
                bytes,
            )
        };
        Self {
            next: load("player_next_track"),
            previous: load("player_previous_track"),
            pause: load("player_pause"),
            resume: load("player_continue"),
            volume: load("volume"),
        }
    }
    fn effect(&self, effect: Effect) -> Pattern {
        match effect {
            Effect::Volume(volume) => {
                if self.volume.frames.is_empty() {
                    return Pattern::default();
                }
                let index = usize::from(volume.min(100)) * (self.volume.frames.len() - 1) / 100;
                Pattern {
                    duration: 1000,
                    frames: (0..50)
                        .map(|tick| {
                            let fade = (1000 - tick * 20).min(400);
                            Frame {
                                pixels: self.volume.frames[index]
                                    .pixels
                                    .iter()
                                    .map(|v| (u32::from(*v) * fade / 400) as u8)
                                    .collect(),
                                ms: 20,
                            }
                        })
                        .collect(),
                }
            }
            effect => {
                let p = match effect {
                    Effect::Next => &self.next,
                    Effect::Previous => &self.previous,
                    Effect::Pause => &self.pause,
                    _ => &self.resume,
                };
                Pattern {
                    duration: p.duration,
                    frames: p
                        .frames
                        .iter()
                        .map(|f| Frame {
                            pixels: f.pixels.clone(),
                            ms: f.ms,
                        })
                        .collect(),
                }
            }
        }
    }
}
fn encode(pattern: &Pattern, bytes: usize) -> Vec<u8> {
    use std::fmt::Write;
    let mut text = String::new();
    for frame in &pattern.frames {
        for pixel in frame.pixels.chunks_exact(bytes) {
            for value in pixel {
                write!(text, "{value:02x}").unwrap();
            }
            text.push(' ');
        }
        writeln!(text, "{}", frame.ms).unwrap();
    }
    text.into_bytes()
}
fn effect_name(effect: Effect) -> String {
    match effect {
        Effect::Next => "next".into(),
        Effect::Previous => "previous".into(),
        Effect::Pause => "pause".into(),
        Effect::Resume => "resume".into(),
        Effect::Volume(v) => format!("volume-{}", v.min(100)),
    }
}

struct Foreground {
    native: crate::led_native::Native,
    durations: std::collections::HashMap<String, Duration>,
    model: Model,
}
impl Foreground {
    fn open(model: Model, preview: bool) -> io::Result<Self> {
        let patterns = Patterns::load(model);
        let logo = Patterns::logo(model);
        let (_, _, bytes) = model.geometry();
        let region = match model {
            Model::Station2 => "panel.led",
            Model::Cucumber => "ring.led",
        };
        let mut bundles = Vec::new();
        let mut durations = std::collections::HashMap::new();
        let effects = [
            Effect::Next,
            Effect::Previous,
            Effect::Pause,
            Effect::Resume,
        ]
        .into_iter()
        .chain((0..=100).map(Effect::Volume));
        for effect in effects {
            let pattern = patterns.effect(effect);
            if pattern.frames.is_empty() || pattern.duration > 10_000 {
                continue;
            }
            let mut files = vec![(region.to_owned(), encode(&pattern, bytes))];
            let mut duration = pattern.duration;
            if let Some(logo) = &logo {
                let logo = logo.effect(effect);
                if !logo.frames.is_empty() && logo.duration <= 10_000 {
                    duration = duration.max(logo.duration);
                    files.push(("oknix.led".into(), encode(&logo, 3)));
                }
            }
            let clock = matches!(model, Model::Cucumber) && matches!(effect, Effect::Volume(_));
            if clock {
                duration = duration.max(2000);
            }
            let name = effect_name(effect);
            durations.insert(name.clone(), Duration::from_millis(u64::from(duration)));
            bundles.push((name, files));
        }
        let native = if preview {
            crate::led_native::Native::preview(model.native_name(), bundles)
        } else {
            crate::led_native::Native::start(model.native_name(), bundles)
        }?;
        Ok(Self {
            native,
            durations,
            model,
        })
    }
    fn show(&self, effect: Effect, decreasing: bool) -> io::Result<Duration> {
        let name = effect_name(effect);
        let duration = self
            .durations
            .get(&name)
            .ok_or_else(|| io::Error::other("Firmware LED effect unavailable"))?;
        let clock = match (self.model, effect) {
            (Model::Cucumber, Effect::Volume(volume)) => {
                Some(crate::led_clock::animation(volume, decreasing))
            }
            _ => None,
        };
        self.native.show(&name, clock.as_deref())?;
        // Allow the local loader to start before cooperative spectrum writes resume.
        Ok(*duration + Duration::from_millis(250))
    }
}

struct Output {
    file: File,
    size: usize,
    region: usize,
    last: Option<Vec<u8>>,
}
impl Output {
    fn open(path: &Path, model: Model) -> io::Result<Self> {
        let (total, pixels, bytes) = model.geometry();
        Self::region(path, total * bytes, pixels * bytes)
    }
    fn region(path: &Path, size: usize, region: usize) -> io::Result<Self> {
        Ok(Self {
            file: OpenOptions::new().read(true).write(true).open(path)?,
            size,
            region,
            last: None,
        })
    }
    fn read(&mut self) -> io::Result<Vec<u8>> {
        let mut data = vec![0; self.size];
        self.file.seek(SeekFrom::Start(0))?;
        self.file.read_exact(&mut data)?;
        Ok(data)
    }
    fn write(&mut self, pixels: &[u8]) -> io::Result<bool> {
        if pixels.len() != self.region {
            return Err(io::Error::other("Invalid LED frame size"));
        }
        let current = self.read()?;
        let owned = self.last.as_ref().map_or_else(
            || current[..self.region].iter().all(|v| *v == 0),
            |last| current[..self.region] == last[..],
        );
        if !owned {
            self.last = None;
            return Ok(false);
        }
        // Partial framebuffer writes leave the five Station 2 logo slots untouched.
        self.file.seek(SeekFrom::Start(0))?;
        self.file.write_all(pixels)?;
        self.last = Some(pixels.to_vec());
        Ok(true)
    }
    fn release(&mut self) {
        if self.last.is_some() {
            let _ = self.write(&vec![0; self.region]);
        }
        self.last = None;
    }
}
impl Drop for Output {
    fn drop(&mut self) {
        self.release();
    }
}

/// Four broad frequency zones, with a quiet baseline and smooth attack/release from the
/// analyzer. The panel's physical layout differs from its linear framebuffer indexing;
/// each LED therefore gets a continuously blended colour rather than pretending it is a grid.
fn visual(model: Model, levels: [u8; 4], phase: usize) -> Vec<u8> {
    let (_, pixels, bytes) = model.geometry();
    let mut frame = vec![0; pixels * bytes];
    let colors = [[55u32, 4, 100], [10, 18, 120], [0, 100, 70], [60, 100, 5]];
    for (i, pixel) in frame.chunks_exact_mut(bytes).enumerate() {
        let at = (i * 4 * 256 / pixels + phase) % 1024;
        let band = at / 256;
        let next = (band + 1) % 4;
        let t = (at % 256) as u32;
        let level = (u32::from(levels[band].min(100)) * (256 - t)
            + u32::from(levels[next].min(100)) * t)
            / 256;
        for ch in 0..3 {
            let color = (colors[band][ch] * (256 - t) + colors[next][ch] * t) / 256;
            pixel[ch] = (color * level * level / 10000) as u8;
        }
    }
    frame
}

pub fn start(model: Model, shared: Arc<crate::Shared>) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut native = None;
        let mut output = None::<Output>;
        let mut previous = (false, false, 0);
        let origin = Instant::now();
        let mut retry = origin;
        let mut native_retry = origin;
        let mut foreground_until = origin;
        while shared.running.load(Ordering::Acquire) {
            let now = Instant::now();
            let active = shared.active.load(Ordering::Acquire);
            let paused = shared.paused.load(Ordering::Relaxed);
            let volume = crate::percent(shared.volume.load(Ordering::Relaxed));
            let decreasing = volume < previous.2;
            let mut effect = None;
            if active && previous.0 {
                if volume != previous.2 {
                    effect = Some(Effect::Volume(volume));
                } else if paused != previous.1 {
                    effect = Some(if paused {
                        Effect::Pause
                    } else {
                        Effect::Resume
                    });
                }
            } else if active && !paused {
                effect = Some(Effect::Resume);
            }
            previous = (active, paused, volume);
            if let Some((event, at)) = shared.feedback.take()
                && now.duration_since(at) < Duration::from_secs(2)
            {
                effect = Some(event);
            }
            let allowed = shared.controls_enabled.load(Ordering::Acquire);
            if allowed && native.is_none() && now >= native_retry {
                match Foreground::open(model, false) {
                    Ok(foreground) => native = Some(foreground),
                    Err(e) => eprintln!("leds: native compositor assets unavailable: {e}"),
                }
                native_retry = now + Duration::from_secs(5);
            }
            // Unlike background spectrum, short user feedback is allowed during Alice.
            // Release our last frame BEFORE asking the native compositor to take over.
            if let Some(effect) = effect.filter(|_| allowed) {
                output = None;
                if let Some(native) = &native {
                    match native.show(effect, decreasing) {
                        Ok(duration) => foreground_until = Instant::now() + duration,
                        Err(e) => eprintln!("leds: native effect failed: {e}"),
                    }
                }
            }
            let frame = if allowed
                && !shared.ducking.active()
                && now >= foreground_until
                && active
                && !paused
            {
                shared.spectrum.frame().map(|levels| {
                    visual(model, levels, (origin.elapsed().as_millis() / 30) as usize)
                })
            } else {
                None
            };
            if let Some(frame) = frame {
                if output.is_none() && now >= retry {
                    output = Output::open(Path::new("/dev/fb0"), model).ok();
                    retry = now + Duration::from_secs(1);
                }
                if let Some(out) = output.as_mut() {
                    match out.write(&frame) {
                        Ok(true) => {}
                        Ok(false) => {
                            output = None;
                            retry = now + Duration::from_secs(1);
                        }
                        Err(e) => {
                            eprintln!("leds: {e}; yielding output");
                            output = None;
                            retry = now + Duration::from_secs(5);
                        }
                    }
                }
            } else {
                output = None;
            }
            thread::sleep(Duration::from_millis(20));
        }
    })
}

/// A bounded foreground check through the same native compositor as button feedback.
pub fn preview(model: Model, name: &str) -> io::Result<()> {
    let effect = match name {
        "next" => Effect::Next,
        "previous" => Effect::Previous,
        "pause" => Effect::Pause,
        "resume" => Effect::Resume,
        _ => Effect::Volume(
            name.strip_prefix("volume:")
                .or_else(|| name.strip_prefix("volume-down:"))
                .and_then(|n| n.parse().ok())
                .filter(|v| *v <= 100)
                .ok_or_else(|| {
                    io::Error::other("Use next, previous, pause, resume or volume:0..100")
                })?,
        ),
    };
    let native = Foreground::open(model, true)?;
    thread::sleep(native.show(effect, name.starts_with("volume-down:"))?);
    println!("Native LED preview requested: {name}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn firmware_formats_and_timing_are_bounded() {
        let p = Pattern::parse("no_size_optimization\n01020304 20\n05060708 40\n", 1, 4).unwrap();
        assert_eq!(p.at(19), Some([1, 2, 3, 4].as_slice()));
        assert_eq!(p.at(20), Some([5, 6, 7, 8].as_slice()));
        assert_eq!(p.at(60), None);
        assert!(Pattern::parse("ff0000 20", 1, 3).is_some());
        for text in [
            "loop\nff0000 20",
            "ff0000 0",
            "gg0000 20",
            "00000000 20",
            "ff0000 20\nno_size_optimization",
        ] {
            assert!(Pattern::parse(text, 1, 3).is_none(), "{text}");
        }
        assert!(Pattern::parse(&"000000 20\n".repeat(513), 1, 3).is_none());
    }
    #[test]
    fn generated_volume_is_finite_and_does_not_animate_through_other_levels() {
        let patterns = Patterns {
            next: Pattern::default(),
            previous: Pattern::default(),
            pause: Pattern::default(),
            resume: Pattern::default(),
            volume: Pattern::parse("gammacorrection\n000000 20\n808080 20\nffffff 20\n", 1, 3)
                .unwrap(),
        };
        for (volume, level) in [(0, 0), (50, 37), (100, 255)] {
            let p = patterns.effect(Effect::Volume(volume));
            let encoded = encode(&p, 3);
            let loaded = Pattern::parse(std::str::from_utf8(&encoded).unwrap(), 1, 3).unwrap();
            assert_eq!(loaded.duration, 1000);
            assert_eq!(loaded.at(0).unwrap(), &[level; 3]);
            assert_eq!(loaded.at(599).unwrap(), &[level; 3]);
            assert!(loaded.at(980).unwrap().iter().all(|v| *v <= level / 20));
            assert!(loaded.at(1000).is_none());
        }
    }
    #[test]
    fn panel_writes_preserve_logo_and_yield_to_native_renderer() {
        let path = std::env::temp_dir().join(format!("spotiya-led-{}", uuid::Uuid::new_v4()));
        let mut initial = vec![0; 356];
        initial[340] = 99;
        fs::write(&path, &initial).unwrap();
        let mut out = Output::open(&path, Model::Station2).unwrap();
        assert!(out.write(&vec![8; 336]).unwrap());
        assert_eq!(fs::read(&path).unwrap()[340], 99);
        // Quasar replaces a panel pixel: neither the next tick nor Drop may erase it.
        let mut native = fs::read(&path).unwrap();
        native[0] = 77;
        fs::write(&path, &native).unwrap();
        assert!(!out.write(&vec![9; 336]).unwrap());
        drop(out);
        assert_eq!(fs::read(&path).unwrap(), native);
        fs::write(&path, &initial).unwrap();
        let mut out = Output::open(&path, Model::Station2).unwrap();
        assert!(out.write(&vec![8; 336]).unwrap());
        drop(out);
        assert_eq!(fs::read(&path).unwrap(), initial);
        fs::remove_file(path).unwrap();
    }
    #[test]
    fn music_frame_silence_and_white_channel() {
        for model in [Model::Station2, Model::Cucumber] {
            assert!(visual(model, [0; 4], 0).iter().all(|v| *v == 0));
            assert!(visual(model, [100; 4], 0).iter().any(|v| *v > 0));
        }
        assert!(
            visual(Model::Station2, [100; 4], 100)
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| p[3] == 0)
        );
    }
}
