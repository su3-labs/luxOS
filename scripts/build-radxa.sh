#!/usr/bin/env bash
set -euo pipefail
project=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
filename=radxa-zero3_bookworm_kde_b1.output_512.img.xz
url=https://github.com/radxa-build/radxa-zero3/releases/download/rsdk-b1/$filename
digest=6f9f67df6f997bef41aac2cc568ebb4b7820216be1256a49ce472cb877684c08ad793ff726da3155a02f0eab60b2b2c9318168b3cf8fab81849ae91e8724f10d
out=$project/out
key=
stock=
bundle=

usage() {
    printf 'Usage: %s --ssh-key PUBLIC_KEY [--output DIR] [--stock-image IMG.XZ] [--library BUNDLE]\n' "$0"
}
while (( $# )); do
    case "$1" in
        --ssh-key) key=${2:?}; shift 2 ;;
        --output) out=${2:?}; shift 2 ;;
        --stock-image) stock=${2:?}; shift 2 ;;
        --library) bundle=${2:?}; shift 2 ;;
        --help|-h) usage; exit 0 ;;
        *) usage >&2; exit 2 ;;
    esac
done
[[ -n "$key" ]] || { usage >&2; exit 2; }
ssh-keygen -lf "$key" >/dev/null
mkdir -p "$out"
out=$(cd -- "$out" && pwd)
target=$out/luxos-radxa-zero3w.img
password=$out/luxos-radxa-zero3w.setup-passphrase
[[ ! -e "$target" && ! -e "$password" ]] || { echo "Output already exists" >&2; exit 1; }
if [[ -z "$stock" ]]; then
    stock=$out/$filename
    if [[ ! -f "$stock" ]]; then
        curl --fail --location --retry 3 --output "$stock.part" "$url"
        mv "$stock.part" "$stock"
    fi
fi
if command -v sha512sum >/dev/null; then
    actual=$(sha512sum "$stock")
else
    actual=$(shasum -a 512 "$stock")
fi
[[ ${actual%% *} == "$digest" ]] || { echo "Radxa publisher checksum mismatch" >&2; exit 1; }
stage=$(mktemp -d "$out/.luxos-build.XXXXXX")
trap 'rm -rf "$stage"' EXIT
umask 077
install -m 0600 "$key" "$stage/authorized_keys"
openssl rand -hex 12 > "$stage/setup-passphrase"
xz -dc "$stock" > "$stage/system.img"
mounts=(--mount "type=bind,source=$project,target=/source,readonly" --mount "type=bind,source=$stage,target=/work")
if [[ -n "$bundle" ]]; then
    bundle=$(cd -- "$(dirname -- "$bundle")" && pwd)/$(basename -- "$bundle")
    mounts+=(--mount "type=bind,source=$bundle,target=/input-library.tar.gz,readonly")
fi
docker build --platform linux/arm64 --file "$project/scripts/radxa-build.Dockerfile" \
    --tag luxos-radxa-builder:bookworm "$project"
docker run --rm --privileged --platform linux/arm64 "${mounts[@]}" \
    luxos-radxa-builder:bookworm bash /source/scripts/configure-radxa-image.sh
mv "$stage/system.img" "$target"
mv "$stage/setup-passphrase" "$password"
if command -v sha256sum >/dev/null; then
    (cd "$out"; sha256sum "${target##*/}") > "$target.sha256"
else
    (cd "$out"; shasum -a 256 "${target##*/}") > "$target.sha256"
fi
printf 'Image written to %s\n' "$target"
printf 'Hotspot passphrase saved to %s\n' "$password"
