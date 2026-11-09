//! Read the v1 LuxOS spectra and rerank candidates with coverage-weighted cosine.
use crate::{
    postings,
    signal::{self, Fingerprint, Invalid, WIDTH},
};
use anyhow::{Context, Result, ensure};
use half::f16;
use memmap2::{Mmap, MmapOptions};
use rusqlite::{Connection, OpenFlags};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    cmp::{Ordering, Reverse},
    collections::{BTreeMap, BinaryHeap, HashMap},
    fs::{self, File},
    path::{Path, PathBuf},
    sync::OnceLock,
    time::Instant,
};

const MIN_INDEXED_ROWS: usize = 10_000;
const SHORTLIST: usize = 4096;
const ROW_BYTES: usize = WIDTH * 2;

#[derive(Debug, Clone, Copy, Deserialize, Serialize, JsonSchema, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    Auto,
    Exact,
    Indexed,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Measured,
    Computed,
    Both,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Self::Measured => "measured",
            Self::Computed => "computed",
            Self::Both => "both",
        }
    }
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct SearchRequest {
    pub wavenumber: Vec<f64>,
    pub intensity: Vec<f64>,
    pub kind: Option<Kind>,
    pub sources: Option<Vec<String>>,
    pub limit: Option<usize>,
    pub mode: Option<Mode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub rows: usize,
    pub kind: Kind,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl Manifest {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.version == 1
                && (1..=postings::MAX_ROWS).contains(&self.rows)
                && self.kind != Kind::Both,
            "unsupported reference manifest"
        );
        Ok(())
    }
}

#[derive(Debug, Serialize)]
pub struct Candidate {
    source: String,
    reference: String,
    name: Option<String>,
    smiles: Option<String>,
    provenance: Value,
    score: f64,
}

#[derive(Debug, Serialize)]
pub struct Results {
    matches: BTreeMap<&'static str, Vec<Candidate>>,
    references_searched: BTreeMap<&'static str, usize>,
    spectra_scored: BTreeMap<&'static str, usize>,
    indexed_sources: Vec<String>,
    score: &'static str,
    interpretation: &'static str,
}

#[derive(Clone)]
pub struct Library {
    path: PathBuf,
}

#[derive(Debug, Clone, Copy)]
struct Hit {
    score: f32,
    source: usize,
    row: usize,
}

impl PartialEq for Hit {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for Hit {}
impl PartialOrd for Hit {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Hit {
    fn cmp(&self, other: &Self) -> Ordering {
        self.score
            .total_cmp(&other.score)
            .then_with(|| self.source.cmp(&other.source))
            .then_with(|| self.row.cmp(&other.row))
    }
}

struct Collection {
    id: String,
    manifest: Manifest,
    folder: PathBuf,
    spectra: Mmap,
    bounds: Mmap,
    postings: Option<postings::Postings>,
}

fn map(path: &Path, size: usize) -> Result<Mmap> {
    let file = File::open(path)?;
    ensure!(
        file.metadata()?.len() == size as u64,
        "unexpected size for {}",
        path.display()
    );
    // The library format is published as immutable files; updates replace files atomically.
    Ok(unsafe { MmapOptions::new().map(&file) }?)
}

fn half_table() -> &'static [f32] {
    static TABLE: OnceLock<Box<[f32]>> = OnceLock::new();
    TABLE.get_or_init(|| {
        (0..=u16::MAX)
            .map(|bits| f16::from_bits(bits).to_f32())
            .collect()
    })
}

fn read_half(bytes: [u8; 2], table: &[f32]) -> f32 {
    table[u16::from_le_bytes(bytes) as usize]
}

impl Collection {
    fn open(folder: PathBuf, id: String, manifest: Manifest, open_index: bool) -> Result<Self> {
        let spectra = map(&folder.join("spectra.f16"), manifest.rows * ROW_BYTES)?;
        let bounds = map(&folder.join("bounds.u16"), manifest.rows * 4)?;
        let index = folder.join(postings::FILE);
        let postings = if open_index && index.exists() {
            Some(
                postings::Postings::open(&index, manifest.rows)
                    .with_context(|| format!("invalid peak index for {id}"))?,
            )
        } else {
            None
        };
        Ok(Self {
            id,
            manifest,
            folder,
            spectra,
            bounds,
            postings,
        })
    }

