use crate::{
    control::Status,
    installation::{self, BASE, INIT, RECEIVER_PID, SUPERVISOR_PID},
};
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsStr,
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::Path,
    time::{Duration, Instant},
};

const SETTING: &str = "device-names.json";
// The RAOP service label also contains a 12-digit identifier and '@'.
const MAX_NAME: usize = 50;

#[derive(Clone, Default, Deserialize, Serialize, PartialEq, Debug)]
#[serde(deny_unknown_fields)]
pub struct Saved {
    pub common: Option<String>,
    pub spotify: Option<String>,
    pub airplay: Option<String>,
}
#[derive(Serialize, PartialEq, Debug)]
pub struct Names {
    pub spotify: String,
    pub airplay: String,
}
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Scope {
    All,
    Spotify,
    Airplay,
}
#[derive(PartialEq, Debug)]
pub enum Action {
    Show,
    Set(Scope, String),
    Reset(Scope),
}
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
pub fn validate(name: &str) -> io::Result<String> {
    if name.chars().any(char::is_control) {
        return Err(invalid("Device name cannot contain control characters"));
    }
    if ["%h", "%H", "%v", "%V"]
        .iter()
        .any(|token| name.contains(token))
    {
        return Err(invalid(
            "Device name cannot contain AirPlay substitutions %h, %H, %v or %V",
        ));
    }
    let name = name.trim();
    if name.is_empty() || name.len() > MAX_NAME {
        return Err(invalid("Device name must contain 1–50 UTF-8 bytes"));
    }
    Ok(name.into())
}
pub fn parse(args: &[String]) -> io::Result<Action> {
    let words: Vec<_> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        [] => Ok(Action::Show),
        ["reset"] => Ok(Action::Reset(Scope::All)),
        ["reset", "spotify"] => Ok(Action::Reset(Scope::Spotify)),
        ["reset", "airplay"] => Ok(Action::Reset(Scope::Airplay)),
        [name] | ["set", name] => Ok(Action::Set(Scope::All, validate(name)?)),
        ["spotify", name] => Ok(Action::Set(Scope::Spotify, validate(name)?)),
        ["airplay", name] => Ok(Action::Set(Scope::Airplay, validate(name)?)),
        _ => Err(invalid(
            "Usage: spotiya name [NAME | set NAME | spotify NAME | airplay NAME | reset [spotify|airplay]]",
        )),
    }
}
pub fn read(state: &Path) -> io::Result<Saved> {
    let file = match fs::File::open(state.join(SETTING)) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Saved::default()),
        Err(e) => return Err(e),
    };
    let mut text = String::new();
    file.take(4097).read_to_string(&mut text)?;
    if text.len() > 4096 {
        return Err(invalid("Device names setting exceeds 4 KiB"));
    }
    let saved: Saved =
        serde_json::from_str(&text).map_err(|_| invalid("Invalid device names setting"))?;
    for name in [&saved.common, &saved.spotify, &saved.airplay]
        .into_iter()
        .flatten()
    {
        if validate(name)? != *name {
            return Err(invalid(
                "Saved device names must not have surrounding whitespace",
            ));
        }
    }
    Ok(saved)
}
impl Saved {
    pub fn resolve(&self, profile: &str, explicit: Option<&str>) -> io::Result<Names> {
        // Profiles allow longer display labels; shorten only their network names,
        // keeping UTF-8 intact and leaving the profile itself unchanged.
        let mut end = profile.len().min(MAX_NAME);
        while !profile.is_char_boundary(end) {
            end -= 1;
        }
        let default = validate(&profile[..end])?;
        let common = self.common.as_deref().unwrap_or(&default);
        if let Some(name) = explicit {
            let name = validate(name)?;
            return Ok(Names {
                spotify: name.clone(),
                airplay: name,
            });
        }
        Ok(Names {
            spotify: self.spotify.as_deref().unwrap_or(common).into(),
            airplay: self.airplay.as_deref().unwrap_or(common).into(),
        })
    }
    fn change(&mut self, action: Action) {
        match action {
            Action::Set(Scope::All, name) => {
                *self = Self {
                    common: Some(name),
                    ..Self::default()
                }
            }
            Action::Set(Scope::Spotify, name) => self.spotify = Some(name),
            Action::Set(Scope::Airplay, name) => self.airplay = Some(name),
            Action::Reset(Scope::All) => *self = Self::default(),
            Action::Reset(Scope::Spotify) => self.spotify = None,
            Action::Reset(Scope::Airplay) => self.airplay = None,
            Action::Show => {}
        }
    }
}
fn write(state: &Path, bytes: &[u8]) -> io::Result<()> {
    let temp = state.join(format!(".device-names-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temp, state.join(SETTING))?;
        fs::File::open(state)?.sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}
fn transaction(
    state: &Path,
    saved: &Saved,
    mut apply: impl FnMut() -> io::Result<()>,
) -> io::Result<()> {
    let previous = match fs::read(state.join(SETTING)) {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    write(state, &serde_json::to_vec_pretty(saved)?)?;
    if let Err(error) = apply() {
        let restored = match previous {
            Some(bytes) => write(state, &bytes),
            None => fs::remove_file(state.join(SETTING)),
        };
        let recovered = apply();
        return Err(io::Error::other(format!(
            "Rename failed: {error}; previous names restored: {}; receiver recovered: {}",
            restored.is_ok(),
            recovered.is_ok(),
        )));
    }
    Ok(())
}
fn live(file: &str) -> Option<u32> {
    installation::live_pid(Path::new(BASE), Path::new(file))
}
fn restart(state: &Path, profile: &str) -> io::Result<()> {
    let names = read(state)?.resolve(profile, None)?;
    let output = installation::command(
        Path::new(INIT),
        &[OsStr::new("restart")],
        Duration::from_secs(20),
    )?;
    if !output.status.success() {
        return Err(io::Error::other("Receiver restart failed"));
    }
    let deadline = Instant::now() + Duration::from_secs(6);
    while Instant::now() < deadline {
        if let Some(pid) = live(RECEIVER_PID)
            && let Ok(text) = fs::read_to_string(crate::control::STATUS)
            && let Ok(status) = serde_json::from_str::<Status>(&text)
            && status.receiver_pid == pid
            && status.name == names.spotify
            && status.airplay_name == names.airplay
        {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(io::Error::other(
        "Receiver did not become ready with the requested names",
    ))
}
pub fn execute(
    state: &Path,
    selection: Option<&str>,
    explicit: Option<&str>,
    action: Action,
) -> io::Result<()> {
    // Share the lock with profile selection and installation: each can restart the service.
    let _lock = crate::file_lock(state, "profile-selection.lock")?;
    let profile = crate::profiles::load(selection, state)?.profile.name;
    let mut saved = read(state)?;
    if action == Action::Show {
        println!(
            "{}",
            serde_json::to_string_pretty(&saved.resolve(&profile, explicit)?)?
        );
        return Ok(());
    }
    let old = saved.clone();
    saved.change(action);
    let managed = fs::canonicalize(state).ok().is_some_and(|path| {
        fs::canonicalize(format!("{BASE}/receiver-state")).is_ok_and(|base| base == path)
    });
    let running = managed && (live(SUPERVISOR_PID).is_some() || live(RECEIVER_PID).is_some());
    if saved != old {
        transaction(state, &saved, || {
            if running {
                restart(state, &profile)
            } else {
                Ok(())
            }
        })?;
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&saved.resolve(&profile, None)?)?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn profile_defaults_overrides_reset_and_explicit_launch_name() {
        let mut saved = Saved::default();
        assert_eq!(
            saved.resolve("Yandex Station 2", None).unwrap().spotify,
            "Yandex Station 2"
        );
        saved.change(Action::Set(Scope::All, "Гостиная".into()));
        saved.change(Action::Set(Scope::Airplay, "Музыка".into()));
        let names = saved.resolve("Other profile", None).unwrap();
        assert_eq!(names.spotify, "Гостиная");
        assert_eq!(names.airplay, "Музыка");
        assert_eq!(
            saved.resolve("Profile", Some("Temporary")).unwrap(),
            Names {
                spotify: "Temporary".into(),
                airplay: "Temporary".into()
            }
        );
        saved.change(Action::Reset(Scope::Airplay));
        assert_eq!(saved.resolve("Profile", None).unwrap().airplay, "Гостиная");
        saved.change(Action::Reset(Scope::All));
        assert_eq!(
            saved.resolve("New profile", None).unwrap().airplay,
            "New profile"
        );
        let long = "Ю".repeat(48);
        assert_eq!(saved.resolve(&long, None).unwrap().spotify, "Ю".repeat(25));
    }
    #[test]
    fn validation_and_command_parsing_keep_names_literal() {
        for name in [
            "",
            "  ",
            "a\nb",
            "\tname",
            "bad\0name",
            "Room %H",
            &"x".repeat(51),
            &"я".repeat(26),
        ] {
            assert!(validate(name).is_err());
        }
        let name = "Гостиная; $(false) \\\"";
        assert_eq!(
            parse(&["airplay".into(), name.into()]).unwrap(),
            Action::Set(Scope::Airplay, name.into())
        );
        assert_eq!(
            parse(&["reset".into(), "spotify".into()]).unwrap(),
            Action::Reset(Scope::Spotify)
        );
        assert!(parse(&["reset".into(), "unknown".into()]).is_err());
    }
    #[test]
    fn persistence_and_failed_restart_restore_previous_setting() {
        let state = std::env::temp_dir().join(format!("spotiya-names-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&state).unwrap();
        let saved = Saved {
            common: Some("Гостиная".into()),
            ..Saved::default()
        };
        assert!(transaction(&state, &saved, || Err(io::Error::other("restart failed"))).is_err());
        assert_eq!(read(&state).unwrap(), Saved::default());
        transaction(&state, &saved, || Ok(())).unwrap();
        assert_eq!(read(&state).unwrap(), saved);
        let mut calls = 0;
        assert!(
            transaction(&state, &Saved::default(), || {
                calls += 1;
                if calls == 1 {
                    Err(io::Error::other("restart failed"))
                } else {
                    Ok(())
                }
            })
            .is_err()
        );
        assert_eq!(calls, 2);
        assert_eq!(read(&state).unwrap(), saved);
        fs::write(state.join(SETTING), "{\"spotify\":\"bad\\nname\"}").unwrap();
        assert!(read(&state).is_err());
        fs::remove_dir_all(state).unwrap();
    }
}
