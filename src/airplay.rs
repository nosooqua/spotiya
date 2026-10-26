use crate::{control, profiles};
use std::{
    fs,
    io::{self, BufRead, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    process::{Child, Command},
    sync::{Notify, oneshot},
    time::Instant,
};

pub struct Gate {
    pub active: AtomicBool,
    pub spotify_open: AtomicBool,
    spotify_closed: Notify,
    pid: Mutex<u32>,
    preempted: AtomicBool,
}
impl Gate {
    pub fn new() -> Self {
        Self {
            active: AtomicBool::new(false),
            spotify_open: AtomicBool::new(false),
            spotify_closed: Notify::new(),
            pid: Mutex::new(0),
            preempted: AtomicBool::new(false),
        }
    }
    pub fn close_spotify(&self) {
        self.spotify_open.store(false, Ordering::Release);
        self.spotify_closed.notify_waiters();
    }
    /// Waits until the Spotify sink has closed the audio device, or the deadline passes.
    pub async fn spotify_released(&self, deadline: Instant) -> bool {
        loop {
            let closed = self.spotify_closed.notified();
            tokio::pin!(closed);
            closed.as_mut().enable();
            if !self.spotify_open.load(Ordering::Acquire) {
                return true;
            }
            if tokio::time::timeout_at(deadline, closed).await.is_err() {
                return !self.spotify_open.load(Ordering::Acquire);
            }
        }
    }
    pub fn reclaim(&self) -> io::Result<()> {
        if !self.active.load(Ordering::Acquire) {
            return Ok(());
        }
        let locked = self
            .pid
            .lock()
            .map_err(|_| io::Error::other("AirPlay process lock poisoned"))?;
        let pid = *locked;
        if pid == 0 {
            self.active.store(false, Ordering::Release);
            return Ok(());
        }
        self.preempted.store(true, Ordering::Release);
        // The child is not reaped until the receiver polls it, so this PID cannot be reused.
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while std::time::Instant::now() < deadline {
            match fs::read_to_string(format!("/proc/{pid}/stat")) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    self.active.store(false, Ordering::Release);
                    return Ok(());
                }
                Ok(s) if s.rsplit_once(") ").is_some_and(|(_, s)| s.starts_with('Z')) => {
                    self.active.store(false, Ordering::Release);
                    return Ok(());
                }
                _ => std::thread::sleep(Duration::from_millis(5)),
            }
        }
        Err(io::Error::other("AirPlay output did not close"))
    }
}

#[derive(Debug, PartialEq)]
pub enum Action {
    Begin,
    End,
    Volume(u16),
}
pub struct Request {
    pub action: Action,
    pub token: String,
    pub reply: oneshot::Sender<bool>,
}
pub fn parse(command: &str) -> Option<(Action, String)> {
    let mut parts = command.split_whitespace();
    if parts.next()? != "AIRPLAY" {
        return None;
    }
    let kind = parts.next()?;
    let token = parts.next()?;
    if uuid::Uuid::parse_str(token).is_err() {
        return None;
    }
    let action = match kind {
        "BEGIN" => Action::Begin,
        "END" => Action::End,
        "VOLUME" => {
            let db: f64 = parts.next()?.parse().ok()?;
            if !db.is_finite() || !((-30.0..=0.0).contains(&db) || db == -144.0) {
                return None;
            }
            Action::Volume(if db == -144.0 {
                0
            } else {
                ((db + 30.0) / 30.0 * 100.0).round() as u16
            })
        }
        _ => return None,
    };
    parts.next().is_none().then(|| (action, token.into()))
}
pub fn hook(kind: &str, volume: Option<&str>) -> io::Result<()> {
    let result = hook_request(kind, volume);
    if result.is_err() && kind == "begin" {
        let parent = unsafe { libc::getppid() };
        if fs::read_link(format!("/proc/{parent}/exe")).is_ok_and(|p| {
            fs::canonicalize(
                Path::new(crate::installation::BASE).join("airplay/bin/shairport-sync"),
            )
            .is_ok_and(|expected| p == expected)
        }) {
            unsafe {
                libc::kill(-parent, libc::SIGKILL);
            }
        }
    }
    result
}
fn hook_request(kind: &str, volume: Option<&str>) -> io::Result<()> {
    let token = std::env::var("SPOTIYA_AIRPLAY_TOKEN").map_err(io::Error::other)?;
    let command = format!(
        "AIRPLAY {} {}{}",
        kind.to_uppercase(),
        token,
        volume.map(|v| format!(" {v}")).unwrap_or_default()
    );
    if parse(&command).is_none() {
        return Err(io::Error::other("Invalid AirPlay hook"));
    }
    let mut socket = std::os::unix::net::UnixStream::connect(crate::voice::CONTROL)?;
    socket.set_read_timeout(Some(Duration::from_secs(5)))?;
    socket.set_write_timeout(Some(Duration::from_secs(2)))?;
    writeln!(socket, "{command}")?;
    let mut reply = String::new();
    io::BufReader::new(socket).read_line(&mut reply)?;
    if reply.trim() == "OK" {
        Ok(())
    } else {
        Err(io::Error::other("AirPlay handover rejected"))
    }
}
fn quoted(value: &str) -> String {
    format!(
        "\"{}\"",
        value
            .chars()
            .filter(|c| !c.is_control())
            .flat_map(|c| {
                if matches!(c, '\\' | '"') {
                    vec!['\\', c]
                } else {
                    vec![c]
                }
            })
            .collect::<String>()
    )
}
const LOG: &str = "/tmp/spotiya-airplay.log";