    fn load_values(&self, row: usize, output: &mut [f32]) {
        let table = half_table();
        for (value, bytes) in output.iter_mut().zip(
            self.spectra[row * ROW_BYTES..(row + 1) * ROW_BYTES]
                .as_chunks::<2>()
                .0,
        ) {
            *value = read_half(*bytes, table);
        }
    }

    fn bounds(&self, row: usize) -> (usize, usize) {
        let i = row * 4;
        let lo = u16::from_le_bytes([self.bounds[i], self.bounds[i + 1]]) as usize;
        let hi = u16::from_le_bytes([self.bounds[i + 2], self.bounds[i + 3]]) as usize;
        (lo.min(WIDTH), hi.min(WIDTH))
    }

    fn overlap(&self, row: usize, q: &Fingerprint) -> (usize, usize) {
        let (lo, hi) = self.bounds(row);
        (lo.max(q.start), hi.min(q.end))
    }

    fn covered(&self, row: usize, q: &Fingerprint) -> bool {
        let (a, b) = self.overlap(row, q);
        b.saturating_sub(a) * 2 >= q.end - q.start
    }

    fn score(&self, row: usize, q: &Fingerprint, prefix: &[f64], table: &[f32]) -> f32 {
        let (a, b) = self.overlap(row, q);
        let width = b.saturating_sub(a);
        let q_width = q.end - q.start;
        if width * 2 < q_width {
            return 0.0;
        }
        let offset = row * ROW_BYTES;
        let data = &self.spectra[offset + a * 2..offset + b * 2];
        let values = &q.values[a..b];
        // Accumulate independent lanes to avoid a single dependency chain in the hot loop.
        let mut dot = [0_f32; 4];
        let mut power = [0_f32; 4];
        let (blocks, tail) = data.as_chunks::<8>();
        let (queries, query_tail) = values.as_chunks::<4>();
        for (encoded, query) in blocks.iter().zip(queries) {
            for i in 0..4 {
                let v = read_half([encoded[2 * i], encoded[2 * i + 1]], table);
                dot[i] += v * query[i];
                power[i] += v * v;
            }
        }
        for (&encoded, &value) in tail.as_chunks::<2>().0.iter().zip(query_tail) {
            let v = read_half(encoded, table);
            dot[0] += v * value;
            power[0] += v * v;
        }
        let norm = ((prefix[b] - prefix[a]) * f64::from(power.into_iter().sum::<f32>())).sqrt();
        if !norm.is_finite() || norm <= 1e-9 {
            return 0.0;
        }
        (f64::from(dot.into_iter().sum::<f32>()) / norm).clamp(0.0, 1.0) as f32
            * (width as f32 / q_width as f32).sqrt()
    }
}

impl Library {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn manifests(&self) -> Result<BTreeMap<String, Manifest>> {
        let entries = match fs::read_dir(&self.path) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
            Err(err) => return Err(err.into()),
        };
        let mut found = BTreeMap::new();
        for entry in entries {
            let entry = entry?;
            let id = entry.file_name().into_string().ok();
            let manifest = entry.path().join("manifest.json");
            if let Some(id) = id.filter(|id| !id.starts_with('.'))
                && manifest.is_file()
            {
                let data: Manifest = serde_json::from_reader(File::open(&manifest)?)
                    .with_context(|| format!("read {}", manifest.display()))?;
                data.validate()
                    .with_context(|| format!("validate {}", manifest.display()))?;
                found.insert(id, data);
            }
        }
        Ok(found)
    }

