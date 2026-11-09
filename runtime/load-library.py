"""Install a LuxOS reference bundle after the boot filesystem expands."""
import hashlib
import json
import re
import shutil
import sys
import tarfile
import tempfile
from pathlib import Path

from spectra import INDEX_VERSION, WIDTH

HOME = Path("/var/lib/su3-raman")
ARCHIVE = HOME / "library.tar.gz"
CHECKSUM = HOME / "library.sha256"
FILES = {"manifest.json", "catalog.sqlite", "spectra.f16", "bounds.u16"}


def manifest(directory):
    item = json.loads((directory / "manifest.json").read_text())
    if (item["version"] != INDEX_VERSION or item["kind"] not in {"measured", "computed"}
            or type(item["rows"]) is not int or not 0 < item["rows"] <= 10_000_000):
        raise ValueError(f"Unsupported index manifest in {directory.name}")
    sizes = {"spectra.f16": item["rows"] * WIDTH * 2, "bounds.u16": item["rows"] * 4}
    for name, size in sizes.items():
        if (directory / name).stat().st_size != size:
            raise ValueError(f"Invalid size for {directory.name}/{name}")


def unpack(archive, directory):
    with tarfile.open(archive, "r:gz") as source:
        files = {}
        expanded = 0
        for item in source:
            parts = item.name.rstrip("/").split("/")
            if not re.fullmatch(r"[a-z0-9][a-z0-9-]{0,63}", parts[0]):
                raise ValueError(f"Invalid source name: {item.name}")
            if item.isdir() and len(parts) == 1:
                continue
            if not item.isfile() or len(parts) != 2 or parts[1] not in FILES or item.name in files:
                raise ValueError(f"Unexpected bundle member: {item.name}")
            files[item.name] = item
            expanded += item.size
            if len(files) > 256:
                raise ValueError("Too many index files")
        free = shutil.disk_usage(directory).free
        if expanded + 128 * 1024**2 > free:
            print("Waiting for enough space to unpack the library", file=sys.stderr)
            raise SystemExit(75)
        sources = {name.split("/", 1)[0] for name in files}
        if not sources or any(f"{name}/{filename}" not in files for name in sources for filename in FILES):
            raise ValueError("Incomplete reference collection")
        # Re-open the compressed archive for a single sequential extraction pass.
    with tarfile.open(archive, "r|gz") as source:
        for item in source:
            if item.name in files:
                destination = directory / item.name
                destination.parent.mkdir(exist_ok=True)
                with source.extractfile(item) as reader, destination.open("xb") as writer:
                    shutil.copyfileobj(reader, writer, 1024**2)
    for name in sources:
        manifest(directory / name)


def main():
    if not ARCHIVE.exists():
        return
    with ARCHIVE.open("rb") as source:
        digest = hashlib.file_digest(source, "sha256").hexdigest()
    if digest != CHECKSUM.read_text().split()[0]:
        raise ValueError("Reference bundle checksum mismatch")
    destination = HOME / "index"
    receipt = destination / ".bundle-sha256"
    if not receipt.exists() or receipt.read_text().strip() != digest:
        if destination.exists() and any(destination.iterdir()):
            raise ValueError("Move the existing index before installing a new bundle")
        with tempfile.TemporaryDirectory(prefix=".library-", dir=HOME) as tmp:
            staging = Path(tmp)
            unpack(ARCHIVE, staging)
            (staging / receipt.name).write_text(digest + "\n")
            staging.replace(destination)
    ARCHIVE.unlink()
    CHECKSUM.unlink(missing_ok=True)
    print("Installed the local Raman reference library")


if __name__ == "__main__":
    main()
