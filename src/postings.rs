//! Compact per-collection peak postings for candidate generation.
use crate::signal::{Fingerprint, WIDTH};
use anyhow::{Context, Result, ensure};
use memmap2::{Mmap, MmapOptions};
use std::{cmp::Reverse, collections::BinaryHeap, fs::File, path::Path};
use tempfile::NamedTempFile;

pub const FILE: &str = "peaks.lxo";
pub const MAX_ROWS: usize = 10_000_000;
const MAGIC: &[u8; 8] = b"LXPEAK02";
const BIN_WIDTH: usize = 1;
const BINS: usize = WIDTH.div_ceil(BIN_WIDTH);
const REFERENCE_PEAKS: usize = 24;
const QUERY_PEAKS: usize = 32;
const HEADER: usize = 16 + 8 * (BINS + 1);
const ROW_MASK: u32 = (1 << 24) - 1;

pub fn peak_bins(values: &[f32], limit: usize) -> Vec<(usize, u8)> {
    let max = values.iter().copied().fold(0.0, f32::max);
    if max <= 0.0 {
        return Vec::new();
    }
    let mut peaks: Vec<_> = values
        .iter()
        .copied()
        .enumerate()
        .filter(|&(i, v)| {
            v >= max * 0.025
                && (i == 0 || v > values[i - 1])
                && (i + 1 == values.len() || v >= values[i + 1])
        })
        .collect();
    peaks.sort_unstable_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    // Stored vectors already carry the full-spectrum L2 normalization.
    peaks
        .into_iter()
        .take(limit)
        .filter_map(|(bin, v)| {
            let weight = (v * 255.0).round() as u8;
            (weight > 0).then_some((bin, weight))
        })
        .collect()
}

fn u64_at(bytes: &[u8], start: usize) -> u64 {
    u64::from_le_bytes(bytes[start..start + 8].try_into().unwrap())
}

pub struct Postings {
    map: Mmap,
    offsets: [usize; BINS + 1],
    rows: usize,
}

impl Postings {
    pub fn open(path: &Path, rows: usize) -> Result<Self> {
        let file = File::open(path)?;
        // Collection files are immutable and replaced atomically by the publisher.
        let map = unsafe { MmapOptions::new().map(&file) }?;
        ensure!(
            map.len() >= HEADER && &map[..8] == MAGIC,
            "invalid peak index header"
        );
        let count = u32::from_le_bytes(map[8..12].try_into().unwrap()) as usize;
        ensure!(
            count == rows && rows <= MAX_ROWS,
            "peak index row count mismatch"
        );
        let layout = u32::from_le_bytes(map[12..16].try_into().unwrap());
        ensure!(
            layout == ((BIN_WIDTH as u32) << 16) | REFERENCE_PEAKS as u32,
            "unsupported peak index layout"
        );
        ensure!(
            (map.len() - HEADER).is_multiple_of(4),
            "truncated peak index"
        );
        let postings = (map.len() - HEADER) / 4;
        let mut offsets = [0; BINS + 1];
        for (i, offset) in offsets.iter_mut().enumerate() {
            *offset = usize::try_from(u64_at(&map, 16 + i * 8))?;
        }
        ensure!(
            offsets[0] == 0
                && offsets[BINS] == postings
                && offsets.windows(2).all(|v| v[0] <= v[1])
                && postings <= rows * REFERENCE_PEAKS,
            "invalid peak index offsets"
        );
        Ok(Self { map, offsets, rows })
    }

    pub fn shortlist(
        &self,
        query: &Fingerprint,
        limit: usize,
        covered: impl Fn(usize) -> bool,
    ) -> Result<Vec<usize>> {
        let peaks = peak_bins(&query.values, QUERY_PEAKS);
        if peaks.is_empty() {
            return Ok(Vec::new());
        }
        let mut scores = vec![0_u32; self.rows];
        // Each entry is one 24-bit reference ID and one 8-bit peak strength.
        for (bin, strength) in peaks {
            // Approximate the overlap of the 6 cm⁻¹-smoothed peaks.
            const KERNEL: [u32; 4] = [16, 14, 10, 6];
            for neighbor in bin.saturating_sub(3)..=(bin + 3).min(BINS - 1) {
                let begin = HEADER + 4 * self.offsets[neighbor];
                let end = HEADER + 4 * self.offsets[neighbor + 1];
                let weight = u32::from(strength) * KERNEL[bin.abs_diff(neighbor)];
                for &bytes in self.map[begin..end].as_chunks::<4>().0 {
                    let packed = u32::from_le_bytes(bytes);
                    let row = (packed & ROW_MASK) as usize;
                    let entry = scores.get_mut(row).context("invalid row in peak index")?;
                    *entry += weight * (packed >> 24);
                }
            }
        }
        let mut heap = BinaryHeap::with_capacity(limit);
        for (row, score) in scores.into_iter().enumerate() {
            if score == 0 || !covered(row) {
                continue;
            }
            let candidate = (score, row);
            if heap.len() < limit {
                heap.push(Reverse(candidate));
            } else if candidate > heap.peek().unwrap().0 {
                *heap.peek_mut().unwrap() = Reverse(candidate);
            }
        }
        let mut rows: Vec<_> = heap.into_iter().map(|v| v.0.1).collect();
        rows.sort_unstable(); // Keep exact reranking reads in disk order.
        Ok(rows)
    }
}

