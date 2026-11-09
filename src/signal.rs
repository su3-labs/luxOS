//! Preprocess calibrated spectra on the existing LuxOS reference grid.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

pub const WIDTH: usize = 951;
const FIRST: f64 = 200.0;
const STEP: f64 = 4.0;

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct Invalid(pub String);

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct PeakRequest {
    pub wavenumber: Vec<f64>,
    pub intensity: Vec<f64>,
    pub min_prominence: Option<f64>,
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct Peak {
    shift_cm_1: f64,
    relative_prominence: f64,
    width_cm_1: f64,
}

#[derive(Clone)]
pub struct Fingerprint {
    pub values: Vec<f32>,
    pub start: usize,
    pub end: usize,
}

fn checked(x: &[f64], y: &[f64]) -> Result<(Vec<f64>, Vec<f64>), Invalid> {
    if x.len() != y.len() || !(8..=65_536).contains(&x.len()) {
        return Err(Invalid(
            "Supply 8–65,536 paired Raman shifts and intensities.".into(),
        ));
    }
    if x.iter().chain(y).any(|v| !v.is_finite()) {
        return Err(Invalid(
            "Raman shifts and intensities must be finite.".into(),
        ));
    }
    if x.windows(2).all(|w| w[0] < w[1]) {
        return Ok((x.to_vec(), y.to_vec()));
    }
    if x.windows(2).all(|w| w[0] > w[1]) {
        return Ok((
            x.iter().rev().copied().collect(),
            y.iter().rev().copied().collect(),
        ));
    }
    Err(Invalid("Raman shifts must be strictly monotonic.".into()))
}

fn normalize(values: &mut [f64]) {
    let (min, max) = values
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), &v| {
            (lo.min(v), hi.max(v))
        });
    if (max - min).is_infinite() {
        let scale = max.abs().max(min.abs());
        for value in values.iter_mut() {
            *value /= scale;
        }
        normalize(values);
        return;
    }
    let range = (max - min).max(1e-300);
    for value in values {
        *value = (*value - min) / range;
    }
}

fn reflected(index: isize, count: usize) -> usize {
    let period = count as isize * 2;
    let index = index.rem_euclid(period);
    index.min(period - 1 - index) as usize
}

fn extreme(input: &[f64], width: usize, minimum: bool) -> Vec<f64> {
    let mut values = Vec::with_capacity(input.len());
    let mut window: VecDeque<(usize, f64)> = VecDeque::new();
    for i in 0..(input.len() + width - 1) {
        let value = input[reflected(i as isize - (width / 2) as isize, input.len())];
        while window
            .back()
            .is_some_and(|&(_, v)| if minimum { v >= value } else { v <= value })
        {
            window.pop_back();
        }
        window.push_back((i, value));
        if window.front().is_some_and(|&(j, _)| j + width <= i) {
            window.pop_front();
        }
        if i + 1 >= width {
            values.push(window[0].1);
        }
    }
    values
}

fn baseline(input: &[f64], width: usize) -> Vec<f64> {
    let opened = extreme(&extreme(input, width, true), width, false);
    input
        .iter()
        .zip(opened)
        .map(|(y, base)| (y - base).max(0.0))
        .collect()
}

pub fn fingerprint(x: &[f64], y: &[f64]) -> Result<Fingerprint, Invalid> {
    let (x, y) = checked(x, y)?;
    let start = (((x[0] - FIRST) / STEP).ceil() as usize).min(WIDTH);
    let end = ((((x[x.len() - 1] - FIRST) / STEP).floor() + 1.0) as usize).min(WIDTH);
    if end.saturating_sub(start) < 12 {
        return Err(Invalid("Too little spectrum within 200–4000 cm⁻¹.".into()));
    }
    let mut j = 0;
    let mut sampled: Vec<_> = (start..end)
        .map(|i| {
            let grid = FIRST + STEP * i as f64;
            while j + 2 < x.len() && x[j + 1] < grid {
                j += 1;
            }
            let distance = x[j + 1] - x[j];
            let t = if distance.is_finite() {
                (grid - x[j]) / distance
            } else {
                (grid / 2.0 - x[j] / 2.0) / (x[j + 1] / 2.0 - x[j] / 2.0)
            };
            y[j] * (1.0 - t) + y[j + 1] * t
        })
        .collect();
    normalize(&mut sampled);
    let corrected = baseline(&sampled, 31);
    let kernel: Vec<_> = (-6_i32..=6)
        .map(|i| (-f64::from(i * i) / 4.5).exp())
        .collect();
    let kernel_sum: f64 = kernel.iter().sum();
    let features: Vec<_> = (0..corrected.len())
        .map(|i| {
            let sum: f64 = kernel
                .iter()
                .enumerate()
                .map(|(j, &w)| {
                    w * corrected[reflected(i as isize + j as isize - 6, corrected.len())]
                })
                .sum();
            let grid = FIRST + STEP * (start + i) as f64;
            sum / kernel_sum
                * if (400.0..=1800.0).contains(&grid) {
                    1.0
                } else {
                    0.45
                }
        })
        .collect();
    let norm = features.iter().map(|v| v * v).sum::<f64>().sqrt();
    if !norm.is_finite() || norm <= 1e-8 {
        return Err(Invalid("The spectrum has no usable features.".into()));
    }
    let mut values = vec![0.0; WIDTH];
    for (out, v) in values[start..end].iter_mut().zip(features) {
        *out = (v / norm) as f32;
    }
    Ok(Fingerprint { values, start, end })
}

