use std::{collections::VecDeque, io, net::SocketAddr, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::mpsc,
    time::Instant,
};

pub const CONTROL: &str = "/run/spotiya.control";
const MAX_FRAME: usize = 512 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
    Play(String),
    Resume,
    Pause,
    Next,
    Previous,
    Toggle,
    Volume(u16),
    Voice(bool),
    Controls(bool),
    Spectrum(bool),
    Airplay(bool),
    Release,
    Retry,
}
pub struct Event {
    pub intent: Intent,
    pub received: Instant,
    pub from_voice: bool,
    pub quick: bool,
    pub request_id: Option<String>,
}

pub fn intent(text: &str) -> Option<Intent> {
    let text = text
        .trim()
        .trim_matches(['.', '!', '?'])
        .to_lowercase()
        .replace('ё', "е");
    let text = text.strip_prefix("алиса, ").unwrap_or(&text).trim();
    match text {
        "стоп"
        | "пауза"
        | "останови музыку"
        | "выключи музыку"
        | "поставь на паузу"
        | "останови воспроизведение" => return Some(Intent::Pause),
        "дальше" | "следующий" | "следующий трек" | "следующая песня" | "переключи трек" =>
        {
            return Some(Intent::Next);
        }
        "назад" | "предыдущий трек" | "предыдущая песня" => {
            return Some(Intent::Previous);
        }
        "продолжи" | "продолжи музыку" | "продолжай музыку" | "включи музыку" =>
        {
            return Some(Intent::Resume);
        }
        _ => {}
    }
    let rest = ["включи ", "поставь ", "запусти ", "сыграй "]
        .iter()
        .find_map(|p| text.strip_prefix(p))?
        .trim();
    let music_prefix = [
        "песню ",
        "трек ",
        "музыку ",
        "группу ",
        "исполнителя ",
        "альбом ",
        "плейлист ",
    ];
    let explicit = music_prefix.iter().find_map(|p| rest.strip_prefix(p));
    let query = explicit.unwrap_or(rest).trim().trim_matches('"').trim();
    if query.is_empty() || query.len() > 400 {
        return None;
    }
    let home = [
        "свет",
        "освещение",
        "ламп",
        "люстр",
        "торшер",
        "ночник",
        "розетк",
        "телевизор",
        "тв",
        "кондиционер",
        "вентилятор",
        "обогревател",
        "пылесос",
        "робот",
        "чайник",
        "будильник",
        "таймер",
        "блютуз",
        "bluetooth",
        "режим",
        "экран",
        "микрофон",
        "громк",
        "звук",
        "отоплен",
        "увлажнител",
        "очистител",
        "зарядк",
    ];
    if explicit.is_none()
        && query
            .split_whitespace()
            .any(|word| home.iter().any(|p| word.starts_with(p)))
    {
        return None;
    }
    Some(Intent::Play(query.to_owned()))
}

fn native_volume_request(text: &str) -> bool {
    let normalized = text.trim().to_lowercase().replace('ё', "е");
    let text = normalized.strip_prefix("алиса, ").unwrap_or(&normalized);
    let mut words = text.split_whitespace();
    let Some(first) = words.next() else {
        return false;
    };
    if matches!(
        first.trim_end_matches(['.', '!', '?']),
        "громче" | "тише" | "погромче" | "потише"
    ) {
        return true;
    }
    if first == "громкость" {
        return words.next().is_some();
    }
    matches!(
        first,
        "сделай"
            | "поставь"
            | "установи"
            | "измени"
            | "увеличь"
            | "уменьши"
            | "добавь"
            | "убавь"
            | "прибавь"
            | "включи"
            | "выключи"
            | "отключи"
    ) && words.next().is_some_and(|word| {
        matches!(
            word,
            "звук" | "громкость" | "громче" | "тише" | "погромче" | "потише"
        )
    })
}

