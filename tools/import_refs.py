"""Build immutable per-source indexes from the original Raman releases."""
import argparse
import ast
import csv
import io
import json
import pickle
import shutil
import sqlite3
import sys
import tempfile
import zipfile
import zlib
from pathlib import Path

import numpy as np
from scipy.ndimage import gaussian_filter1d

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "runtime"))
from spectra import GRID, INDEX_VERSION, vector

INFO = {
    "arcadia": ("measured", "Arcadia OpenRAMAN", "https://github.com/Arcadia-Science/2025-diyraman-bio"),
    "galway": ("measured", "University of Galway pharmaceutical ingredients", "https://doi.org/10.6084/m9.figshare.27931131"),
    "openspecy": ("measured", "Open Specy", "https://www.openspecy.org/"),
    "ramanbiolib": ("measured", "RamanBioLib digitized literature", "https://github.com/mteranm/ramanbiolib"),
    "mol2raman": ("computed", "Mol2Raman DFT training set", "https://doi.org/10.5281/zenodo.15391264"),
    "qm9s": ("computed", "QM9S harmonic DFT", "https://figshare.com/articles/dataset/QM9S_dataset/24235333"),
    "qme14s": ("computed", "QMe14S harmonic DFT", "https://figshare.com/s/889262a4e999b5c9a5b3"),
    "raman-chembl": ("computed", "Raman-ChEMBL PBE/6-31G harmonic DFT", "https://figshare.com/ndownloader/files/54662735"),
}


def record(key, x, y, name=None, smiles=None, **meta):
    return str(key), x, y, name, smiles, meta


def arcadia(path):
    with (path / "spectral_library/peaktable.csv").open() as handle:
        labels = {}
        for row in csv.DictReader(handle):
            filename = row["Processed Spectrum Filename (in data/processed/processed_data folder)"]
            if filename and row["Compound"]:
                labels[filename.removesuffix(".csv")] = row["Compound"]
    with zipfile.ZipFile(path / "data/processed.zip") as archive:
        for name in archive.namelist():
            file = Path(name)
            if "processed_data" not in file.parts or file.stem not in labels or file.suffix != ".csv":
                continue
            reader = csv.DictReader(io.TextIOWrapper(archive.open(name)))
            points = [(float(r["Raman shift (cm-1) adjusted"]), float(r["Baselined Filtered Intensity (a.u.)"]))
                      for r in reader]
            x, y = np.array(points).T
            yield record(name, x, y, name=labels[file.stem], excitation_nm=532)


def galway(path):
    with (path / "raman_spectra_api_compounds.csv").open() as handle:
        rows = csv.reader(handle)
        x = np.array(next(rows)[:-1], dtype=np.float64)
        for i, row in enumerate(rows):
            yield record(i + 1, x, np.array(row[:-1], dtype=np.float64), name=row[-1], excitation_nm=785)


def openspecy(path):
    import pandas as pd
    import rdata

    data = rdata.read_rds(path / "raw.rds")["raman"]
    wavenumber = data["wavenumber"]
    intensities = data["spectra"].to_numpy()
    assert intensities.shape == (len(wavenumber), len(data["metadata"]))
    for i, row in enumerate(data["metadata"].to_dict("records")):
        y = intensities[:, i]
        good = np.isfinite(y)
        clean = {k: str(row[k]) for k in ("material", "sample_name", "organization", "rruffid",
                 "url", "laser_wavelength", "license", "cas_number") if pd.notna(row[k])}
        key = clean.pop("sample_name", str(i))
        name = clean.pop("material", None)
        yield record(key, wavenumber[good], y[good], name=name, **clean)


def ramanbiolib(path):
    with (path / "ramanbiolib/db/metadata_db.csv").open() as handle:
        metadata = {r["id"]: {k: v for k, v in r.items() if k != "id" and v}
                    for r in csv.DictReader(handle)}
    with (path / "ramanbiolib/db/raman_spectra_db.csv").open() as handle:
        for row in csv.DictReader(handle):
            yield record(row["id"], ast.literal_eval(row["wavenumbers"]),
                         ast.literal_eval(row["intensity"]), name=row["component"],
                         literature=metadata.get(row["id"]), license="ODbL")


def mol2raman(path):
    # This importer is run in a dedicated, offline container with a read-only source mount.
    with (path / "dtf_data_smile.pickle").open("rb") as handle:
        frame = pickle.load(handle)
    for i, (smiles, x, y) in enumerate(frame.itertuples(index=False, name=None)):
        yield record(i + 1, x, y, smiles=smiles)


