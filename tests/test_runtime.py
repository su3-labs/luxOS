import importlib.util
import io
import json
import sqlite3
import tarfile
import tempfile
import unittest
from pathlib import Path

import numpy as np
from fastapi.testclient import TestClient

from api import app, library
from spectra import GRID, vector

RUNTIME = Path(__file__).resolve().parents[1] / "runtime"


def load_script(name):
    spec = importlib.util.spec_from_file_location(name, RUNTIME / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def spectrum():
    intensity = sum(np.exp(-.5 * ((GRID - peak) / 7) ** 2) for peak in (918, 1374, 2250))
    return {"wavenumber": GRID.tolist(), "intensity": intensity.tolist()}


class RuntimeTests(unittest.TestCase):
    def test_upload_peak_and_search(self):
        sample = spectrum()
        packed, lo, hi = vector(**sample)
        with tempfile.TemporaryDirectory() as tmp:
            previous, library.path = library.path, Path(tmp)
            self.addCleanup(setattr, library, "path", previous)
            for kind in ("measured", "computed"):
                folder = library.path / kind
                folder.mkdir()
                (folder / "manifest.json").write_text(json.dumps({
                    "version": 1, "rows": 1, "kind": kind, "title": kind
                }))
                packed.astype("<f2").tofile(folder / "spectra.f16")
                np.array([[lo, hi]], dtype="<u2").tofile(folder / "bounds.u16")
                with sqlite3.connect(folder / "catalog.sqlite") as db:
                    db.execute("CREATE TABLE reference (ref TEXT, name TEXT, smiles TEXT, provenance TEXT)")
                    db.execute("INSERT INTO reference VALUES (?, ?, ?, ?)", ("test", kind, None, "{}"))
            with TestClient(app) as client:
                self.assertIn("Raman station", client.get("/").text)
                response = client.post("/api/peaks", json=sample)
                self.assertEqual(response.status_code, 200, response.text)
                self.assertEqual(len(response.json()["peaks"]), 3)
                response = client.post("/api/search", json={**sample, "kind": "both"})
                self.assertEqual(response.status_code, 200, response.text)
                matches = response.json()["matches"]
                self.assertEqual(matches["measured"][0]["score"], 1)
                self.assertEqual(matches["computed"][0]["name"], "computed")
                self.assertEqual(client.post("/api/search", json={**sample, "wavenumber": [1] * 8}).status_code, 422)

    def test_setup_modes(self):
        wifi = load_script("su3-wifi")
        _, personal = wifi.credentials({"ssid": "Lab Wi-Fi", "password": "pass word-123"})
        self.assertIn("psk=pass\\sword-123", personal)
        _, enterprise = wifi.credentials({
            "ssid": "University", "username": "tester", "password": "pass",
            "domain": "radius.example.org"
        })
        self.assertIn("system-ca-certs=true", enterprise)
        self.assertIn("domain-suffix-match=radius.example.org", enterprise)
        with self.assertRaises(ValueError):
            wifi.credentials({"ssid": "University", "username": "tester", "password": "pass"})

    def test_bundle_validates_member_paths(self):
        loader = load_script("load-library")
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            archive = directory / "index.tar.gz"
            with tarfile.open(archive, "w:gz") as handle:
                member = tarfile.TarInfo("test/../bad")
                member.size = 1
                handle.addfile(member, io.BytesIO(b"x"))
            with self.assertRaises(ValueError):
                loader.unpack(archive, directory)
            self.assertFalse((directory / "bad").exists())
