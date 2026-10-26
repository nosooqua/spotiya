use std::io;

use aes::cipher::{KeyIvInit, StreamCipher, StreamCipherSeek};

type Aes128Ctr = ctr::Ctr128BE<aes::Aes128>;

use librespot_core::audio_key::{AudioKey, AudioKeyMaterial};

const AUDIO_AESIV: [u8; 16] = [
    0x72, 0xe0, 0x67, 0xfb, 0xdd, 0xcb, 0xcf, 0x77, 0xeb, 0xe8, 0xbc, 0x64, 0x3f, 0x63, 0x0d, 0x93,
];

pub struct AudioDecrypt<T: io::Read> {
    // a `None` cipher is a convenience to make `AudioDecrypt` pass files unaltered
    cipher: Option<Aes128Ctr>,
    reader: T,
}

impl<T: io::Read> AudioDecrypt<T> {
    pub fn new_material(key: Option<AudioKeyMaterial>, reader: T) -> io::Result<Self> {
        Ok(Self::new(key.map(|AudioKeyMaterial::Aes(key)| key), reader))
    }

    pub fn new(key: Option<AudioKey>, reader: T) -> AudioDecrypt<T> {
        let cipher = if let Some(key) = key {
            Aes128Ctr::new_from_slices(&key.0, &AUDIO_AESIV).ok()
        } else {
            // some files are unencrypted
            None
        };

        AudioDecrypt { cipher, reader }
    }
}

impl<T: io::Read> io::Read for AudioDecrypt<T> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let len = self.reader.read(output)?;

        if let Some(ref mut cipher) = self.cipher {
            cipher.apply_keystream(&mut output[..len]);
        }

        Ok(len)
    }
}

impl<T: io::Read + io::Seek> io::Seek for AudioDecrypt<T> {
    fn seek(&mut self, pos: io::SeekFrom) -> io::Result<u64> {
        let newpos = self.reader.seek(pos)?;

        if let Some(ref mut cipher) = self.cipher {
            cipher.seek(newpos);
        }

        Ok(newpos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Read, Seek, SeekFrom};

    #[test]
    fn aes_material_decrypts_chunked_reads_and_unaligned_seeks() {
        let key = AudioKey([42; 16]);
        let plain: Vec<u8> = (0..131_101).map(|i| (i % 251) as u8).collect();
        let mut encrypted = plain.clone();
        Aes128Ctr::new_from_slices(&key.0, &AUDIO_AESIV)
            .unwrap()
            .apply_keystream(&mut encrypted);
        let mut reader =
            AudioDecrypt::new_material(Some(AudioKeyMaterial::Aes(key)), Cursor::new(encrypted))
                .unwrap();
        let mut output = vec![0; plain.len()];
        for chunk in output.chunks_mut(997) {
            reader.read_exact(chunk).unwrap();
        }
        assert_eq!(output, plain);
        reader.seek(SeekFrom::Start(65_533)).unwrap();
        let mut sample = [0; 71];
        reader.read_exact(&mut sample).unwrap();
        assert_eq!(sample, plain[65_533..65_604]);
        let mut unencrypted = AudioDecrypt::new_material(None, Cursor::new(&plain)).unwrap();
        let mut output = Vec::new();
        unencrypted.read_to_end(&mut output).unwrap();
        assert_eq!(output, plain);
    }
}