// QuasarMessage uses {252: serialized payload, 253: message type}.
// Read AliceState activity and final voice recognition without retaining other payloads.
fn varint(data: &[u8], offset: &mut usize) -> Option<u64> {
    let mut value = 0u64;
    for shift in (0..70).step_by(7) {
        let byte = *data.get(*offset)?;
        *offset += 1;
        if shift == 63 && byte > 1 {
            return None;
        }
        value |= u64::from(byte & 127) << shift;
        if byte < 128 {
            return Some(value);
        }
    }
    None
}
#[derive(Clone, Copy)]
enum Value<'a> {
    Number(u64),
    Bytes(&'a [u8]),
}
fn field(data: &[u8], wanted: u64) -> Option<Value<'_>> {
    let mut offset = 0;
    while offset < data.len() {
        let key = varint(data, &mut offset)?;
        if key >> 3 == 0 {
            return None;
        }
        let value = match key & 7 {
            0 => Value::Number(varint(data, &mut offset)?),
            2 => {
                let len = usize::try_from(varint(data, &mut offset)?).ok()?;
                let end = offset.checked_add(len)?;
                let bytes = data.get(offset..end)?;
                offset = end;
                Value::Bytes(bytes)
            }
            1 | 5 => {
                let len = if key & 7 == 1 { 8 } else { 4 };
                offset = offset.checked_add(len)?;
                if offset > data.len() {
                    return None;
                }
                continue;
            }
            _ => return None,
        };
        if key >> 3 == wanted {
            return Some(value);
        }
    }
    None
}
fn bytes(data: &[u8], number: u64) -> Option<&[u8]> {
    if let Value::Bytes(v) = field(data, number)? {
        Some(v)
    } else {
        None
    }
}
fn number(data: &[u8], field_number: u64) -> Option<u64> {
    if let Value::Number(v) = field(data, field_number)? {
        Some(v)
    } else {
        None
    }
}
fn alice_busy(data: &[u8]) -> Option<bool> {
    if number(data, 253) != Some(3) {
        return None;
    }
    let state = bytes(data, 252)?;
    match number(state, 1).unwrap_or(0) {
        0 | 6 => Some(false),        // IDLE / NONE
        1 | 2 | 3 | 5 => Some(true), // LISTENING / SPEAKING / BUSY / SHAZAM
        _ => None,
    }
}
fn recognized(data: &[u8]) -> Option<(String, String)> {
    if number(data, 253) != Some(3) {
        return None;
    }
    let state = bytes(data, 252)?;
    // Quasar marks the phrase final while still LISTENING, about a second before it turns
    // BUSY. Acting then lets the cancel reach the server before Alice's own reply, such as
    // a Yandex Plus refusal, which would otherwise be spoken.
    if !matches!(number(state, 1), Some(1 | 3)) {
        return None;
    }
    let result = bytes(state, 15)?;
    if number(result, 2) != Some(1) || number(result, 4) != Some(1) {
        return None;
    }
    let text = std::str::from_utf8(bytes(result, 1)?).ok()?;
    if text.is_empty() || text.len() > 512 || Some(text.as_bytes()) != bytes(state, 2) {
        return None;
    }
    let request = std::str::from_utf8(bytes(state, 7)?).ok()?;
    if request.len() != 36 {
        return None;
    }
    Some((request.to_owned(), text.to_owned()))
}
// Spotter notifications are separate from Alice's ASR. Only the native command
// capability's onPhraseSpotted notification is accepted; activation words,
// playback RPCs (including our own audio guard), and arbitrary text are ignored.
fn quick_command(data: &[u8]) -> Option<Intent> {
    if number(data, 253) != Some(210) {
        return None;
    }
    let remoting = bytes(data, 252)?;
    if bytes(remoting, 7)? != b"CommandSpotterCapability" {
        return None;
    }
    let notification = bytes(remoting, 15)?;
    if number(notification, 1) != Some(2) {
        return None;
    }
    match std::str::from_utf8(bytes(notification, 3)?).ok()? {
        "стоп" | "пауза" | "хватит" => Some(Intent::Pause),
        "дальше" | "следующий" => Some(Intent::Next),
        "назад" => Some(Intent::Previous),
        "продолжи" | "играй" => Some(Intent::Resume),
        _ => None,
    }
}
fn put_var(out: &mut Vec<u8>, mut v: u64) {
    while v >= 128 {
        out.push((v as u8 & 127) | 128);
        v >>= 7;
    }
    out.push(v as u8);
}
fn put_number(out: &mut Vec<u8>, field: u64, value: u64) {
    put_var(out, field << 3);
    put_var(out, value);
}
fn put_bytes(out: &mut Vec<u8>, field: u64, value: &[u8]) {
    put_var(out, (field << 3) | 2);
    put_var(out, value.len() as u64);
    out.extend_from_slice(value);
}
fn rpc(capability: &str, field: u64, method: &[u8]) -> Vec<u8> {
    let mut remoting = Vec::new();
    put_bytes(&mut remoting, 7, capability.as_bytes());
    put_bytes(&mut remoting, field, method);
    let mut message = Vec::new();
    put_bytes(&mut message, 252, &remoting);
    put_number(&mut message, 253, 210);
    let mut frame = (message.len() as u32).to_le_bytes().to_vec();
    frame.extend_from_slice(&message);
    frame
}
pub async fn cancel_alice(address: SocketAddr, request_id: Option<&str>) -> io::Result<()> {
    // AliceCapabilityProxy::cancelDialog(false, requestId), verified against firmware.
    // Kind 3 is stopConversation: during final ASR it can leave the phrase spotter
    // stopped even after Alice reports IDLE. Kind 1 cancels the request normally.
    let mut method = vec![8, 1];
    put_number(&mut method, 7, 0);
    if let Some(request_id) = request_id {
        put_bytes(&mut method, 10, request_id.as_bytes());
    }
    send_native(address, rpc("AliceCapability", 8, &method)).await
}
pub async fn start_activation(address: SocketAddr) -> io::Result<()> {
    // ActivationSpotterCapability::start(), not enable(): native mute/disabled checks remain.
    send_native(address, rpc("ActivationSpotterCapability", 14, &[8, 2])).await
}
pub async fn pause_native(address: SocketAddr) -> io::Result<()> {
    // Native CLI 'o': PlaybackControlCapabilityMethod kind 1 (pause).
    let mut source = Vec::new();
    put_number(&mut source, 1, 14);
    put_number(&mut source, 2, 3);
    put_number(&mut source, 3, 1);
    put_bytes(&mut source, 4, uuid::Uuid::new_v4().to_string().as_bytes());
    put_bytes(&mut source, 6, b"SpotifyVoice");
    let mut method = vec![8, 1];
    put_bytes(&mut method, 5, &source);
    put_number(&mut method, 7, 0);
    send_native(address, rpc("PlaybackControlCapability", 13, &method)).await
}
pub async fn prepare_playback(
    address: Option<SocketAddr>,
    from_voice: bool,
    request_id: Option<&str>,
) -> io::Result<()> {
    if let Some(address) = address {
        if from_voice && request_id.is_some() {
            cancel_alice(address, request_id).await?;
        }
        let _ = pause_native(address).await;
    }
    Ok(())
}
pub fn playable_track(text: &str) -> Option<String> {
    use librespot::core::spotify_uri::SpotifyUri;
    let uri = SpotifyUri::from_uri(text.trim()).ok()?;
    match uri {
        SpotifyUri::Track { .. } | SpotifyUri::Episode { .. } => Some(uri.to_uri()),
        _ => None,
    }
}
async fn send_native(address: SocketAddr, frame: Vec<u8>) -> io::Result<()> {
    tokio::time::timeout(Duration::from_millis(500), async {
        let mut socket = TcpStream::connect(address).await?;
        socket.write_all(&frame).await?;
        socket.shutdown().await
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "native command timeout"))?
}
async fn read_events(
    socket: &mut (impl tokio::io::AsyncRead + Unpin),
    sender: &mpsc::Sender<Event>,
    ducking: &crate::ducking::Ducking,
    activation: &crate::voice_recovery::Activation,
    seen: &mut VecDeque<String>,
) -> io::Result<()> {
    let mut data = Vec::with_capacity(4096);
    let mut latest_request: Option<String> = None;
    let mut pending_quick: Option<Event> = None;
    loop {
        let size = socket.read_u32_le().await? as usize;
        if size == 0 || size > MAX_FRAME {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "native frame length",
            ));
        }
        data.resize(size, 0);
        socket.read_exact(&mut data).await?;
        if let Some(busy) = alice_busy(&data) {
            ducking.set_busy(busy);
            let state = bytes(&data, 252).unwrap_or_default();
            let idle = match number(state, 1).unwrap_or(0) {
                0 => Some(true),
                1 | 2 | 3 | 5 => Some(false),
                _ => None,
            };
            let request = bytes(state, 7)
                .and_then(|b| std::str::from_utf8(b).ok())
                .filter(|s| !s.is_empty());
            activation.observe(idle, request);
            latest_request = request.filter(|s| s.len() == 36).map(str::to_owned);
            if pending_quick.as_ref().is_some_and(|event| {
                event.received.elapsed() >= Duration::from_secs(2)
                    || event.request_id.as_deref() != request
                    || matches!(number(state, 1), Some(1 | 2 | 5))
            }) {
                pending_quick = None;
            }
            if number(state, 1) == Some(3)
                && let Some(event) = pending_quick.take()
                && let Some(id) = request
                && !seen.iter().any(|s| s == id)
            {
                seen.push_back(id.to_owned());
                if seen.len() > 64 {
                    seen.pop_front();
                }
                let _ = sender.try_send(event);
            }
        }
        if let Some((request, text)) = recognized(&data)
            && !seen.contains(&request)
        {
            seen.push_back(request.clone());
            if seen.len() > 64 {
                seen.pop_front();
            }
            if native_volume_request(&text) {
                ducking.volume.native_command();
            }
            if let Some(intent) = intent(&text) {
                let _ = sender.try_send(Event {
                    intent,
                    received: Instant::now(),
                    from_voice: true,
                    quick: false,
                    request_id: Some(request),
                });
            }
        }
        if !ducking.active()
            && latest_request.is_some()
            && let Some(intent) = quick_command(&data)
        {
            // The firmware announces the request ID in IDLE, then the spotted
            // phrase, then BUSY. Wait for that exact request so cancellation and
            // activation recovery cannot affect a different conversation.
            pending_quick = Some(Event {
                intent,
                received: Instant::now(),
                from_voice: true,
                quick: true,
                request_id: latest_request.clone(),
            });
        }
        if data.capacity() > 64 * 1024 {
            data = Vec::with_capacity(4096);
        }
    }
}
pub async fn watch(
    sender: mpsc::Sender<Event>,
    ducking: Arc<crate::ducking::Ducking>,
    activation: Arc<crate::voice_recovery::Activation>,
    address: SocketAddr,
) {
    let mut seen: VecDeque<String> = VecDeque::new();
    loop {
        let result: io::Result<()> = async {
            let mut socket = TcpStream::connect(address).await?;
            eprintln!("voice: connected to native Alice events");
            read_events(&mut socket, &sender, &ducking, &activation, &mut seen).await
        }
        .await;
        ducking.set_busy(false);
        activation.observe(None, None);
        if sender.is_closed() {
            return;
        }
        if result.is_err() {
            eprintln!("voice: native connection lost; retrying");
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}
pub fn context(query: &str) -> String {
    let mut value = String::from("spotify:search:");
    for byte in query.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                value.push(char::from(byte))
            }
            b' ' => value.push('+'),
            byte => {
                use std::fmt::Write;
                let _ = write!(value, "%{byte:02X}");
            }
        }
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn native_commands_use_the_profile_endpoint() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let receiver = tokio::spawn(async move {
            let mut objects = Vec::new();
            for _ in 0..2 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let size = socket.read_u32_le().await.unwrap();
                let mut msg = vec![0; size as usize];
                socket.read_exact(&mut msg).await.unwrap();
                assert_eq!(number(&msg, 253), Some(210));
                let remoting = bytes(&msg, 252).unwrap();
                if let Some(method) = bytes(remoting, 8) {
                    assert_eq!(
                        number(method, 1),
                        Some(1),
                        "must cancelDialog, never stopConversation"
                    );
                    assert_eq!(number(method, 7), Some(0));
                    assert_eq!(
                        bytes(method, 10),
                        Some(b"01234567-89ab-cdef-0123-456789abcdef".as_slice())
                    );
                }
                objects.push(
                    std::str::from_utf8(bytes(bytes(&msg, 252).unwrap(), 7).unwrap())
                        .unwrap()
                        .to_owned(),
                );
            }
            objects
        });
        prepare_playback(
            Some(address),
            true,
            Some("01234567-89ab-cdef-0123-456789abcdef"),
        )
        .await
        .unwrap();
        let objects = tokio::time::timeout(Duration::from_secs(2), receiver)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(objects, ["AliceCapability", "PlaybackControlCapability"]);
        prepare_playback(None, true, None).await.unwrap();
    }
    #[tokio::test]
    async fn activation_recovery_starts_without_enabling_a_disabled_spotter() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let receiver = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let size = socket.read_u32_le().await.unwrap();
            let mut msg = vec![0; size as usize];
            socket.read_exact(&mut msg).await.unwrap();
            let remoting = bytes(&msg, 252).unwrap();
            assert_eq!(number(&msg, 253), Some(210));
            assert_eq!(
                bytes(remoting, 7),
                Some(b"ActivationSpotterCapability".as_slice())
            );
            assert_eq!(bytes(remoting, 14), Some([8, 2].as_slice()));
        });
        start_activation(address).await.unwrap();
        receiver.await.unwrap();
    }
    fn spotter_frame(capability: &str, method: u64, kind: u64, phrase: &[u8]) -> Vec<u8> {
        let mut notification = Vec::new();
        put_number(&mut notification, 1, kind);
        put_bytes(&mut notification, 3, phrase);
        rpc(capability, method, &notification)
    }
    #[test]
    fn quick_commands_accept_only_command_spotter_phrase_notifications() {
        for (phrase, expected) in [
            ("стоп", Intent::Pause),
            ("пауза", Intent::Pause),
            ("хватит", Intent::Pause),
            ("дальше", Intent::Next),
            ("следующий", Intent::Next),
            ("назад", Intent::Previous),
            ("продолжи", Intent::Resume),
            ("играй", Intent::Resume),
        ] {
            let frame = spotter_frame("CommandSpotterCapability", 15, 2, phrase.as_bytes());
            assert_eq!(quick_command(&frame[4..]), Some(expected));
            for n in 4..frame.len() {
                assert_eq!(quick_command(&frame[4..n]), None);
            }
        }
        for (capability, method, kind, phrase) in [
            ("ActivationSpotterCapability", 15, 2, "стоп"),
            ("PlaybackControlCapability", 13, 1, "стоп"),
            ("CommandSpotterCapability", 14, 2, "стоп"),
            ("CommandSpotterCapability", 15, 1, "стоп"),
            ("CommandSpotterCapability", 15, 2, "алиса"),
            ("CommandSpotterCapability", 15, 2, "включи свет"),
            ("CommandSpotterCapability", 15, 2, "включи песню"),
        ] {
            let frame = spotter_frame(capability, method, kind, phrase.as_bytes());
            assert_eq!(quick_command(&frame[4..]), None);
        }
        let invalid = spotter_frame("CommandSpotterCapability", 15, 2, &[0xff]);
        assert_eq!(quick_command(&invalid[4..]), None);
    }
    #[tokio::test]
    async fn missing_request_id_never_cancels_an_unscoped_dialog() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        prepare_playback(Some(address), true, None).await.unwrap();
        let (mut socket, _) = listener.accept().await.unwrap();
        let size = socket.read_u32_le().await.unwrap();
        let mut msg = vec![0; size as usize];
        socket.read_exact(&mut msg).await.unwrap();
        assert_eq!(
            bytes(bytes(&msg, 252).unwrap(), 7),
            Some(b"PlaybackControlCapability".as_slice())
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(20), listener.accept())
                .await
                .is_err()
        );
    }
    #[tokio::test]
    async fn quick_commands_wait_for_their_own_busy_request_and_ignore_replays() {
        let (mut client, mut server) = tokio::io::duplex(8192);
        let id = "12345678-1234-1234-1234-123456789012";
        let state = |kind, id: &str| {
            let mut body = Vec::new();
            put_number(&mut body, 1, kind);
            put_bytes(&mut body, 7, id.as_bytes());
            let mut msg = Vec::new();
            put_bytes(&mut msg, 252, &body);
            put_number(&mut msg, 253, 3);
            let mut frame = (msg.len() as u32).to_le_bytes().to_vec();
            frame.extend(msg);
            frame
        };
        for (begin, end, command, request) in [
            (0, 3, "дальше", id),
            (0, 3, "дальше", id), // Duplicate native delivery.
            (1, 3, "стоп", "22345678-1234-1234-1234-123456789012"), // Existing dialog.
            (0, 3, "пауза", "32345678-1234-1234-1234-123456789012"),
            (0, 3, "назад", ""), // Missing request ID.
        ] {
            client.write_all(&state(begin, request)).await.unwrap();
            client
                .write_all(&spotter_frame(
                    "CommandSpotterCapability",
                    15,
                    2,
                    command.as_bytes(),
                ))
                .await
                .unwrap();
            client.write_all(&state(end, request)).await.unwrap();
        }
        client.write_all(&state(0, id)).await.unwrap();
        client
            .write_all(&spotter_frame(
                "CommandSpotterCapability",
                15,
                2,
                "назад".as_bytes(),
            ))
            .await
            .unwrap();
        client
            .write_all(&state(3, "42345678-1234-1234-1234-123456789012"))
            .await
            .unwrap();
        drop(client);
        let (sender, mut events) = mpsc::channel(8);
        let result = read_events(
            &mut server,
            &sender,
            &crate::ducking::Ducking::new(),
            &crate::voice_recovery::Activation::default(),
            &mut VecDeque::new(),
        )
        .await;
        assert!(result.is_err());
        for intent in [Intent::Next, Intent::Pause] {
            let event = events.try_recv().unwrap();
            assert_eq!(event.intent, intent);
            assert!(event.from_voice && event.quick);
            assert!(event.request_id.is_some());
        }
        assert!(events.try_recv().is_err());
    }
    #[test]
    fn native_volume_commands_are_distinct_from_music_and_questions() {
        for text in [
            "громче",
            "тише!",
            "погромче",
            "громкость пять",
            "громкость на 30 процентов",
            "сделай потише",
            "увеличь громкость",
            "выключи звук",
            "Алиса, установи громкость 3",
        ] {
            assert!(native_volume_request(text), "{text}");
        }
        for text in [
            "какая погода",
            "какая сейчас громкость",
            "почему музыка тихая",
            "включи песню Тише",
            "включи Radiohead",
            "поставь таймер",
        ] {
            assert!(!native_volume_request(text), "{text}");
        }
    }
    fn state_message(state: Option<u64>, kind: u64) -> Vec<u8> {
        let mut body = Vec::new();
        if let Some(state) = state {
            put_number(&mut body, 1, state);
        }
        let mut msg = Vec::new();
        put_bytes(&mut msg, 252, &body);
        put_number(&mut msg, 253, kind);
        msg
    }
    #[test]
    fn alice_activity_includes_listening_thinking_and_speech_only() {
        for state in [0, 6] {
            assert_eq!(alice_busy(&state_message(Some(state), 3)), Some(false));
        }
        for state in [1, 2, 3, 5] {
            assert_eq!(alice_busy(&state_message(Some(state), 3)), Some(true));
        }
        assert_eq!(alice_busy(&state_message(None, 3)), Some(false));
        assert_eq!(alice_busy(&state_message(Some(99), 3)), None);
        assert_eq!(alice_busy(&state_message(Some(1), 210)), None);
        assert_eq!(alice_busy(&[]), None);
    }
    #[tokio::test]
    async fn framed_alice_events_duck_without_dispatching_transport_commands() {
        let (mut client, mut server) = tokio::io::duplex(2048);
        let (sender, mut commands) = mpsc::channel(8);
        let duck = Arc::new(crate::ducking::Ducking::new());
        let observed = duck.clone();
        let reader = tokio::spawn(async move {
            let result = read_events(
                &mut server,
                &sender,
                &observed,
                &crate::voice_recovery::Activation::default(),
                &mut VecDeque::new(),
            )
            .await;
            observed.set_busy(false);
            result
        });
        for (state, busy) in [(1, true), (3, true), (2, true), (0, false), (1, true)] {
            let msg = state_message(Some(state), 3);
            client.write_u32_le(msg.len() as u32).await.unwrap();
            for fragment in msg.chunks(3) {
                client.write_all(fragment).await.unwrap();
            }
            tokio::time::timeout(Duration::from_secs(1), async {
                while duck.active() != busy {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
        }
        assert!(commands.try_recv().is_err());
        drop(client);
        assert!(reader.await.unwrap().is_err());
        assert!(!duck.active());
    }
    #[test]
    fn recognizes_music_and_leaves_household_commands_alone() {
        assert_eq!(
            intent("Включи radiohead"),
            Some(Intent::Play("radiohead".into()))
        );
        assert_eq!(
            intent("Включи песню Всё идёт по плану"),
            Some(Intent::Play("все идет по плану".into()))
        );
        assert_eq!(intent("Останови музыку"), Some(Intent::Pause));
        assert_eq!(intent("Включи музыку"), Some(Intent::Resume));
        assert_eq!(intent("Продолжи музыку"), Some(Intent::Resume));
        assert_eq!(intent("следующий трек"), Some(Intent::Next));
        for s in [
            "включи свет",
            "включи свет в спальне",
            "поставь таймер на пять минут",
            "включи телевизор",
            "включи блютуз",
            "какая погода",
        ] {
            assert_eq!(intent(s), None, "{s}");
        }
        assert_eq!(context("never gonna"), "spotify:search:never+gonna");
        assert_eq!(context("a/b?c"), "spotify:search:a%2Fb%3Fc");
    }
    #[test]
    fn persisted_track_accepts_only_playable_spotify_items() {
        let track = "spotify:track:4uLU6hMCjMI75M1A2tKUQC";
        assert_eq!(playable_track(track).as_deref(), Some(track));
        assert_eq!(
            playable_track(&format!("{track}\n")).as_deref(),
            Some(track)
        );
        for value in [
            "",
            "spotify:search:radiohead",
            "spotify:album:4uLU6hMCjMI75M1A2tKUQC",
            "spotify:track:invalid",
            "/etc/passwd",
        ] {
            assert_eq!(playable_track(value), None);
        }
    }
    #[test]
    fn final_phrase_while_listening_or_busy_and_bounded_wire_parser() {
        let id = b"12345678-1234-1234-1234-123456789012";
        let text = "Включи radiohead".as_bytes();
        let mut asr = Vec::new();
        put_bytes(&mut asr, 1, text);
        put_number(&mut asr, 2, 1);
        put_number(&mut asr, 4, 1);
        let mut state = Vec::new();
        put_number(&mut state, 1, 3);
        put_bytes(&mut state, 2, text);
        put_bytes(&mut state, 7, id);
        put_bytes(&mut state, 15, &asr);
        let mut msg = Vec::new();
        put_bytes(&mut msg, 252, &state);
        put_number(&mut msg, 253, 3);
        assert_eq!(
            recognized(&msg),
            Some((
                String::from_utf8(id.to_vec()).unwrap(),
                "Включи radiohead".into()
            ))
        );
        for n in 0..msg.len() {
            assert_eq!(recognized(&msg[..n]), None);
        }
        for malformed in [
            vec![0],
            vec![0xff; 12],
            vec![0x12, 0xff, 0x7f],
            vec![0xe8, 0x0f, 210, 1],
        ] {
            assert_eq!(recognized(&malformed), None);
        }
        // The same final phrase while still LISTENING is taken a second earlier.
        let with_state = |alice: u8, asr: &[u8]| {
            let mut state = Vec::new();
            put_number(&mut state, 1, u64::from(alice));
            put_bytes(&mut state, 2, text);
            put_bytes(&mut state, 7, id);
            put_bytes(&mut state, 15, asr);
            let mut msg = Vec::new();
            put_bytes(&mut msg, 252, &state);
            put_number(&mut msg, 253, 3);
            msg
        };
        assert!(recognized(&with_state(1, &asr)).is_some());
        // Partial phrases and other states are ignored.
        let mut partial = Vec::new();
        put_bytes(&mut partial, 1, text);
        put_number(&mut partial, 2, 0);
        put_number(&mut partial, 4, 1);
        assert_eq!(recognized(&with_state(1, &partial)), None);
        assert_eq!(recognized(&with_state(2, &asr)), None);
        assert_eq!(recognized(&with_state(0, &asr)), None);
        assert_eq!(
            &rpc("AliceCapability", 8, &[8, 3])[4..],
            &[
                0xe2, 0x0f, 0x15, 0x3a, 0x0f, b'A', b'l', b'i', b'c', b'e', b'C', b'a', b'p', b'a',
                b'b', b'i', b'l', b'i', b't', b'y', 0x42, 2, 8, 3, 0xe8, 0x0f, 0xd2, 1
            ]
        );
    }
}
