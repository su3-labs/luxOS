# LuxOS

LuxOS is an open-source Raman instrument system for small ARM64 computers. It layers a Wi-Fi setup portal, local web app, REST API, and MCP server over the board vendor's Linux image.

| Board | Install path |
| --- | --- |
| Radxa ZERO 3W | Build an SD image from official Radxa OS Bookworm `rsdk-b1`. |
| Raspberry Pi | Install on 64-bit Raspberry Pi OS Lite Bookworm or Trixie. The board needs a Wi-Fi interface that supports AP mode. |

The ARM64 runtime is tested on Bookworm and Trixie. Raspberry Pi radio and hotspot behavior still needs a physical board check.

The service takes already calibrated Raman shifts and intensities. It detects peaks and ranks reference spectra on the CPU. The default search uses measured references. Calculated references form a separate group. Match scores express similarity.

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
sudo ./scripts/install.sh
```

The installer preserves the Pi's hostname and existing SSH account. It uses the NetworkManager setup shipped with recent Raspberry Pi OS. Pass `--library /path/to/library.tar.gz` to install an existing local bundle. [Raspberry Pi OS](https://www.raspberrypi.com/documentation/computers/os.html)

## Connect and search

1. If the board has no active Wi-Fi, join `SU3-Raman-Setup`. Get the passphrase from the Radxa build output or read `sudo cat /etc/su3/setup-passphrase` on an installed device.
2. Open [Wi-Fi setup](http://10.42.77.1/) and enter your network details. The portal supports open networks, WPA Personal, and PEAP/MSCHAPv2 with a system-trusted certificate and the RADIUS domain.
3. Rejoin your Wi-Fi and open `http://<hostname>.local:8000/`. On a fresh Radxa image the hostname is `su3-raman`. The site accepts a two-column spectrum file with shifts in cm⁻¹ and intensity.

The REST endpoints are `GET /api/status`, `POST /api/peaks`, and `POST /api/search`. Peak and search requests use paired `wavenumber` and `intensity` arrays. Search accepts `kind` (`measured`, `computed`, or `both`), an optional list of source IDs, and a result limit. The service exposes its schema at `/openapi.json`.

For MCP, connect through SSH. Use your board's SSH account and hostname.

```sh
ssh -N -L 8765:127.0.0.1:8765 su3@su3-raman.local
```

Point the client at `http://127.0.0.1:8765/mcp` using Streamable HTTP. The tools are `raman_status`, `detect_peaks`, and `search_library`.

## Reference libraries

Download and retain source files separately. Each upstream dataset has its own license and attribution. See [Reference libraries](tools/README.md) for sources and the import process. The MIT license covers LuxOS source code.

## Development

```sh
python3 -m venv .venv
. .venv/bin/activate
python -m pip install -r requirements.txt
PYTHONPATH=runtime python -m unittest discover -s tests
```
