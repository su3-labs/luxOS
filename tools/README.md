# Reference libraries

LuxOS indexes selected Raman spectra from these sources. Each source keeps its own metadata and provenance. Large downloads and generated indexes belong in storage outside the checkout.

| Source ID | Method | Download |
| --- | --- | --- |
| `arcadia` | Experimental | [Arcadia DIY Raman](https://github.com/Arcadia-Science/2025-diyraman-bio) |
| `galway` | Experimental | [Pharmaceutical compounds](https://doi.org/10.6084/m9.figshare.27931131) |
| `openspecy` | Experimental | [Open Specy raw library](https://d2jrxerjcsjhs7.cloudfront.net/raw.rds) |
| `ramanbiolib` | Digitized experimental literature | [RamanBioLib](https://github.com/mteranm/ramanbiolib) |
| `mol2raman` | Calculated | [Mol2Raman training data](https://doi.org/10.5281/zenodo.15391264) |
| `qm9s` | Harmonic DFT | [QM9S](https://figshare.com/articles/dataset/QM9S_dataset/24235333) |
| `qme14s` | Harmonic DFT | [QMe14S files](https://figshare.com/s/889262a4e999b5c9a5b3) |
| `raman-chembl` | Harmonic DFT | [Part 1](https://figshare.com/ndownloader/files/54662735), [Part 2](https://figshare.com/ndownloader/files/54662885) |

Create one folder per source ID under your source directory. Store the original release files there. For Arcadia and RamanBioLib, clone the projects into their respective folders. QM9S uses both the broadened Raman CSV and the molecule metadata ZIP. QMe14S uses `Raman_broaden.zip`. The ChEMBL importer uses both SQLite files.

```sh
python -m pip install -r tools/requirements.txt
python tools/import_refs.py galway --sources /data/raman/sources --output /data/raman/indexes
luxos --index /data/raman/indexes build-index
python tools/pack-library.py /data/raman/indexes/galway --output /data/raman/library.tar.gz
```

Repeat the import for each desired source and pass all its index folders to the pack command. Packing includes a peak sidecar when present. Rebuild the sidecar after replacing a source. For Mol2Raman, verify the publisher's checksum and run the importer in an offline, unprivileged container with a read-only source mount. That import requires `--allow-publisher-pickle`.

The index stores fingerprint-weighted vectors on a shared 200–4000 cm⁻¹ grid. The importer records skipped rows. Raman-ChEMBL Raman activities are converted to relative Stokes intensities using a 785 nm excitation assumption and 298.15 K, with the modeled parameters recorded per entry. QM9S and QMe14S overlap. Spectrum counts include repeated measurements and repeated structures.

Review upstream terms before redistributing an index. Open Specy includes source-specific terms. RamanBioLib is under the ODbL. Keep calculated and measured matches separate when interpreting results.
