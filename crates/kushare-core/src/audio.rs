//! Audio compartido por el PC y el teléfono: IMA-ADPCM (el micrófono del
//! teléfono por Bluetooth), remuestreo sencillo y tonos DTMF. Rust puro y
//! sin dependencias: el mismo código codifica en Android y descodifica en
//! el PC.

const INDEX_TABLE: [i8; 16] = [-1, -1, -1, -1, 2, 4, 6, 8, -1, -1, -1, -1, 2, 4, 6, 8];

const STEP_TABLE: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66,
    73, 80, 88, 97, 107, 118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449,
    494, 544, 598, 658, 724, 796, 876, 963, 1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066, 2272,
    2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845, 8630, 9493,
    10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794, 32767,
];

/// Estado del predictor IMA-ADPCM.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AdpcmState {
    pub predictor: i16,
    pub index: u8,
}

impl AdpcmState {
    fn decode_nibble(&mut self, nibble: u8) -> i16 {
        let step = STEP_TABLE[self.index as usize];
        let mut diff = step >> 3;
        if nibble & 4 != 0 {
            diff += step;
        }
        if nibble & 2 != 0 {
            diff += step >> 1;
        }
        if nibble & 1 != 0 {
            diff += step >> 2;
        }
        let mut pred = self.predictor as i32;
        if nibble & 8 != 0 {
            pred -= diff;
        } else {
            pred += diff;
        }
        self.predictor = pred.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
        self.index = (self.index as i32 + INDEX_TABLE[nibble as usize] as i32).clamp(0, 88) as u8;
        self.predictor
    }

    fn encode_sample(&mut self, sample: i16) -> u8 {
        let step = STEP_TABLE[self.index as usize];
        let mut diff = sample as i32 - self.predictor as i32;
        let mut nibble = 0u8;
        if diff < 0 {
            nibble = 8;
            diff = -diff;
        }
        let mut s = step;
        if diff >= s {
            nibble |= 4;
            diff -= s;
        }
        s >>= 1;
        if diff >= s {
            nibble |= 2;
            diff -= s;
        }
        s >>= 1;
        if diff >= s {
            nibble |= 1;
        }
        // Avanzar el predictor exactamente como lo hará el descodificador.
        self.decode_nibble(nibble);
        nibble
    }
}

