# LuxOS

LuxOS is an open-source Raman instrument system for small ARM64 computers. It layers a Wi-Fi setup portal, local web app, REST API, and MCP server over the board vendor's Linux image.

| Board | Install path |
| --- | --- |
| Radxa ZERO 3W | Build an SD image from official Radxa OS Bookworm `rsdk-b1`. |
| Raspberry Pi | Install on 64-bit Raspberry Pi OS Lite Bookworm or Trixie. The board needs a Wi-Fi interface that supports AP mode. |

The ARM64 runtime is tested on Bookworm and Trixie. Raspberry Pi radio and hotspot behavior still needs a physical board check.

The device runs one Rust executable for Wi-Fi setup, library loading, peak detection, reference search, REST, and MCP. The web UI uses HTML and JavaScript. The service accepts calibrated Raman shifts and intensities and searches on the CPU. The default search uses measured references. Calculated references form a separate group. Match scores express similarity.

## Radxa ZERO 3W

Install Docker, curl, xz, OpenSSL, and OpenSSH on the build host. Use a Docker engine with ARM64 support and loop mounts. The builder checks the official image SHA-512, preserves its boot partitions, and writes to a file.

```sh
./scripts/build-radxa.sh --ssh-key ~/.ssh/id_ed25519.pub --output ./out
```

Use `--stock-image` with a previously downloaded copy of the pinned Radxa image. Pass `--library /path/to/library.tar.gz` to preload a local reference bundle. The Radxa first-boot resize makes room for the library before it unpacks. Each build generates a fresh hotspot passphrase and writes it to the private `luxos-radxa-zero3w.setup-passphrase` file next to the image.

Flash `out/luxos-radxa-zero3w.img` to a microSD using an image writer. The image accepts key-based SSH as `su3` and generates its host keys on first boot. [Radxa OS release](https://github.com/radxa-build/radxa-zero3/releases/tag/rsdk-b1)

## Raspberry Pi OS Lite

Install and boot the official 64-bit Raspberry Pi OS Lite. Set up temporary Ethernet or Wi-Fi for dependency installation, then run this from a clone on the Pi.

```sh
cargo build --release --locked
sudo ./scripts/install.sh
```

Build with Rust 1.92 or newer, or pass an existing ARM64 Linux executable with `sudo ./scripts/install.sh --binary /path/to/luxos`. The installer preserves the Pi's hostname and existing SSH account. It uses the NetworkManager setup shipped with recent Raspberry Pi OS. Pass `--library /path/to/library.tar.gz` to install an existing local bundle. [Raspberry Pi OS](https://www.raspberrypi.com/documentation/computers/os.html)

## Connect and search

1. If the board has no active Wi-Fi, join `SU3-Raman-Setup`. Get the passphrase from the Radxa build output or read `sudo cat /etc/su3/setup-passphrase` on an installed device.
2. Open [Wi-Fi setup](http://10.42.77.1/) and enter your network details. The portal supports open networks, WPA Personal, and PEAP/MSCHAPv2 with a system-trusted certificate and the RADIUS domain.
3. Rejoin your Wi-Fi and open `http://<hostname>.local:8000/`. On a fresh Radxa image the hostname is `su3-raman`. The site accepts a two-column spectrum file with shifts in cm⁻¹ and intensity.

The REST endpoints are `GET /api/status`, `POST /api/peaks`, and `POST /api/search`. Peak and search requests use paired `wavenumber` and `intensity` arrays. Search accepts `kind` (`measured`, `computed`, or `both`), an optional list of source IDs, a result limit, and a search mode. The service exposes its schema at `/openapi.json`.

For MCP, connect through SSH. Use your board's SSH account and hostname.

```sh
ssh -N -L 8765:127.0.0.1:8765 su3@su3-raman.local
```

Point the client at `http://127.0.0.1:8765/mcp` using Streamable HTTP. The tools are `raman_status`, `detect_peaks`, and `search_library`.

## Indexed search

LuxOS reads the existing v1 spectral libraries. To enable faster search, build peak postings on a machine with access to the library.

```sh
luxos --index /data/raman/indexes build-index
luxos --index /data/raman/indexes evaluate --samples 48
```

`auto` uses the peak index for collections of at least 10,000 entries when the sidecar is available. It shortlists up to 4,096 references per collection and reranks them with the same coverage-weighted cosine score as exact search. `exact` scans all eligible references. `indexed` requires the sidecar for large collections. Responses report which collections used the index and how many full spectra were scored.

On Quark, 48 reference-derived queries over 653,528 spectra took 15.7 ms median with the index and 156.6 ms with exact Rust search. Indexed results recovered 97.1% of the exact top 10. These are warm ranking timings. Physical Radxa timings and performance on independently acquired spectra need separate validation.

One collection can address up to 10 million entries. At that size the existing 951-point float16 vectors occupy about 19 GB, and peak postings occupy up to 960 MB. Full reference vectors stay on disk. A 10-million-row candidate score buffer occupies 40 MB.

## Reference libraries

Download and retain source files separately. Each upstream dataset has its own license and attribution. See [Reference libraries](tools/README.md) for sources and the import process. The MIT license covers LuxOS source code.

## Development

```sh
cargo test --locked
cargo fmt --check
cargo test --release --locked ten_million_row_capacity -- --ignored --nocapture
```

The final command exercises the shortlist on a synthetic 10-million-row postings file. The dataset conversion tools are optional Python programs for the machine that prepares the reference library.
