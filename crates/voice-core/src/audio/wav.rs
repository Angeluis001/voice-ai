//! Minimal 16-bit PCM WAV writing.

/// Wrap raw little-endian 16-bit PCM in a canonical 44-byte WAV header.
pub fn wrap_pcm16le(pcm: &[u8], sample_rate: u32, channels: u16) -> Vec<u8> {
    let data_len = pcm.len() as u32;
    let block_align = channels * 2;
    let byte_rate = sample_rate * u32::from(block_align);
    let mut out = Vec::with_capacity(44 + pcm.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    out.extend_from_slice(pcm);
    out
}

/// Samples to little-endian bytes.
pub fn samples_to_bytes(samples: &[i16]) -> Vec<u8> {
    samples.iter().flat_map(|s| s.to_le_bytes()).collect()
}

/// Little-endian bytes to samples. A trailing odd byte is dropped.
pub fn bytes_to_samples(bytes: &[u8]) -> Vec<i16> {
    bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect()
}

/// Read a 16-bit PCM WAV file: `(sample_rate, channels, interleaved samples)`.
/// Walks the chunks to find `fmt ` and `data`; tolerates streamed WAVs whose
/// declared data size is larger than the file.
pub fn read_pcm16(bytes: &[u8]) -> Result<(u32, u16, Vec<i16>), String> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("not a WAV file".into());
    }
    let u16_at = |i: usize| u16::from_le_bytes([bytes[i], bytes[i + 1]]);
    let u32_at =
        |i: usize| u32::from_le_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
    let (mut pos, mut format) = (12usize, None);
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32_at(pos + 4) as usize;
        let body = pos + 8;
        let end = body.saturating_add(size).min(bytes.len());
        if id == b"fmt " && end >= body + 16 {
            format = Some((
                u16_at(body),
                u16_at(body + 2),
                u32_at(body + 4),
                u16_at(body + 14),
            ));
        } else if id == b"data" {
            let (kind, channels, rate, bits) = format.ok_or("WAV has no format chunk")?;
            if kind != 1 || bits != 16 {
                return Err(format!(
                    "only 16-bit PCM WAV is supported (format {kind}, {bits} bits)"
                ));
            }
            return Ok((rate, channels, bytes_to_samples(&bytes[body..end])));
        }
        pos = body.saturating_add(size).saturating_add(size & 1);
    }
    Err("WAV has no audio data".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_is_canonical() {
        let wav = wrap_pcm16le(&[1, 0, 2, 0], 8000, 1);
        assert_eq!(wav.len(), 48);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(wav[4..8].try_into().unwrap()), 40);
        assert_eq!(&wav[8..16], b"WAVEfmt ");
        assert_eq!(u16::from_le_bytes(wav[20..22].try_into().unwrap()), 1);
        assert_eq!(u16::from_le_bytes(wav[22..24].try_into().unwrap()), 1);
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 8000);
        assert_eq!(u32::from_le_bytes(wav[28..32].try_into().unwrap()), 16000);
        assert_eq!(u16::from_le_bytes(wav[32..34].try_into().unwrap()), 2);
        assert_eq!(u16::from_le_bytes(wav[34..36].try_into().unwrap()), 16);
        assert_eq!(&wav[36..40], b"data");
        assert_eq!(u32::from_le_bytes(wav[40..44].try_into().unwrap()), 4);
    }

    #[test]
    fn sample_round_trip_drops_odd_byte() {
        assert_eq!(bytes_to_samples(&[1, 0, 255, 255, 9]), vec![1, -1]);
        assert_eq!(samples_to_bytes(&[1, -1]), vec![1, 0, 255, 255]);
    }

    #[test]
    fn reads_back_what_it_wrote() {
        let wav = wrap_pcm16le(&samples_to_bytes(&[5, -6, 7]), 24000, 1);
        assert_eq!(read_pcm16(&wav).unwrap(), (24000, 1, vec![5, -6, 7]));
        assert!(read_pcm16(b"definitely not audio").is_err());
    }
}