def qm9s(path):
    smiles = {}
    with zipfile.ZipFile(path / "qm9s_csv.zip") as archive:
        for name in archive.namelist():
            if name.endswith(".csv"):
                with archive.open(name) as handle:
                    next(handle)
                    _, index, qm9_id, structure, *_ = next(csv.reader([handle.readline().decode()]))
                    smiles[int(float(index)) - 1] = (structure, int(float(qm9_id)))
    with (path / "raman_boraden.csv").open() as handle:
        x = np.fromstring(handle.readline().split(",", 1)[1], sep=",")
        for line in handle:
            key, data = line.split(",", 1)
            structure, qm9_id = smiles[int(key)]
            yield record(key, x, np.fromstring(data, sep=","), smiles=structure, qm9_id=qm9_id)


def qme14s(path):
    with zipfile.ZipFile(path / "Raman_broaden.zip") as archive:
        for name in archive.namelist():
            if name.endswith(".csv"):
                rows = archive.read(name).splitlines()
                smiles = rows[0].decode().strip()
                y = np.fromstring(rows[-1].decode(), sep=",")
                if len(y) not in {3500, 3501}:
                    raise ValueError(f"Unexpected QMe14S grid in {name}: {len(y)}")
                x = np.arange(500, 500 + len(y))
                yield record(Path(name).stem, x, y, smiles=smiles, lorentzian_half_width_cm_1=10)


def raman_chembl(path):
    import orjson

    for database in sorted(path.glob("Raman-ChEMBL-part*.db")):
        uri = database.resolve().as_uri() + "?mode=ro&immutable=1"
        with sqlite3.connect(uri, uri=True) as connection:
            for key, smiles, compressed in connection.execute(
                "SELECT id, SMILES, blob_data FROM molecule ORDER BY id"
            ):
                parsed = orjson.loads(zlib.decompress(compressed))
                x = np.array(parsed["freq"])
                activity = np.array(parsed["Raman Activ"])
                good = (x >= GRID[0]) & (x <= GRID[-1]) & (activity > 0)
                x, activity = x[good], activity[good]
                signal = (1e7 / 785 - x) ** 4 * activity / (x * -np.expm1(-1.4387769 * x / 298.15))
                y = np.zeros_like(GRID)
                np.add.at(y, np.rint((x - GRID[0]) / 4).astype(int), signal)
                yield record(f"{database.stem}:{key}", GRID, gaussian_filter1d(y, 1.25),
                             smiles=smiles, modeled_excitation_nm=785, modeled_temperature_k=298.15,
                             harmonic_frequency_scale=1)


def build(sources, output, source, importer):
    kind, title, url = INFO[source]
    final = output / source
    if final.exists():
        raise FileExistsError(f"An index already exists at {final}")
    stage = Path(tempfile.mkdtemp(prefix=f".{source}.", dir=output))
    count = skipped = 0
    try:
        with sqlite3.connect(stage / "catalog.sqlite") as db, \
                (stage / "spectra.f16").open("wb") as spectra, \
                (stage / "bounds.u16").open("wb") as bounds:
            db.execute("CREATE TABLE reference (ref TEXT NOT NULL, name TEXT, smiles TEXT, provenance TEXT NOT NULL)")
            for key, x, y, name, smiles, meta in importer(sources / source):
                try:
                    y, lo, hi = vector(x, y)
                except ValueError:
                    skipped += 1
                    continue
                spectra.write(y.astype("<f2").tobytes())
                bounds.write(np.array([lo, hi], dtype="<u2").tobytes())
                db.execute("INSERT INTO reference VALUES (?,?,?,?)",
                           (key, name, smiles, json.dumps(meta, allow_nan=False)))
                count += 1
                if count % 10000 == 0:
                    print(f"{source} {count:,}", flush=True)
        if not count:
            raise ValueError(f"No usable reference spectra in {source}")
        (stage / "manifest.json").write_text(json.dumps({
            "version": INDEX_VERSION, "title": title, "kind": kind,
            "source_url": url, "rows": count, "rejected": skipped,
        }, indent=2) + "\n")
        stage.chmod(0o755)
        stage.rename(final)
        print(f"{source} complete: {count:,} indexed, {skipped:,} rejected", flush=True)
    except BaseException:
        shutil.rmtree(stage)
        raise


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("source", choices=INFO)
    parser.add_argument("--sources", type=Path, default=Path("/sources"))
    parser.add_argument("--output", type=Path, default=Path("/index"))
    parser.add_argument("--allow-publisher-pickle", action="store_true",
                        help="Enable Mol2Raman deserialization in an offline, isolated environment.")
    options = parser.parse_args()
    if options.source == "mol2raman" and not options.allow_publisher_pickle:
        parser.error("Run Mol2Raman in isolation and pass --allow-publisher-pickle.")
    options.output.mkdir(parents=True, exist_ok=True)
    build(options.sources, options.output, options.source, globals()[options.source.replace("-", "_")])


if __name__ == "__main__":
    main()