pub fn build(path: &Path, rows: usize, row: impl Fn(usize, &mut [f32])) -> Result<u64> {
    ensure!(
        rows > 0 && rows <= MAX_ROWS,
        "collection must contain 1–{MAX_ROWS} spectra"
    );
    let mut counts = [0_u64; BINS];
    let mut values = [0.0; WIDTH];
    for id in 0..rows {
        row(id, &mut values);
        for (bin, _) in peak_bins(&values, REFERENCE_PEAKS) {
            counts[bin] += 1;
        }
    }
    let mut offsets = [0_u64; BINS + 1];
    for i in 0..BINS {
        offsets[i + 1] = offsets[i] + counts[i];
    }
    let entries = offsets[BINS];
    let temporary = NamedTempFile::new_in(path.parent().context("peak index parent")?)?;
    temporary.as_file().set_len(HEADER as u64 + entries * 4)?;
    {
        // This temporary file is held exclusively by this writer until it is published.
        let mut map = unsafe { MmapOptions::new().map_mut(temporary.as_file()) }?;
        map[..8].copy_from_slice(MAGIC);
        map[8..12].copy_from_slice(&(rows as u32).to_le_bytes());
        map[12..16]
            .copy_from_slice(&(((BIN_WIDTH as u32) << 16) | REFERENCE_PEAKS as u32).to_le_bytes());
        for (i, offset) in offsets.iter().enumerate() {
            map[16 + 8 * i..24 + 8 * i].copy_from_slice(&offset.to_le_bytes());
        }
        for id in 0..rows {
            row(id, &mut values);
            for (bin, weight) in peak_bins(&values, REFERENCE_PEAKS) {
                let start = HEADER + 4 * offsets[bin] as usize;
                let value = id as u32 | (u32::from(weight) << 24);
                map[start..start + 4].copy_from_slice(&value.to_le_bytes());
                offsets[bin] += 1;
            }
        }
        map.flush()?;
    }
    temporary.as_file().sync_all()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o644))?;
    }
    temporary.persist(path)?;
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidecar_ranks_overlapping_peaks() -> Result<()> {
        let folder = tempfile::tempdir()?;
        let path = folder.path().join(FILE);
        build(&path, 5, |row, values| {
            values.fill(0.0);
            values[200 + 10 * row] = 0.7;
            values[500 + 10 * row] = 0.7;
        })?;
        let index = Postings::open(&path, 5)?;
        let mut values = vec![0.0; WIDTH];
        values[219] = 0.7;
        values[520] = 0.7;
        let query = Fingerprint {
            values,
            start: 0,
            end: WIDTH,
        };
        assert_eq!(index.shortlist(&query, 2, |_| true)?, [2]);
        assert!(Postings::open(&path, 4).is_err());
        Ok(())
    }

    #[test]
    #[ignore = "writes a 480 MB synthetic postings fixture"]
    fn ten_million_row_capacity() -> Result<()> {
        use std::io::{BufWriter, Write};
        use std::time::Instant;

        let mut file = NamedTempFile::new()?;
        let mut values = vec![0.0; WIDTH];
        for i in 0..20 {
            values[115 + 31 * i] = 0.2;
        }
        let query = Fingerprint {
            values,
            start: 0,
            end: WIDTH,
        };
        let anchors: Vec<_> = peak_bins(&query.values, QUERY_PEAKS)
            .into_iter()
            .map(|v| v.0)
            .collect();
        let per_bin = MAX_ROWS * 12 / BINS;
        let mut total = 0_u64;
        {
            let mut writer = BufWriter::with_capacity(1024 * 1024, file.as_file_mut());
            writer.write_all(MAGIC)?;
            writer.write_all(&(MAX_ROWS as u32).to_le_bytes())?;
            writer
                .write_all(&(((BIN_WIDTH as u32) << 16) | REFERENCE_PEAKS as u32).to_le_bytes())?;
            writer.write_all(&total.to_le_bytes())?;
            for bin in 0..BINS {
                total += (per_bin + usize::from(anchors.contains(&bin))) as u64;
                writer.write_all(&total.to_le_bytes())?;
            }
            for bin in 0..BINS {
                for i in 0..per_bin {
                    let id = ((i * 7919 + bin * 104729) % (MAX_ROWS - 1)) as u32;
                    let weight = 8 + (i % 128) as u32;
                    writer.write_all(&(id | (weight << 24)).to_le_bytes())?;
                }
                if anchors.contains(&bin) {
                    writer.write_all(&((MAX_ROWS as u32 - 1) | (255 << 24)).to_le_bytes())?;
                }
            }
            writer.flush()?;
        }
        let index = Postings::open(file.path(), MAX_ROWS)?;
        let now = Instant::now();
        let hits = index.shortlist(&query, 4096, |_| true)?;
        println!(
            "{MAX_ROWS} synthetic rows, {total} postings, shortlist in {:?}",
            now.elapsed()
        );
        assert_eq!(hits.len(), 4096);
        assert!(hits.contains(&(MAX_ROWS - 1)));
        Ok(())
    }
}
