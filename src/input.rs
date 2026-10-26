//! Profile-defined evdev actions. Device discovery uses the exact kernel input name.
use serde::{Deserialize, Serialize};
use std::{
    fs, io,
    io::Read,
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
    path::{Path, PathBuf},
};

fn open(path: &Path) -> io::Result<fs::File> {
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
}
pub fn list() -> io::Result<()> {
    let mut devices = vec![];
    for entry in fs::read_dir("/sys/class/input")? {
        let entry = entry?;
        let event = entry.file_name().to_string_lossy().into_owned();
        if !event
            .strip_prefix("event")
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        {
            continue;
        }
        let name = fs::read_to_string(entry.path().join("device/name"))?;
        let path = Path::new("/dev/input").join(event);
        let file = open(&path)?;
        let mut bits = [0u8; 96];
        if unsafe {
            libc::ioctl(
                file.as_raw_fd(),
                0x80604521 as libc::c_ulong,
                bits.as_mut_ptr(),
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        let keys: Vec<_> = (0..768)
            .filter(|code| bits[code / 8] & (1 << (code % 8)) != 0)
            .collect();
        devices.push(serde_json::json!({"path":path,"name":name.trim(),"keys":keys}));
    }
    devices.sort_by_key(|v| v["path"].as_str().unwrap_or_default().to_owned());
    println!("{}", serde_json::to_string_pretty(&devices)?);
    Ok(())
}
/// Passive, bounded capture: no EVIOCGRAB and no playback commands.
pub fn monitor(selector: &str) -> io::Result<()> {
    let path = if selector.starts_with('/') {
        device(Path::new("/"), Some(selector), None)
    } else {
        device(Path::new("/"), None, Some(selector))
    }
    .ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "Input not found or name is ambiguous; use --inputs",
        )
    })?;
    let mut file = open(&path)?;
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Event {
        sec: libc::c_long,
        usec: libc::c_long,
        kind: u16,
        code: u16,
        value: i32,
    }
    let mut bytes = [0u8; std::mem::size_of::<Event>()];
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    eprintln!("Listening to {} for 60 seconds (no grab)", path.display());
    while std::time::Instant::now() < deadline {
        let mut poll = libc::pollfd {
            fd: file.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        if unsafe { libc::poll(&mut poll, 1, 200) } < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        if poll.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            return Err(io::Error::other("Input disconnected"));
        }
        if poll.revents & libc::POLLIN == 0 {
            continue;
        }
        match file.read(&mut bytes) {
            Ok(n) if n == bytes.len() => {
                let e = unsafe { bytes.as_ptr().cast::<Event>().read_unaligned() };
                if e.kind == 1 || (e.kind == 0 && e.code == 3) {
                    println!(
                        "{}.{:06} type={} code={} value={}",
                        e.sec, e.usec, e.kind, e.code, e.value
                    );
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) => {}
            Err(e) => return Err(e),
            _ => return Err(io::Error::other("Unexpected input event size")),
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(u32)]
pub enum Action {
    Toggle,
    Next,
    Previous,
    VolumeUp,
    VolumeDown,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub code: u32,
    pub action: Action,
    #[serde(default)]
    pub repeat: bool,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Gesture {
    pub from: u32,
    pub to: u32,
    pub action: Action,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Mapping {
    pub bindings: Vec<Binding>,
    pub gestures: Vec<Gesture>,
    pub gesture_ms: u32,
    pub repeat_delay_ms: u32,
    pub repeat_ms: u32,
    pub volume_step: u32,
}
impl Default for Mapping {
    fn default() -> Self {
        Self {
            bindings: vec![],
            gestures: vec![],
            gesture_ms: 250,
            repeat_delay_ms: 450,
            repeat_ms: 180,
            volume_step: 5,
        }
    }
}
impl Mapping {
    pub fn validate(&self) -> io::Result<()> {
        let valid_code = |code| (1..768).contains(&code);
        let mut codes = std::collections::HashSet::new();
        let mut pairs = std::collections::HashSet::new();
        if self.bindings.is_empty()
            || self.bindings.len() > 32
            || self.gestures.len() > 32
            || !(20..=1000).contains(&self.gesture_ms)
            || !(200..=2000).contains(&self.repeat_delay_ms)
            || !(50..=1000).contains(&self.repeat_ms)
            || !(1..=20).contains(&self.volume_step)
            || self.bindings.iter().any(|b| {
                !valid_code(b.code)
                    || !codes.insert(b.code)
                    || (b.repeat && !matches!(b.action, Action::VolumeUp | Action::VolumeDown))
            })
            || self.gestures.iter().any(|g| {
                g.from == g.to
                    || !codes.contains(&g.from)
                    || !codes.contains(&g.to)
                    || !pairs.insert((g.from, g.to))
            })
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid evdev bindings, gestures or timing",
            ));
        }
        Ok(())
    }
}

pub fn device(root: &Path, path: Option<&str>, name: Option<&str>) -> Option<PathBuf> {
    let matches = |event: &str| {
        let path = root.join("dev/input").join(event);
        (path.exists()
            && name.is_none_or(|expected| {
                fs::read_to_string(root.join("sys/class/input").join(event).join("device/name"))
                    .is_ok_and(|actual| actual.trim() == expected)
            }))
        .then_some(path)
    };
    if let Some(path) = path {
        let event = Path::new(path).file_name()?.to_str()?;
        if Path::new(path).parent()? != Path::new("/dev/input") {
            return None;
        }
        return matches(event);
    }
    name?;
    let mut found = fs::read_dir(root.join("sys/class/input"))
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|e| {
            let event = e.file_name();
            let event = event.to_str()?;
            let suffix = event.strip_prefix("event")?;
            if suffix.is_empty() || !suffix.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            matches(event)
        });
    let first = found.next()?;
    // Ambiguous names need an explicit path, never an arbitrary first device.
    found.next().is_none().then_some(first)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn input_names_survive_renumbering_and_ambiguous_names_fail_closed() {
        let root = std::env::temp_dir().join(format!("input-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("dev/input")).unwrap();
        for (event, name) in [("event0", "gpio-keys"), ("event7", "midi_touch")] {
            fs::create_dir_all(root.join(format!("sys/class/input/{event}/device"))).unwrap();
            fs::write(
                root.join(format!("sys/class/input/{event}/device/name")),
                name,
            )
            .unwrap();
            fs::write(root.join(format!("dev/input/{event}")), b"").unwrap();
        }
        assert_eq!(
            device(&root, None, Some("midi_touch")),
            Some(root.join("dev/input/event7"))
        );
        assert!(device(&root, Some("/dev/input/event0"), Some("midi_touch")).is_none());
        assert!(device(&root, None, None).is_none());
        assert!(device(&root, Some("/tmp/event7"), None).is_none());
        fs::write(
            root.join("sys/class/input/event0/device/name"),
            "midi_touch",
        )
        .unwrap();
        assert!(device(&root, None, Some("midi_touch")).is_none());
        assert_eq!(
            device(&root, Some("/dev/input/event7"), Some("midi_touch")),
            Some(root.join("dev/input/event7"))
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn mapping_rejects_ambiguous_or_unbounded_configuration() {
        let profile: serde_json::Value =
            serde_json::from_str(include_str!("../profiles/midi.json")).unwrap();
        let mut original = profile["controls"]["mapping"].clone();
        original["bindings"][0]["repeat"] = serde_json::json!(false);
        for (pointer, value) in [
            ("/bindings/1/code", serde_json::json!(582)),
            ("/bindings/0/code", serde_json::json!(768)),
            ("/bindings/0/repeat", serde_json::json!(true)),
            ("/gestures/0/from", serde_json::json!(999)),
            ("/gestures/1/to", serde_json::json!(115)),
            ("/gesture_ms", serde_json::json!(0)),
            ("/repeat_ms", serde_json::json!(0)),
            ("/repeat_delay_ms", serde_json::json!(0)),
            ("/volume_step", serde_json::json!(100)),
        ] {
            let mut value_map = original.clone();
            *value_map.pointer_mut(pointer).unwrap() = value;
            let mapping: Mapping = serde_json::from_value(value_map).unwrap();
            assert!(mapping.validate().is_err(), "{pointer}");
        }
        let mapping: Mapping = serde_json::from_value(original).unwrap();
        mapping.validate().unwrap();
    }
}