/// Codifica un bloque: cabecera (predictor i16 LE + índice u8) y dos
/// muestras por byte (la primera en el nibble bajo). El estado sigue entre
/// bloques, pero cada bloque se puede descodificar solo.
pub fn adpcm_encode(state: &mut AdpcmState, samples: &[i16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(3 + samples.len().div_ceil(2));
    out.extend_from_slice(&state.predictor.to_le_bytes());
    out.push(state.index);
    for pair in samples.chunks(2) {
        let lo = state.encode_sample(pair[0]);
        let hi = pair.get(1).map(|&s| state.encode_sample(s)).unwrap_or(0);
        out.push(lo | (hi << 4));
    }
    out
}

/// Descodifica un bloque de [`adpcm_encode`]. Un bloque con número impar de
/// muestras da una de más (silencio aproximado): no se nota a 16 kHz.
pub fn adpcm_decode(block: &[u8]) -> Vec<i16> {
    if block.len() < 3 {
        return Vec::new();
    }
    let mut state = AdpcmState {
        predictor: i16::from_le_bytes([block[0], block[1]]),
        index: block[2].min(88),
    };
    let mut out = Vec::with_capacity((block.len() - 3) * 2);
    for &b in &block[3..] {
        out.push(state.decode_nibble(b & 0x0f));
        out.push(state.decode_nibble(b >> 4));
    }
    out
}

/// Remuestrea `samples` multiplicando la frecuencia por `factor`
/// (interpolación lineal). `last` es la última muestra del bloque anterior,
/// para que no haya saltos entre bloques; se actualiza.
pub fn upsample(samples: &[i16], factor: usize, last: &mut i16) -> Vec<i16> {
    let mut out = Vec::with_capacity(samples.len() * factor);
    let mut prev = *last as i32;
    for &s in samples {
        let s = s as i32;
        for k in 1..=factor {
            out.push((prev + (s - prev) * k as i32 / factor as i32) as i16);
        }
        prev = s;
    }
    if let Some(&s) = samples.last() {
        *last = s;
    }
    out
}

/// Frecuencias (baja, alta) de una tecla DTMF.
pub fn dtmf_freqs(key: char) -> Option<(f32, f32)> {
    const LOW: [f32; 4] = [697.0, 770.0, 852.0, 941.0];
    const HIGH: [f32; 4] = [1209.0, 1336.0, 1477.0, 1633.0];
    const KEYS: [[char; 4]; 4] = [
        ['1', '2', '3', 'A'],
        ['4', '5', '6', 'B'],
        ['7', '8', '9', 'C'],
        ['*', '0', '#', 'D'],
    ];
    let key = key.to_ascii_uppercase();
    for (r, row) in KEYS.iter().enumerate() {
        if let Some(c) = row.iter().position(|&k| k == key) {
            return Some((LOW[r], HIGH[c]));
        }
    }
    None
}

/// ¿Son todos teclas DTMF válidas ("0-9*#ABCD")?
pub fn is_dtmf(digits: &str) -> bool {
    !digits.is_empty() && digits.chars().all(|c| dtmf_freqs(c).is_some())
}

/// PCM mono de 16 bits con los tonos de `digits`: `tone_ms` de tono y
/// `gap_ms` de silencio después de cada tecla. Ignora caracteres no DTMF.
pub fn dtmf_pcm(digits: &str, rate: u32, tone_ms: u32, gap_ms: u32) -> Vec<i16> {
    let tone = (rate * tone_ms / 1000) as usize;
    let gap = (rate * gap_ms / 1000) as usize;
    let mut out = Vec::new();
    for (lo, hi) in digits.chars().filter_map(dtmf_freqs) {
        for n in 0..tone {
            let t = n as f32 / rate as f32;
            let v = (2.0 * std::f32::consts::PI * lo * t).sin()
                + (2.0 * std::f32::consts::PI * hi * t).sin();
            out.push((v * 0.45 * i16::MAX as f32) as i16);
        }
        out.extend(std::iter::repeat_n(0, gap));
    }
    out
}

/// Nivel de pico (0.0–1.0) de un bloque PCM.
pub fn peak(samples: &[i16]) -> f32 {
    samples
        .iter()
        .map(|&s| (s as i32).unsigned_abs())
        .max()
        .unwrap_or(0) as f32
        / 32768.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f32, rate: u32, n: usize) -> Vec<i16> {
        (0..n)
            .map(|i| {
                ((2.0 * std::f32::consts::PI * freq * i as f32 / rate as f32).sin() * 12000.0)
                    as i16
            })
            .collect()
    }

    #[test]
    fn adpcm_ida_y_vuelta() {
        let input = sine(440.0, 16000, 640);
        let mut st = AdpcmState::default();
        // Dos bloques seguidos: el segundo se descodifica solo.
        let a = adpcm_encode(&mut st, &input[..320]);
        let b = adpcm_encode(&mut st, &input[320..]);
        assert_eq!(a.len(), 3 + 160);
        let mut out = adpcm_decode(&a);
        out.extend(adpcm_decode(&b));
        assert_eq!(out.len(), input.len());
        // Tras la entrada en régimen, el error es pequeño frente a la señal.
        let err: f64 = input[40..]
            .iter()
            .zip(&out[40..])
            .map(|(&x, &y)| ((x as f64) - (y as f64)).powi(2))
            .sum::<f64>()
            / (input.len() - 40) as f64;
        assert!(err.sqrt() < 600.0, "error RMS {}", err.sqrt());
    }

    #[test]
    fn adpcm_bloque_corto() {
        assert!(adpcm_decode(&[1, 2]).is_empty());
        let mut st = AdpcmState::default();
        let block = adpcm_encode(&mut st, &[100, 200, 300]);
        assert_eq!(adpcm_decode(&block).len(), 4);
    }

    #[test]
    fn remuestreo_x3_sin_saltos() {
        let mut last = 0;
        let out = upsample(&[300, 600], 3, &mut last);
        assert_eq!(out, vec![100, 200, 300, 400, 500, 600]);
        assert_eq!(last, 600);
        let out = upsample(&[600], 3, &mut last);
        assert_eq!(out, vec![600, 600, 600]);
    }

    /// Potencia de `freq` en `x` (algoritmo de Goertzel).
    fn goertzel(x: &[i16], freq: f32, rate: u32) -> f32 {
        let w = 2.0 * std::f32::consts::PI * freq / rate as f32;
        let c = 2.0 * w.cos();
        let (mut s1, mut s2) = (0.0f32, 0.0f32);
        for &v in x {
            let s = v as f32 / 32768.0 + c * s1 - s2;
            s2 = s1;
            s1 = s;
        }
        s1 * s1 + s2 * s2 - c * s1 * s2
    }

    #[test]
    fn dtmf_frecuencias_correctas() {
        assert!(is_dtmf("0123456789*#abcd"));
        assert!(!is_dtmf("12x"));
        assert!(!is_dtmf(""));
        let rate = 8000;
        let pcm = dtmf_pcm("5", rate, 100, 0);
        assert_eq!(pcm.len(), 800);
        let on = goertzel(&pcm, 770.0, rate).min(goertzel(&pcm, 1336.0, rate));
        let off = [697.0, 852.0, 941.0, 1209.0, 1477.0, 1633.0]
            .iter()
            .map(|&f| goertzel(&pcm, f, rate))
            .fold(0.0, f32::max);
        assert!(on > off * 50.0, "on {on} off {off}");
    }

    #[test]
    fn pico() {
        assert_eq!(peak(&[]), 0.0);
        assert!((peak(&[0, -16384, 100]) - 0.5).abs() < 1e-3);
    }
}