    pub fn status(&self) -> Result<Value> {
        let sources = self
            .manifests()?
            .into_iter()
            .map(|(id, mut manifest)| {
                manifest.extra.insert(
                    "peak_index".into(),
                    self.path.join(&id).join(postings::FILE).is_file().into(),
                );
                (id, manifest)
            })
            .collect::<BTreeMap<_, _>>();
        Ok(serde_json::json!({
            "device": "SU(3) Raman",
            "mode": "offline spectral analysis",
            "hardware_acquisition": false,
            "engine": "rust",
            "sources": sources,
        }))
    }

    fn collections(&self, kind: Kind, ids: &[String], mode: Mode) -> Result<Vec<Collection>> {
        let manifests = self.manifests()?;
        if let Some(id) = ids.iter().find(|id| !manifests.contains_key(*id)) {
            return Err(Invalid(format!("Source not loaded: {id}.")).into());
        }
        manifests
            .into_iter()
            .filter(|(id, meta)| {
                (kind == Kind::Both || kind == meta.kind) && (ids.is_empty() || ids.contains(id))
            })
            .map(|(id, meta)| {
                let open_index = mode != Mode::Exact && meta.rows >= MIN_INDEXED_ROWS;
                Collection::open(self.path.join(&id), id, meta, open_index)
            })
            .collect()
    }

    pub fn build(&self, ids: &[String]) -> Result<()> {
        for c in self.collections(Kind::Both, ids, Mode::Exact)? {
            let count = postings::build(
                &c.folder.join(postings::FILE),
                c.manifest.rows,
                |row, values| c.load_values(row, values),
            )?;
            println!(
                "{}: {} spectra, {count} peak postings",
                c.id, c.manifest.rows
            );
        }
        Ok(())
    }

    pub fn search(&self, input: SearchRequest) -> Result<Results> {
        let limit = input.limit.unwrap_or(10);
        if !(1..=30).contains(&limit) {
            return Err(Invalid("Use a limit of 1–30.".into()).into());
        }
        let q = signal::fingerprint(&input.wavenumber, &input.intensity)?;
        let kind = input.kind.unwrap_or(Kind::Measured);
        let mode = input.mode.unwrap_or_default();
        let sources = self.collections(kind, input.sources.as_deref().unwrap_or(&[]), mode)?;
        let (hits, references_searched, spectra_scored, indexed_sources) =
            rank(&sources, &q, mode, limit, SHORTLIST)?;
        let mut databases = HashMap::new();
        let mut matches = BTreeMap::new();
        for (group, hits) in hits {
            let mut candidates = Vec::new();
            for hit in hits {
                let c = &sources[hit.source];
                let db = match databases.entry(hit.source) {
                    std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
                    std::collections::hash_map::Entry::Vacant(entry) => entry.insert(
                        Connection::open_with_flags(
                            c.folder.join("catalog.sqlite"),
                            OpenFlags::SQLITE_OPEN_READ_ONLY,
                        )
                        .with_context(|| format!("open {} catalog", c.id))?,
                    ),
                };
                let (reference, name, smiles, provenance): (
                    String,
                    Option<String>,
                    Option<String>,
                    String,
                ) = db.query_row(
                    "SELECT ref, name, smiles, provenance FROM reference WHERE rowid = ?",
                    [hit.row as i64 + 1],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )?;
                candidates.push(Candidate {
                    source: c.id.clone(),
                    reference,
                    name,
                    smiles,
                    provenance: serde_json::from_str(&provenance)?,
                    score: signal::rounded(f64::from(hit.score), 10_000.0),
                });
            }
            matches.insert(group, candidates);
        }
        Ok(Results {
            matches,
            references_searched,
            spectra_scored,
            indexed_sources,
            score: "Coverage-weighted fingerprint cosine similarity from 0 to 1",
            interpretation: "Ranked reference candidates. Similarity is not a probability or a confirmed identification.",
        })
    }