fn nearest_higher(
    values: &[f64],
    indices: impl Iterator<Item = usize>,
    default: usize,
) -> Vec<usize> {
    let mut stack: Vec<usize> = Vec::new();
    let mut result = vec![default; values.len()];
    for i in indices {
        while stack.last().is_some_and(|&j| values[j] <= values[i]) {
            stack.pop();
        }
        result[i] = stack.last().copied().unwrap_or(default);
        stack.push(i);
    }
    result
}

struct Minima {
    tree: Vec<f64>,
    offset: usize,
}
impl Minima {
    fn new(values: &[f64]) -> Self {
        let offset = values.len().next_power_of_two();
        let mut tree = vec![f64::INFINITY; offset * 2];
        tree[offset..offset + values.len()].copy_from_slice(values);
        for i in (1..offset).rev() {
            tree[i] = tree[i * 2].min(tree[i * 2 + 1]);
        }
        Self { tree, offset }
    }

    fn range(&self, start: usize, end: usize) -> f64 {
        let (mut a, mut b) = (start + self.offset, end + self.offset);
        let mut best = f64::INFINITY;
        while a < b {
            if a % 2 != 0 {
                best = best.min(self.tree[a]);
                a += 1;
            }
            if b % 2 != 0 {
                b -= 1;
                best = best.min(self.tree[b]);
            }
            a /= 2;
            b /= 2;
        }
        best
    }
}

pub fn peaks(request: PeakRequest) -> Result<Vec<Peak>, Invalid> {
    let (x, mut y) = checked(&request.wavenumber, &request.intensity)?;
    let prominence = request.min_prominence.unwrap_or(0.03);
    let limit = request.limit.unwrap_or(30);
    if !(prominence > 0.0 && prominence <= 1.0 && (1..=100).contains(&limit)) {
        return Err(Invalid(
            "Use prominence in (0, 1] and a limit of 1–100.".into(),
        ));
    }
    normalize(&mut y);
    let mut steps: Vec<_> = x.windows(2).map(|w| w[1] - w[0]).collect();
    steps.sort_unstable_by(f64::total_cmp);
    let median = steps[steps.len() / 2] / 2.0 + steps[(steps.len() - 1) / 2] / 2.0;
    let width = (((120.0 / median).round_ties_even().min(x.len() as f64) as usize / 2) * 2 + 1)
        .clamp(3, x.len() / 2 * 2 - 1);
    let y = baseline(&y, width);
    let maximum = y.iter().copied().fold(0.0, f64::max);
    if maximum <= 0.0 {
        return Ok(Vec::new());
    }
    let left = nearest_higher(&y, 0..y.len(), usize::MAX);
    let right = nearest_higher(&y, (0..y.len()).rev(), y.len());
    let minima = Minima::new(&y);
    let mut found = Vec::new();
    let mut i = 1;
    while i + 1 < y.len() {
        if y[i - 1] < y[i] {
            let mut end = i;
            while end + 1 < y.len() && y[end + 1] == y[i] {
                end += 1;
            }
            if end + 1 < y.len() && y[end + 1] < y[i] {
                let peak = (i + end) / 2;
                let start = left[peak].wrapping_add(1);
                let stop = right[peak];
                let contour = minima.range(start, peak + 1).max(minima.range(peak, stop));
                let height = y[peak] - contour;
                if height >= maximum * prominence {
                    found.push((height, peak));
                }
            }
            i = end;
        }
        i += 1;
    }
    found.sort_unstable_by(|a, b| b.0.total_cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    Ok(found
        .into_iter()
        .take(limit)
        .map(|(height, peak)| {
            let half = y[peak] - height * 0.5;
            let mut a = peak;
            let mut b = peak;
            while a > 0 && y[a] > half {
                a -= 1;
            }
            while b + 1 < y.len() && y[b] > half {
                b += 1;
            }
            let xa = if y[a] < half {
                x[a] + (x[a + 1] - x[a]) * (half - y[a]) / (y[a + 1] - y[a])
            } else {
                x[a]
            };
            let xb = if y[b] < half {
                x[b] - (x[b] - x[b - 1]) * (half - y[b]) / (y[b - 1] - y[b])
            } else {
                x[b]
            };
            Peak {
                shift_cm_1: rounded(x[peak], 1000.0),
                relative_prominence: rounded(height / maximum, 10_000.0),
                width_cm_1: rounded(xb - xa, 1000.0),
            }
        })
        .collect())
}

pub fn rounded(value: f64, scale: f64) -> f64 {
    (value * scale).round_ties_even() / scale
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fingerprint_and_peak_shapes() {
        let shifts: Vec<_> = (200..=4000).map(f64::from).collect();
        let intensity: Vec<_> = shifts
            .iter()
            .map(|&v| {
                0.05 + v * 0.0001
                    + (-0.5 * ((v - 918.0) / 7.0).powi(2)).exp()
                    + 0.8 * (-0.5 * ((v - 1374.0) / 6.0).powi(2)).exp()
            })
            .collect();
        let q = fingerprint(&shifts, &intensity).unwrap();
        assert_eq!((q.start, q.end), (0, WIDTH));
        assert!((q.values.iter().map(|v| v * v).sum::<f32>() - 1.0).abs() < 1e-6);
        assert_eq!(
            fingerprint(
                &shifts.iter().rev().copied().collect::<Vec<_>>(),
                &intensity.iter().rev().copied().collect::<Vec<_>>()
            )
            .unwrap()
            .values,
            q.values
        );
        let peaks = peaks(PeakRequest {
            wavenumber: shifts,
            intensity,
            min_prominence: None,
            limit: None,
        })
        .unwrap();
        assert_eq!(peaks.len(), 2);
        assert_eq!(peaks[0].shift_cm_1, 918.0);
        assert_eq!(peaks[1].shift_cm_1, 1374.0);
        assert!((peaks[0].width_cm_1 - 16.5).abs() < 0.5);
    }
}
