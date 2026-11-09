"""CPU spectral processing and disk-backed reference search."""
import heapq
import json
import sqlite3
from contextlib import closing
from pathlib import Path

import numpy as np
from scipy.ndimage import gaussian_filter1d, grey_opening
from scipy.signal import find_peaks, peak_widths

GRID = np.arange(200, 4001, 4, dtype=np.float32)
WEIGHTS = np.where((GRID >= 400) & (GRID <= 1800), 1., .45).astype(np.float32)
WIDTH = len(GRID)
INDEX_VERSION = 1


def arrays(wavenumber, intensity):
    x = np.asarray(wavenumber, dtype=np.float64)
    y = np.asarray(intensity, dtype=np.float64)
    if x.ndim != 1 or x.shape != y.shape or not 8 <= x.size <= 65_536:
        raise ValueError("Supply 8–65,536 paired Raman shifts and intensities.")
    if not (np.isfinite(x).all() and np.isfinite(y).all()):
        raise ValueError("Raman shifts and intensities must be finite.")
    if np.all(np.diff(x) < 0):
        x, y = x[::-1], y[::-1]
    if not np.all(np.diff(x) > 0):
        raise ValueError("Raman shifts must be strictly monotonic.")
    return x, y


def vector(wavenumber, intensity):
    x, y = arrays(wavenumber, intensity)
    lo, hi = np.searchsorted(GRID, [x[0], x[-1]], side="left")
    hi = min(hi + (hi < WIDTH and GRID[hi] == x[-1]), WIDTH)
    if hi - lo < 12:
        raise ValueError("Too little spectrum within 200–4000 cm⁻¹.")
    sampled = np.interp(GRID[lo:hi], x, y)
    sampled = (sampled - sampled.min()) / max(np.ptp(sampled), 1e-300)
    features = np.maximum(sampled - grey_opening(sampled, size=31), 0.)
    features = gaussian_filter1d(features, sigma=1.5) * WEIGHTS[lo:hi]
    norm = np.linalg.norm(features)
    if not norm > 1e-8:
        raise ValueError("The spectrum has no usable features.")
    output = np.zeros(WIDTH, dtype=np.float32)
    output[lo:hi] = features / norm
    return output, int(lo), int(hi)


def peaks(wavenumber, intensity, min_prominence=.03, limit=30):
    x, y = arrays(wavenumber, intensity)
    if not 0 < min_prominence <= 1 or not 1 <= limit <= 100:
        raise ValueError("Use prominence in (0, 1] and a limit of 1–100.")
    if not np.ptp(y) > 0:
        return []
    y = (y - y.min()) / np.ptp(y)
    step = float(np.median(np.diff(x)))
    window = min(len(x) // 2 * 2 - 1, max(3, round(120 / step) // 2 * 2 + 1))
    corrected = y - grey_opening(y, size=window)
    indexes, props = find_peaks(corrected, prominence=np.max(corrected) * min_prominence)
    widths, _, left, right = peak_widths(corrected, indexes, rel_height=.5)
    positions = np.arange(len(x))
    widths = np.interp(right, positions, x) - np.interp(left, positions, x)
    strongest = np.argsort(props["prominences"])[-limit:][::-1]
    return [{
        "shift_cm_1": round(float(x[indexes[i]]), 3),
        "relative_prominence": round(float(props["prominences"][i] / np.max(corrected)), 4),
        "width_cm_1": round(float(widths[i]), 3),
    } for i in strongest]


class Library:
    def __init__(self, path):
        self.path = Path(path)

    def sources(self):
        sources = {}
        for path in sorted(self.path.glob("*/manifest.json")):
            item = json.loads(path.read_text())
            if item["version"] == INDEX_VERSION and item["rows"]:
                sources[path.parent.name] = item
        return sources

    def search(self, wavenumber, intensity, kind="measured", sources=None, limit=10):
        if kind not in {"measured", "computed", "both"} or not 1 <= limit <= 30:
            raise ValueError("Use measured, computed or both, with a limit of 1–30.")
        manifest = self.sources()
        if sources and (missing := set(sources) - manifest.keys()):
            raise ValueError(f"Source not loaded: {', '.join(sorted(missing))}.")
        q, qlo, qhi = vector(wavenumber, intensity)
        prefix = np.r_[0., np.cumsum(q.astype(np.float64) ** 2)]
        heaps = {"measured": [], "computed": []}
        searched = dict.fromkeys(heaps, 0)
        for name, item in manifest.items():
            group = item["kind"]
            if (kind != "both" and group != kind) or (sources and name not in sources):
                continue
            path = self.path / name
            count = item["rows"]
            refs = np.memmap(path / "spectra.f16", dtype="<f2", mode="r", shape=(count, WIDTH))
            bounds = np.memmap(path / "bounds.u16", dtype="<u2", mode="r", shape=(count, 2))
            searched[group] += count
            heap = heaps[group]
            for start in range(0, count, 2048):
                stop = min(count, start + 2048)
                block = refs[start:stop, qlo:qhi].astype(np.float32)
                lo = np.maximum(bounds[start:stop, 0], qlo)
                hi = np.minimum(bounds[start:stop, 1], qhi)
                overlap = np.maximum(hi.astype(int) - lo, 0) / (qhi - qlo)
                qpower = np.maximum(prefix[hi] - prefix[lo], 0)
                power = np.einsum("ij,ij->i", block, block)
                norm = np.sqrt(qpower * power)
                scores = np.divide(block @ q[qlo:qhi], norm, out=np.zeros_like(norm),
                                   where=(norm > 1e-9) & (overlap >= .5))
                scores = np.clip(scores, 0., 1.) * np.sqrt(overlap)
                k = min(limit, len(scores))
                for i in np.argpartition(scores, -k)[-k:]:
                    if scores[i] <= 0:
                        continue
                    candidate = (float(scores[i]), name, start + int(i))
                    if len(heap) < limit:
                        heapq.heappush(heap, candidate)
                    elif candidate > heap[0]:
                        heapq.heapreplace(heap, candidate)
        results = {}
        for group, heap in heaps.items():
            results[group] = [self.candidate(score, name, row)
                              for score, name, row in sorted(heap, reverse=True)]
        return {
            "matches": results,
            "references_searched": searched,
            "score": "Coverage-weighted fingerprint cosine similarity from 0 to 1",
            "interpretation": "Ranked reference candidates. Similarity is not a probability or a confirmed identification.",
        }

    def candidate(self, score, source, row):
        uri = (self.path / source / "catalog.sqlite").resolve().as_uri()
        with closing(sqlite3.connect(uri + "?mode=ro&immutable=1", uri=True)) as connection:
            key, name, smiles, provenance = connection.execute(
                "SELECT ref, name, smiles, provenance FROM reference WHERE rowid=?", (row + 1,)
            ).fetchone()
        return {"source": source, "reference": key, "name": name, "smiles": smiles,
                "provenance": json.loads(provenance), "score": round(score, 4)}
