//! Hand short effects to Quasar's foreground compositor. Its local S3-format loader
//! accepts a newline-separated meta.txt and firmware .led assets over HTTP.
//! Only generated lighting assets are served, on loopback, without filesystem routing.
use base64::{Engine, engine::general_purpose::STANDARD};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    io::{self, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

const ADDRESS: &str = "127.0.0.1:38451";

pub struct Native {
    paths: HashMap<String, String>,
    address: String,
    stop: Arc<AtomicBool>,
    server: Option<thread::JoinHandle<()>>,
}

fn var(out: &mut Vec<u8>, mut value: u64) {
    while value >= 128 {
        out.push(value as u8 | 128);
        value >>= 7;
    }
    out.push(value as u8);
}
fn number(out: &mut Vec<u8>, key: u64, value: u64) {
    var(out, key << 3);
    var(out, value);
}
fn bytes(out: &mut Vec<u8>, key: u64, value: &[u8]) {
    var(out, key << 3 | 2);
    var(out, value.len() as u64);
    out.extend_from_slice(value);
}

fn request(address: &str, path: &str, clock: Option<&[u8]>) -> Vec<u8> {
    let mut directory = vec![];
    bytes(&mut directory, 1, format!("http://{address}").as_bytes());
    bytes(&mut directory, 2, path.as_bytes());
    let mut animation = vec![];
    bytes(&mut animation, 2, &directory);
    let mut draw = vec![];
    bytes(&mut draw, 1, &animation);
    if let Some(clock) = clock {
        // The file loader expects textual SCLED; this binary interface takes raw frames.
        let mut binary = vec![];
        number(&mut binary, 1, 1); // Uncompressed.
        bytes(&mut binary, 2, STANDARD.encode(clock).as_bytes());
        let mut animation = vec![];
        bytes(&mut animation, 1, &binary);
        bytes(&mut draw, 1, &animation);
    }
    number(&mut draw, 2, 1); // PlayOnce, bounded by the supplied frames.
    let mut directive = vec![];
    bytes(&mut directive, 1, b"draw_animation_directive");
    bytes(&mut directive, 3, b"client_action");
    bytes(&mut directive, 25, &draw); // Protobuf payload, not JSON.
    let mut message = vec![];
    bytes(&mut message, 252, &directive);
    number(&mut message, 253, 211); // TYPE_DIRECTIVE on aliced's local connector.
    let mut frame = (message.len() as u32).to_le_bytes().to_vec();
    frame.extend(message);
    frame
}

pub type Bundle = (String, Vec<(String, Vec<u8>)>);
impl Native {
    pub fn start(model: &str, bundles: Vec<Bundle>) -> io::Result<Self> {
        Self::start_on(model, bundles, ADDRESS)
    }
    pub fn preview(model: &str, bundles: Vec<Bundle>) -> io::Result<Self> {
        Self::start_on(model, bundles, "127.0.0.1:38452")
    }
    fn start_on(model: &str, bundles: Vec<Bundle>, address: &str) -> io::Result<Self> {
        let listener = TcpListener::bind(address)?;
        listener.set_nonblocking(true)?;
        let mut assets = HashMap::new();
        let mut paths = HashMap::new();
        for (name, files) in bundles {
            if files.is_empty() {
                continue;
            }
            let mut digest = Sha256::new();
            for (file, data) in &files {
                digest.update(file);
                digest.update(data);
            }
            let path = format!("spotiya-led-v1/{:x}/{name}", digest.finalize());
            let mut meta = String::new();
            for (file, data) in files {
                let file = format!("{path}/{model}/{file}");
                meta.push_str(&file);
                meta.push('\n');
                assets.insert(format!("/{file}"), data);
            }
            assets.insert(format!("/{path}/{model}/meta.txt"), meta.into_bytes());
            paths.insert(name, path);
        }
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let server = thread::Builder::new()
            .name("spotiya-led-assets".into())
            .spawn(move || {
                while !stopping.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((mut stream, peer)) if peer.ip().is_loopback() => {
                            let _ = serve(&mut stream, &assets);
                        }
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(5))
                        }
                        Err(_) => break,
                        _ => {}
                    }
                }
            })?;
        Ok(Self {
            paths,
            address: address.into(),
            stop,
            server: Some(server),
        })
    }
    pub fn show(&self, name: &str, clock: Option<&[u8]>) -> io::Result<()> {
        let path = self
            .paths
            .get(name)
            .ok_or_else(|| io::Error::other("Missing native LED effect"))?;
        let address: SocketAddr = "127.0.0.1:9875".parse().unwrap();
        let mut stream = TcpStream::connect_timeout(&address, Duration::from_millis(150))?;
        stream.set_write_timeout(Some(Duration::from_millis(150)))?;
        stream.write_all(&request(&self.address, path, clock))
    }
}
impl Drop for Native {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(server) = self.server.take() {
            let _ = server.join();
        }
    }
}

fn serve(stream: &mut TcpStream, assets: &HashMap<String, Vec<u8>>) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_millis(200)))?;
    stream.set_write_timeout(Some(Duration::from_millis(200)))?;
    let deadline = std::time::Instant::now() + Duration::from_millis(200);
    let mut header = Vec::new();
    let mut buf = [0; 512];
    while header.len() < 4096 && !header.windows(4).any(|s| s == b"\r\n\r\n") {
        let remaining = deadline
            .checked_duration_since(std::time::Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or_else(|| io::Error::from(io::ErrorKind::TimedOut))?;
        stream.set_read_timeout(Some(remaining))?;
        let size = buf.len().min(4096 - header.len());
        let n = stream.read(&mut buf[..size])?;
        if n == 0 {
            return Ok(());
        }
        header.extend_from_slice(&buf[..n]);
    }
    let path = std::str::from_utf8(&header)
        .ok()
        .and_then(|s| s.lines().next())
        .and_then(|s| s.strip_prefix("GET "))
        .and_then(|s| {
            s.strip_suffix(" HTTP/1.1")
                .or_else(|| s.strip_suffix(" HTTP/1.0"))
        });
    let body = path.and_then(|p| assets.get(p));
    let (status, body) = body.map_or(("404 Not Found", &[][..]), |b| ("200 OK", b.as_slice()));
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_envelope_is_length_prefixed_and_bounded() {
        let frame = request(
            ADDRESS,
            "spotiya-led-v1/test/volume-50",
            Some(&crate::led_clock::animation(50, false)),
        );
        assert_eq!(
            u32::from_le_bytes(frame[..4].try_into().unwrap()) as usize,
            frame.len() - 4
        );
        assert!(frame.windows(24).any(|b| b == b"draw_animation_directive"));
        assert!(frame.len() < 512);
    }
    #[test]
    fn server_only_serves_explicit_assets() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = thread::spawn(move || {
            let assets = HashMap::from([("/one/panel.led".into(), b"abc".to_vec())]);
            for _ in 0..2 {
                let (mut s, _) = listener.accept().unwrap();
                serve(&mut s, &assets).unwrap();
            }
        });
        for (path, status) in [
            ("/one/panel.led", "200 OK"),
            ("/../../etc/passwd", "404 Not Found"),
        ] {
            let mut client = TcpStream::connect(address).unwrap();
            write!(client, "GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
            let mut response = String::new();
            client.read_to_string(&mut response).unwrap();
            assert!(response.starts_with(&format!("HTTP/1.1 {status}\r\n")));
        }
        worker.join().unwrap();
    }
}