    pub fn evaluate(
        &self,
        samples: usize,
        limit: usize,
        shortlist: usize,
        ids: &[String],
    ) -> Result<Value> {
        ensure!(
            samples > 0 && (1..=30).contains(&limit) && (limit..=32768).contains(&shortlist),
            "invalid sample count or result limit"
        );
        let sources = self.collections(Kind::Both, ids, Mode::Auto)?;
        let count: usize = sources.iter().map(|c| c.manifest.rows).sum();
        ensure!(count > 0, "no reference collections");
        let samples = samples.min(count);
        let mut exact_ms = Vec::new();
        let mut indexed_ms = Vec::new();
        let mut recalled = 0;
        let mut expected = 0;
        let mut top_hit = 0;
        let mut compared = 0;
        for i in 0..samples {
            let mut row = ((2 * i + 1) * count) / (2 * samples);
            let source = sources
                .iter()
                .find(|c| {
                    if row < c.manifest.rows {
                        true
                    } else {
                        row -= c.manifest.rows;
                        false
                    }
                })
                .unwrap();
            let (start, end) = source.bounds(row);
            if end <= start {
                continue;
            }
            let mut values = vec![0.0; WIDTH];
            source.load_values(row, &mut values);
            let query = Fingerprint { values, start, end };
            let now = Instant::now();
            let (exact, _, _, _) = rank(&sources, &query, Mode::Exact, limit, shortlist)?;
            exact_ms.push(now.elapsed().as_secs_f64() * 1000.0);
            let now = Instant::now();
            let (indexed, _, _, _) = rank(&sources, &query, Mode::Indexed, limit, shortlist)?;
            indexed_ms.push(now.elapsed().as_secs_f64() * 1000.0);
            for (&group, hits) in &exact {
                let other = &indexed[group];
                expected += hits.len();
                recalled += hits
                    .iter()
                    .filter(|h| other.iter().any(|r| h.source == r.source && h.row == r.row))
                    .count();
                if let Some(h) = hits.first() {
                    compared += 1;
                    top_hit += usize::from(
                        other
                            .first()
                            .is_some_and(|r| h.source == r.source && h.row == r.row),
                    );
                }
            }
        }
        exact_ms.sort_unstable_by(f64::total_cmp);
        indexed_ms.sort_unstable_by(f64::total_cmp);
        ensure!(!exact_ms.is_empty(), "no usable samples");
        Ok(serde_json::json!({
            "samples": exact_ms.len(), "references": count,
            "exact_median_ms": exact_ms[exact_ms.len()/2],
            "indexed_median_ms": indexed_ms[indexed_ms.len()/2],
            "recall_at_k": recalled as f64 / expected.max(1) as f64,
            "top_1_agreement": top_hit as f64 / compared.max(1) as f64,
            "k": limit, "shortlist_per_source": shortlist,
            "benchmark": "Reference-derived queries with warm memory mappings; ranking only."
        }))
    }
}

type GroupMap<T> = BTreeMap<&'static str, T>;
type Ranking = (
    GroupMap<Vec<Hit>>,
    GroupMap<usize>,
    GroupMap<usize>,
    Vec<String>,
);

