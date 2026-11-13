//! The real bitrate of a stream, read from its own audio frames (MP3 frame
//! headers, AAC ADTS headers) instead of what the server claims in icy-br.

/// Bitrate measured over a piece of a stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bitrate {
    /// Average kbps over the frames found.
    pub kbps: u32,
    /// The frames had different bitrates (VBR), so `kbps` is an average.
    pub variable: bool,
}

impl Bitrate {
    /// "128", or "~173" for a variable bitrate.
    pub fn label(&self) -> String {
        if self.variable { format!("~{}", self.kbps) } else { self.kbps.to_string() }
    }
}

/// Fewer chained frames than this is treated as noise.
const MIN_FRAMES: usize = 4;

/// Measures the bitrate of MP3 or AAC (ADTS) audio. `data` may start in the
/// middle of a frame; frames only count when the next one starts exactly
/// where the previous one ends. None when no run of frames was found.
pub fn measure(data: &[u8]) -> Option<Bitrate> {
    match (mp3(data), adts(data)) {
        (Some(a), Some(b)) => Some(if a.1 >= b.1 { a.0 } else { b.0 }),
        (a, b) => a.or(b).map(|x| x.0),
    }
}

/// Finds the longest run of chained frames. `frame` returns (length, bits,
/// seconds, kbps) for a frame header at the start of the slice.
fn chain(data: &[u8], frame: impl Fn(&[u8]) -> Option<(usize, f64, f64, u32)>) -> Option<(Bitrate, usize)> {
    let mut best: Option<(Bitrate, usize)> = None;
    let mut i = 0;
    while i < data.len() {
        let (mut j, mut count, mut bits, mut secs) = (i, 0usize, 0.0, 0.0);
        let (mut first, mut variable) = (None, false);
        // A frame counts when another header follows it, or when it is the
        // last one of a chain and ends at (or runs past) the end of the data.
        while let Some((len, b, s, kbps)) = frame(&data[j..]) {
            let next = j + len;
            let followed = next < data.len() && frame(&data[next..]).is_some();
            if !followed && (count == 0 || next < data.len().saturating_sub(8)) {
                break;
            }
            variable |= *first.get_or_insert(kbps) != kbps;
            count += 1;
            bits += b;
            secs += s;
            if !followed {
                break;
            }
            j = next;
        }
        if count >= MIN_FRAMES && best.is_none_or(|(_, n)| count > n) {
            let kbps = (bits / secs / 1000.0).round() as u32;
            best = Some((Bitrate { kbps, variable }, count));
        }
        i = if count >= MIN_FRAMES { j + 1 } else { i + 1 };
    }
    best
}

/// MPEG audio Layer III (MP3), MPEG-1, -2 and -2.5.
fn mp3(data: &[u8]) -> Option<(Bitrate, usize)> {
    const V1: [u32; 15] = [0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320];
    const V2: [u32; 15] = [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160];
    chain(data, |h| {
        if h.len() < 4 || h[0] != 0xFF || h[1] & 0xE0 != 0xE0 {
            return None;
        }
        let version = (h[1] >> 3) & 3; // 0: 2.5, 2: 2, 3: 1
        let layer = (h[1] >> 1) & 3; // 1: Layer III
        let bi = (h[2] >> 4) as usize;
        let si = ((h[2] >> 2) & 3) as usize;
        if version == 1 || layer != 1 || bi == 0 || bi == 15 || si == 3 {
            return None;
        }
        let rate = [[11025, 12000, 8000], [0, 0, 0], [22050, 24000, 16000], [44100, 48000, 32000]][version as usize][si];
        let kbps = if version == 3 { V1[bi] } else { V2[bi] };
        let samples = if version == 3 { 1152 } else { 576 };
        let pad = ((h[2] >> 1) & 1) as usize;
        let len = (samples / 8 * kbps as usize * 1000) / rate as usize + pad;
        Some((len, len as f64 * 8.0, samples as f64 / rate as f64, kbps))
    })
}

/// AAC in ADTS frames. The header has no bitrate, but every frame holds 1024
/// samples, so its length and the sample rate give it (HE-AAC included: the
/// header carries the core rate).
fn adts(data: &[u8]) -> Option<(Bitrate, usize)> {
    const RATES: [u32; 12] = [96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000];
    chain(data, |h| {
        if h.len() < 7 || h[0] != 0xFF || h[1] & 0xF6 != 0xF0 {
            return None;
        }
        let si = ((h[2] >> 2) & 15) as usize;
        let len = (((h[3] & 3) as usize) << 11) | ((h[4] as usize) << 3) | ((h[5] >> 5) as usize);
        let header = if h[1] & 1 == 1 { 7 } else { 9 };
        if si >= RATES.len() || len <= header {
            return None;
        }
        let secs = 1024.0 / RATES[si] as f64;
        // Frame sizes vary with the content, so this "rate" is not used to
        // call AAC variable: it is always averaged.
        Some((len, len as f64 * 8.0, secs, 0))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// MPEG-1 Layer III frames at the given kbps, 44.1 kHz, no padding.
    fn mp3_frames(rates: &[u32]) -> Vec<u8> {
        let idx = |k: u32| [0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320].iter().position(|&r| r == k).unwrap() as u8;
        let mut out = Vec::new();
        for &k in rates {
            let len = 144 * k as usize * 1000 / 44100;
            let mut f = vec![0x55u8; len];
            f[..4].copy_from_slice(&[0xFF, 0xFB, idx(k) << 4, 0xC4]);
            out.extend(f);
        }
        out
    }

    /// AAC-LC ADTS frames of `len` bytes at 44.1 kHz.
    fn adts_frames(len: usize, n: usize) -> Vec<u8> {
        let mut out = Vec::new();
        for _ in 0..n {
            let mut f = vec![0x11u8; len];
            f[..7].copy_from_slice(&[
                0xFF,
                0xF1,
                0x50,
                0x80 | ((len >> 11) & 3) as u8,
                (len >> 3) as u8,
                (((len & 7) << 5) | 0x1F) as u8,
                0xFC,
            ]);
            out.extend(f);
        }
        out
    }

    #[test]
    fn mp3_cbr() {
        let mut data = vec![0xAB; 100]; // joined in the middle of a frame
        data.extend(mp3_frames(&[128; 30]));
        assert_eq!(measure(&data), Some(Bitrate { kbps: 128, variable: false }));
        assert_eq!(measure(&data).unwrap().label(), "128");
    }

    #[test]
    fn mp3_vbr() {
        let rates: Vec<u32> = [128, 160, 192, 224].iter().cycle().take(40).copied().collect();
        let b = measure(&mp3_frames(&rates)).unwrap();
        assert!(b.variable);
        assert!((160..=185).contains(&b.kbps), "{b:?}");
        assert!(b.label().starts_with('~'));
    }

    #[test]
    fn aac() {
        // 418 bytes per 1024 samples at 44.1 kHz = 144 kbps
        let b = measure(&adts_frames(418, 30)).unwrap();
        assert_eq!(b, Bitrate { kbps: 144, variable: false });
    }

    #[test]
    fn noise_is_not_audio() {
        let noise: Vec<u8> = (0..20000u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
        assert_eq!(measure(&noise), None);
        assert_eq!(measure(&[]), None);
        assert_eq!(measure(&mp3_frames(&[128; 2])), None);
    }
}
