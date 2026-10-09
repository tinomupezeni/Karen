pub fn downmix_to_mono(samples: &[f32], channels: u16) -> Vec<f32> {
    if channels <= 1 {
        return samples.to_vec();
    }
    let ch = channels as usize;
    samples
        .chunks_exact(ch)
        .map(|frame| frame.iter().sum::<f32>() / ch as f32)
        .collect()
}

pub fn resample_linear(input: &[f32], from_hz: u32, to_hz: u32) -> Vec<f32> {
    if from_hz == to_hz || input.is_empty() {
        return input.to_vec();
    }
    let ratio = from_hz as f64 / to_hz as f64;
    let out_len = ((input.len() as f64) / ratio).round() as usize;
    (0..out_len)
        .map(|i| {
            let src = i as f64 * ratio;
            let idx = src.floor() as usize;
            let frac = (src - idx as f64) as f32;
            let s0 = input.get(idx).copied().unwrap_or(0.0);
            let s1 = input.get(idx + 1).copied().unwrap_or(s0);
            s0 + (s1 - s0) * frac
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    #[test]
    fn downmix_averages_channels() {
        let stereo = [1.0, 3.0, 2.0, 4.0];
        assert_eq!(downmix_to_mono(&stereo, 2), vec![2.0, 3.0]);
    }

    #[test]
    fn resample_identity_when_same_rate() {
        let input = vec![0.1, 0.2, 0.3];
        assert_eq!(resample_linear(&input, 16_000, 16_000), input);
    }

    #[test]
    fn resample_downsamples_to_expected_length() {
        let sample_rate = 48_000;
        let freq = 440.0;
        let input: Vec<f32> = (0..sample_rate)
            .map(|i| (2.0 * PI * freq * (i as f32) / sample_rate as f32).sin())
            .collect();

        let out = resample_linear(&input, sample_rate, 16_000);
        let expected_len = (input.len() as f64 * 16_000.0 / sample_rate as f64).round() as usize;
        assert_eq!(out.len(), expected_len);
        assert!(out.iter().all(|s| (-1.0..=1.0).contains(s)));
    }
}
