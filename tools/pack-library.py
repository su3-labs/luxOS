#!/usr/bin/env python3
"""Pack one or more compiled reference collections without their raw sources."""
import argparse
import hashlib
import re
import tarfile
from pathlib import Path

FILES = ("manifest.json", "catalog.sqlite", "spectra.f16", "bounds.u16")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("index", nargs="+", type=Path, help="Compiled per-source index folders")
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    temporary = args.output.with_name(args.output.name + ".part")
    ids = set()
    with temporary.open("xb") as output:
        try:
            with tarfile.open(fileobj=output, mode="w:gz", compresslevel=3) as archive:
                for source in args.index:
                    name = source.name
                    if not re.fullmatch(r"[a-z0-9][a-z0-9-]{0,63}", name) or name in ids:
                        raise ValueError(f"Invalid or duplicate collection name: {name}")
                    ids.add(name)
                    for file in FILES:
                        path = source / file
                        if not path.is_file() or path.is_symlink():
                            raise ValueError(f"Missing regular index file: {path}")
                        archive.add(path, arcname=f"{name}/{file}")
        except BaseException:
            temporary.unlink()
            raise
    temporary.replace(args.output)
    with args.output.open("rb") as source:
        digest = hashlib.file_digest(source, "sha256").hexdigest()
    print(f"{digest}  {args.output}")


if __name__ == "__main__":
    main()
