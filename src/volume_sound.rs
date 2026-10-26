//! The firmware's short volume cue, mixed through the profile's existing ALSA device.
use librespot::playback::{
    audio_backend, config::AudioFormat, convert::Converter, decoder::AudioPacket,
};
use std::{
    fs::File,
    io::{self, Read},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};
use symphonia::core::{
    audio::SampleBuffer,
    codecs::DecoderOptions,
    formats::FormatOptions,
    io::{MediaSourceStream, MediaSourceStreamOptions},
    meta::MetadataOptions,
    probe::Hint,
};
const SOUND: &str = "/system/vendor/quasar/sounds/common/volume_sound.mp3";
#[derive(Default)]
pub struct Cue(AtomicBool);
impl Cue {
    pub fn notify(&self) {
        self.0.store(true, Ordering::Release);
    }
}
fn decode(path: &Path) -> io::Result<Vec<f64>> {
    let mut data = Vec::new();
    File::open(path)?.take(65537).read_to_end(&mut data)?;
    if data.len() > 65536 {
        return Err(io::Error::other("Volume cue exceeds 64 KiB"));
    }
    let input = MediaSourceStream::new(
        Box::new(std::io::Cursor::new(data)),
        MediaSourceStreamOptions::default(),
    );
    let mut hint = Hint::new();
    hint.with_extension("mp3");
    let mut probe = symphonia::default::get_probe()
        .format(
            &hint,
            input,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(io::Error::other)?;
    let track = probe
        .format
        .default_track()
        .ok_or_else(|| io::Error::other("No volume cue track"))?;
    let id = track.id;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(io::Error::other)?;
    let mut samples = Vec::new();
    loop {
        let packet = match probe.format.next_packet() {
            Ok(packet) => packet,
            Err(symphonia::core::errors::Error::IoError(e))
                if e.kind() == io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(e) => return Err(io::Error::other(e)),
        };
        if packet.track_id() != id {
            continue;
        }
        let audio = decoder.decode(&packet).map_err(io::Error::other)?;
        let channels = audio.spec().channels.count();
        if audio.spec().rate != 44100 || !matches!(channels, 1 | 2) {
            return Err(io::Error::other("Volume cue must be 44.1 kHz mono/stereo"));
        }
        let mut buffer = SampleBuffer::<f64>::new(audio.capacity() as u64, *audio.spec());
        buffer.copy_interleaved_ref(audio);
        for frame in buffer.samples().chunks_exact(channels) {
            samples.extend_from_slice(&[frame[0], frame[channels - 1]]);
        }
        if samples.len() > 44100 * 2 {
            return Err(io::Error::other("Volume cue exceeds one second"));
        }
    }
    if samples.is_empty() {
        return Err(io::Error::other("Empty volume cue"));
    }
    Ok(samples)
}
fn play(samples: &[f64], device: &str) -> io::Result<()> {
    let backend = audio_backend::find(Some("alsa".into()))
        .ok_or_else(|| io::Error::other("ALSA backend unavailable"))?;
    let mut sink = backend(Some(device.to_owned()), AudioFormat::S16);
    sink.start().map_err(io::Error::other)?;
    let result = sink
        .write(
            AudioPacket::Samples(samples.to_vec()),
            &mut Converter::new(None),
        )
        .map_err(io::Error::other);
    let stopped = sink.stop().map_err(io::Error::other);
    result.and(stopped)
}
pub fn start(shared: Arc<crate::Shared>, device: String) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let samples = match decode(Path::new(SOUND)) {
            Ok(samples) => samples,
            Err(e) => {
                eprintln!("volume cue: {e}");
                return;
            }
        };
        while shared.running.load(Ordering::Acquire) {
            if shared.volume_cue.0.swap(false, Ordering::AcqRel)
                && shared.controls_enabled.load(Ordering::Acquire)
                && !shared.ducking.active()
                && let Err(e) = play(&samples, &device)
            {
                eprintln!("volume cue: {e}");
            }
            thread::sleep(Duration::from_millis(20));
        }
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_and_oversized_cues_fail_closed() {
        let path = std::env::temp_dir().join(format!("spotiya-cue-{}", uuid::Uuid::new_v4()));
        std::fs::write(&path, b"not an mp3").unwrap();
        assert!(decode(&path).is_err());
        std::fs::write(&path, vec![0; 65537]).unwrap();
        assert!(decode(&path).is_err());
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn repeated_buttons_coalesce_into_one_pending_cue() {
        let cue = Cue::default();
        for _ in 0..100 {
            cue.notify();
        }
        assert!(cue.0.swap(false, Ordering::AcqRel));
        assert!(!cue.0.swap(false, Ordering::AcqRel));
    }
}