pub struct Runtime {
    assets: PathBuf,
    runtime: PathBuf,
    setting: PathBuf,
    name: String,
    audio: profiles::Audio,
    children: Vec<Child>,
    token: String,
    retry_at: Instant,
    trimmed: Instant,
    pub enabled: bool,
    pub available: bool,
    pub error: String,
}
impl Runtime {
    pub fn new(base: &Path, state: &Path, name: &str, audio: &profiles::Audio) -> Self {
        let assets = base.join("airplay/bin");
        let available = crate::addons::airplay_available(base);
        let setting = state.join("airplay-enabled");
        let enabled = available && fs::read_to_string(&setting).is_ok_and(|s| s.trim() == "1");
        Self {
            assets,
            runtime: "/run/spotiya-airplay".into(),
            setting,
            name: name.into(),
            audio: audio.clone(),
            children: vec![],
            token: String::new(),
            retry_at: Instant::now(),
            trimmed: Instant::now(),
            enabled,
            available,
            error: String::new(),
        }
    }
    pub fn valid(&self, token: &str) -> bool {
        self.enabled && !self.token.is_empty() && self.token == token
    }
    pub fn running(&self) -> bool {
        self.children.len() == 4 && self.error.is_empty()
    }
    pub fn set_enabled(&mut self, enabled: bool) -> io::Result<()> {
        self.available =
            crate::addons::airplay_available(self.assets.parent().unwrap().parent().unwrap());
        if enabled && !self.available {
            return Err(io::Error::other(
                "Install the optional AirPlay 2 package first",
            ));
        }
        let temp = self.setting.with_extension("new");
        fs::write(&temp, if enabled { "1\n" } else { "0\n" })?;
        fs::rename(temp, &self.setting)?;
        self.enabled = enabled;
        self.retry_at = Instant::now();
        Ok(())
    }
    fn spawn(&mut self, program: &str, args: &[&str]) -> io::Result<u32> {
        let output = crate::supervisor::open_log(Path::new(LOG))?;
        let mut cmd = Command::new(self.assets.join(program));
        cmd.args(args)
            .env(
                "DBUS_SYSTEM_BUS_ADDRESS",
                format!("unix:path={}/bus", self.runtime.display()),
            )
            .env("SPOTIYA_AIRPLAY_TOKEN", &self.token)
            .stdin(Stdio::null())
            .stdout(output.try_clone()?)
            .stderr(output)
            .kill_on_drop(true)
            .process_group(0);
        if program == "shairport-sync" {
            cmd.env("LD_PRELOAD", self.runtime.join("visual.so"))
                .env("SPOTIYA_SPECTRUM_SOCKET", crate::spectrum::AIRPLAY_SOCKET);
        }
        unsafe {
            cmd.pre_exec(|| {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::getppid() == 1 {
                    return Err(io::Error::other("Receiver exited"));
                }
                Ok(())
            });
        }
        let child = cmd.spawn()?;
        let pid = child
            .id()
            .ok_or_else(|| io::Error::other("No AirPlay process PID"))?;
        self.children.push(child);
        Ok(pid)
    }
    async fn start(&mut self, gate: &Gate) -> io::Result<()> {
        fs::create_dir_all(&self.runtime)?;
        fs::write(
            self.runtime.join("visual.so.new"),
            include_bytes!(concat!(env!("OUT_DIR"), "/airplay-tap.so")),
        )?;
        fs::rename(
            self.runtime.join("visual.so.new"),
            self.runtime.join("visual.so"),
        )?;
        fs::set_permissions(&self.runtime, fs::Permissions::from_mode(0o700))?;
        let _ = fs::remove_file(self.runtime.join("bus"));
        let bus = format!(
            "<busconfig><type>system</type><listen>unix:path={}/bus</listen><auth>EXTERNAL</auth><policy user=\"root\"><allow own=\"*\"/><allow send_destination=\"*\"/><allow receive_sender=\"*\"/></policy></busconfig>\n",
            self.runtime.display()
        );
        fs::write(self.runtime.join("dbus.conf"), bus)?;
        let host_file = self.setting.with_file_name("airplay-hostname");
        let hostname = fs::read_to_string(&host_file)
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| {
                s.starts_with("spotiya-airplay-")
                    && s.len() <= 63
                    && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
            })
            .unwrap_or_else(|| {
                format!(
                    "spotiya-airplay-{}",
                    &uuid::Uuid::new_v4().simple().to_string()[..12]
                )
            });
        fs::write(&host_file, format!("{hostname}\n"))?;
        fs::write(
            self.runtime.join("avahi.conf"),
            format!(
                "[server]\nhost-name={hostname}\nuse-ipv4=yes\nuse-ipv6=yes\nenable-dbus=yes\ndisallow-other-stacks=no\n[wide-area]\nenable-wide-area=no\n[publish]\npublish-hinfo=no\npublish-workstation=no\n[reflector]\nenable-reflector=no\n"
            ),
        )?;
        let binary = std::env::current_exe()?;
        let begin = quoted(&format!("{} --airplay-event begin", binary.display()));
        let end = quoted(&format!("{} --airplay-event end", binary.display()));
        let volume = quoted(&format!("{} --airplay-event volume", binary.display()));
        let config = format!(
            "general = {{ name = {}; output_backend = \"alsa\"; mdns_backend = \"avahi\"; port = 7000; ignore_volume_control = \"yes\"; run_this_when_volume_is_set = {volume}; }};\nalsa = {{ output_device = {}; output_rate = \"auto\"; output_format = \"S16\"; }};\nsessioncontrol = {{ run_this_before_play_begins = {begin}; run_this_after_play_ends = {end}; wait_for_completion = \"yes\"; }};\n",
            quoted(&self.name),
            quoted(&self.audio.device)
        );
        fs::write(self.runtime.join("shairport.conf"), config)?;
        self.token = uuid::Uuid::new_v4().to_string();
        self.spawn(
            "dbus-daemon",
            &[
                "--nofork",
                "--nopidfile",
                "--config-file",
                &self.runtime.join("dbus.conf").to_string_lossy(),
            ],
        )?;
        let deadline = Instant::now() + Duration::from_secs(2);
        while !self.runtime.join("bus").exists() {
            if Instant::now() >= deadline {
                return Err(io::Error::other("Private AirPlay D-Bus did not start"));
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        self.spawn(
            "avahi-daemon",
            &[
                "--no-drop-root",
                "--no-rlimits",
                "--file",
                &self.runtime.join("avahi.conf").to_string_lossy(),
            ],
        )?;
        self.spawn("nqptp", &[])?;
        tokio::time::sleep(Duration::from_millis(200)).await;
        let pid = self.spawn(
            "shairport-sync",
            &["-c", &self.runtime.join("shairport.conf").to_string_lossy()],
        )?;
        *gate.pid.lock().expect("AirPlay process lock") = pid;
        Ok(())
    }
    pub async fn stop(&mut self, gate: &Gate) {
        self.token.clear();

        for child in self.children.iter_mut().rev() {
            if let Some(pid) = child.id() {
                unsafe {
                    libc::kill(-(pid as i32), libc::SIGTERM);
                }
            }
            let deadline = Instant::now() + Duration::from_millis(700);
            loop {
                let finished = {
                    let mut pid = gate.pid.lock().expect("AirPlay process lock");
                    let receiver = child.id() == Some(*pid);
                    let finished = !matches!(child.try_wait(), Ok(None));
                    if finished && receiver {
                        *pid = 0;
                        gate.active.store(false, Ordering::Release);
                    }
                    finished
                };
                if finished {
                    break;
                }
                if Instant::now() >= deadline
                    && let Some(pid) = child.id()
                {
                    unsafe {
                        libc::kill(-(pid as i32), libc::SIGKILL);
                    }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
        self.children.clear();
        *gate.pid.lock().expect("AirPlay process lock") = 0;
        gate.active.store(false, Ordering::Release);
    }
    pub async fn poll(&mut self, gate: &Gate) {
        if !self.children.is_empty() && self.trimmed.elapsed() >= Duration::from_secs(30) {
            self.trimmed = Instant::now();
            if let Err(e) = crate::supervisor::trim_log(Path::new(LOG)) {
                eprintln!("airplay: log trim failed: {e}");
            }
        }
        if !self.available {
            self.available =
                crate::addons::airplay_available(self.assets.parent().unwrap().parent().unwrap());
        }
        let failed = {
            let mut pid = gate.pid.lock().expect("AirPlay process lock");
            let mut failed = false;
            for child in &mut self.children {
                let receiver = child.id() == Some(*pid);
                if !matches!(child.try_wait(), Ok(None)) {
                    failed = true;
                    if receiver {
                        *pid = 0;
                        gate.active.store(false, Ordering::Release);
                    }
                }
            }
            failed
        };
        if failed || (!self.enabled && !self.children.is_empty()) {
            self.stop(gate).await;
            if failed && !gate.preempted.swap(false, Ordering::AcqRel) {
                self.error = "AirPlay service exited; retrying".into();
                self.retry_at = Instant::now() + Duration::from_secs(3);
            } else {
                self.error.clear();
                self.retry_at = Instant::now();
            }
        }
        if self.enabled && self.children.is_empty() && Instant::now() >= self.retry_at {
            self.error.clear();
            if let Err(e) = self.start(gate).await {
                self.error = e.to_string();
                self.stop(gate).await;
                self.retry_at = Instant::now() + Duration::from_secs(10);
                log::warn!("airplay: {}", self.error);
            }
        }
    }
    pub fn status(&self, status: &mut control::Status, gate: &Gate) {
        status.airplay_available = self.available;
        status.airplay_enabled = self.enabled;
        status.airplay_running = self.running();
        status.airplay_active = gate.active.load(Ordering::Acquire);
        status.airplay_error.clone_from(&self.error);
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        for child in &self.children {
            if let Some(pid) = child.id() {
                unsafe {
                    libc::kill(-(pid as i32), libc::SIGKILL);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn handover_waits_for_the_spotify_sink_to_close() {
        let gate = std::sync::Arc::new(Gate::new());
        assert!(gate.spotify_released(Instant::now()).await);
        gate.spotify_open.store(true, Ordering::Release);
        assert!(
            !gate
                .spotify_released(Instant::now() + Duration::from_millis(20))
                .await
        );
        let sink = gate.clone();
        let closer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            sink.close_spotify();
        });
        assert!(
            gate.spotify_released(Instant::now() + Duration::from_secs(5))
                .await
        );
        closer.join().unwrap();
    }
    #[test]
    fn hooks_reject_stale_or_malformed_values() {
        let token = uuid::Uuid::new_v4();
        for (db, p) in [("-144", 0), ("-30", 0), ("-15", 50), ("0", 100)] {
            assert_eq!(
                parse(&format!("AIRPLAY VOLUME {token} {db}")),
                Some((Action::Volume(p), token.to_string()))
            );
        }
        for db in ["NaN", "inf", "1", "-31", "0 EXTRA"] {
            assert!(parse(&format!("AIRPLAY VOLUME {token} {db}")).is_none());
        }
        assert!(parse("AIRPLAY BEGIN bad").is_none());
        assert!(parse(&format!("AIRPLAY BEGIN {token} extra")).is_none());
        assert_eq!(quoted("a\"\\\nb"), "\"a\\\"\\\\b\"");
    }
    #[test]
    fn missing_payload_cannot_enable_or_accept_a_hook() {
        let dir = std::env::temp_dir().join(format!("airplay-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let profile = crate::profiles::load(Some("generic"), &dir).unwrap();
        let mut runtime = Runtime::new(&dir, &dir, "Test", &profile.profile.audio);
        assert!(!runtime.available && !runtime.enabled && !runtime.valid(""));
        assert!(runtime.set_enabled(true).is_err());
        runtime.set_enabled(false).unwrap();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn spotify_reclaims_external_output_before_opening_its_sink() {
        use std::os::unix::process::CommandExt;
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("60")
            .process_group(0)
            .spawn()
            .unwrap();
        let gate = Gate::new();
        *gate.pid.lock().unwrap() = child.id();
        gate.active.store(true, Ordering::Release);
        gate.reclaim().unwrap();
        assert!(!gate.active.load(Ordering::Acquire));
        assert!(!child.wait().unwrap().success());
    }

    #[tokio::test]
    async fn failed_helper_stops_the_whole_private_stack_and_disable_survives_restart() {
        let dir = std::env::temp_dir().join(format!("airplay-stack-{}", uuid::Uuid::new_v4()));
        let bins = dir.join("airplay/bin");
        fs::create_dir_all(&bins).unwrap();
        let script = "#!/usr/bin/python3\nimport os,socket,time\nif os.path.basename(__file__)=='dbus-daemon':\n s=socket.socket(socket.AF_UNIX);s.bind(os.environ['DBUS_SYSTEM_BUS_ADDRESS'].split('unix:path=')[1]);s.listen()\nwhile True: time.sleep(1)\n";
        for name in ["dbus-daemon", "avahi-daemon", "nqptp", "shairport-sync"] {
            fs::write(bins.join(name), script).unwrap();
            fs::set_permissions(bins.join(name), fs::Permissions::from_mode(0o755)).unwrap();
        }
        let profile = crate::profiles::load(Some("generic"), &dir).unwrap();
        let mut runtime = Runtime::new(&dir, &dir, "Test", &profile.profile.audio);
        runtime.runtime = dir.join("run");
        runtime.set_enabled(true).unwrap();
        let gate = Gate::new();
        runtime.poll(&gate).await;
        assert!(runtime.running(), "{}", runtime.error);
        assert!(runtime.valid(&runtime.token));
        assert!(!runtime.valid(&uuid::Uuid::new_v4().to_string()));
        let pids: Vec<_> = runtime.children.iter().map(|c| c.id().unwrap()).collect();
        gate.active.store(true, Ordering::Release);
        runtime.children[2].start_kill().unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        runtime.poll(&gate).await;
        assert!(!runtime.running() && !runtime.error.is_empty());
        assert!(!gate.active.load(Ordering::Acquire));
        for pid in pids {
            assert!(!Path::new(&format!("/proc/{pid}")).exists());
        }
        runtime.set_enabled(false).unwrap();
        assert!(!Runtime::new(&dir, &dir, "Test", &profile.profile.audio).enabled);
        fs::remove_dir_all(dir).unwrap();
    }
}
