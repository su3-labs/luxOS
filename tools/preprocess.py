"""Offline preprocessing for published reference datasets."""
import numpy as np
from scipy.ndimage import gaussian_filter1d, grey_opening

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