fn rank(
    sources: &[Collection],
    q: &Fingerprint,
    mode: Mode,
    limit: usize,
    shortlist: usize,
) -> Result<Ranking> {
    let mut prefix = vec![0.0; WIDTH + 1];
    for (i, &value) in q.values.iter().enumerate() {
        prefix[i + 1] = prefix[i] + f64::from(value).powi(2);
    }
    let mut heaps = GroupMap::from([
        ("measured", BinaryHeap::new()),
        ("computed", BinaryHeap::new()),
    ]);
    let mut counts = GroupMap::from([("measured", 0), ("computed", 0)]);
    let mut scored = counts.clone();
    let mut indexed = Vec::new();
    let table = half_table();
    for (source, c) in sources.iter().enumerate() {
        let group = c.manifest.kind.label();
        *counts.get_mut(group).unwrap() += c.manifest.rows;
        let heap = heaps.get_mut(group).unwrap();
        let mut check = |row| {
            let score = c.score(row, q, &prefix, table);
            if score <= 0.0 || !score.is_finite() {
                return;
            }
            let hit = Hit { score, source, row };
            if heap.len() < limit {
                heap.push(Reverse(hit));
            } else if hit > heap.peek().unwrap().0 {
                *heap.peek_mut().unwrap() = Reverse(hit);
            }
        };
        let mut rows = None;
        if mode != Mode::Exact && c.manifest.rows >= MIN_INDEXED_ROWS {
            if let Some(postings) = &c.postings {
                let candidates = postings.shortlist(q, shortlist, |row| c.covered(row, q))?;
                if candidates.len() >= limit || mode == Mode::Indexed {
                    rows = Some(candidates);
                }
            } else if mode == Mode::Indexed {
                return Err(Invalid(format!("Build the peak index for {} first.", c.id)).into());
            }
        }
        if let Some(rows) = rows {
            *scored.get_mut(group).unwrap() += rows.len();
            indexed.push(c.id.clone());
            rows.into_iter().for_each(&mut check);
        } else {
            *scored.get_mut(group).unwrap() += c.manifest.rows;
            (0..c.manifest.rows).for_each(&mut check);
        }
    }
    let hits = heaps
        .into_iter()
        .map(|(group, heap)| {
            let mut list: Vec<_> = heap.into_iter().map(|hit| hit.0).collect();
            list.sort_unstable_by(|a, b| b.cmp(a));
            (group, list)
        })
        .collect();
    Ok((hits, counts, scored, indexed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_reads_existing_index_and_separates_sources() -> Result<()> {
        let data = tempfile::tempdir()?;
        let wavenumber: Vec<_> = (200..=4000).step_by(4).map(f64::from).collect();
        let intensity: Vec<_> = wavenumber
            .iter()
            .map(|&v| (-0.5 * ((v - 918.0) / 7.0).powi(2)).exp())
            .collect();
        let q = signal::fingerprint(&wavenumber, &intensity)?;
        for id in ["computed", "measured"] {
            let folder = data.path().join(id);
            fs::create_dir(&folder)?;
            fs::write(
                folder.join("manifest.json"),
                serde_json::to_vec(&serde_json::json!({
                    "version": 1, "rows": 1, "kind": id, "title": id
                }))?,
            )?;
            fs::write(
                folder.join("spectra.f16"),
                q.values
                    .iter()
                    .flat_map(|&v| f16::from_f32(v).to_bits().to_le_bytes())
                    .collect::<Vec<_>>(),
            )?;
            fs::write(
                folder.join("bounds.u16"),
                [0_u16, WIDTH as u16]
                    .into_iter()
                    .flat_map(u16::to_le_bytes)
                    .collect::<Vec<_>>(),
            )?;
            let db = Connection::open(folder.join("catalog.sqlite"))?;
            db.execute_batch(
                "CREATE TABLE reference (ref TEXT, name TEXT, smiles TEXT, provenance TEXT);",
            )?;
            db.execute("INSERT INTO reference VALUES (?, ?, NULL, '{}')", [id, id])?;
        }
        let library = Library::new(data.path().into());
        let results = library.search(SearchRequest {
            wavenumber,
            intensity,
            kind: Some(Kind::Both),
            sources: None,
            limit: None,
            mode: Some(Mode::Exact),
        })?;
        assert_eq!(results.matches["measured"][0].score, 1.0);
        assert_eq!(results.matches["computed"][0].reference, "computed");
        assert_eq!(results.spectra_scored["measured"], 1);
        Ok(())
    }
}
