//! Install reference bundles atomically, including existing v1 archives.
use crate::{postings, search::Manifest, signal::WIDTH};
use anyhow::{Context, Result, bail, ensure};
use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{self, BufReader, Seek},
    path::{Component, Path, PathBuf},
};

const REQUIRED: &[&str] = &[
    "manifest.json",
    "catalog.sqlite",
    "spectra.f16",
    "bounds.u16",
];

fn source_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('-')
        && name
            .bytes()
            .all(|v| v.is_ascii_lowercase() || v.is_ascii_digit() || v == b'-')
}

fn entry_path(path: &Path, is_dir: bool) -> Result<Option<(String, String)>> {
    let components: Vec<_> = path.components().collect();
    let Some(Component::Normal(id)) = components.first() else {
        bail!("invalid bundle path");
    };
    let id = id
        .to_str()
        .filter(|id| source_name(id))
        .context("invalid collection name")?;
    if is_dir && components.len() == 1 {
        return Ok(None);
    }
    let [_, Component::Normal(name)] = components.as_slice() else {
        bail!("invalid bundle path");
    };
    let name = name.to_str().context("invalid bundle filename")?;
    ensure!(
        REQUIRED.contains(&name) || name == postings::FILE,
        "unknown bundle file"
    );
    Ok(Some((id.into(), name.into())))
}

fn unpack(file: &mut File, into: &Path) -> Result<()> {
    let mut files = BTreeMap::new();
    let mut size = 0_u64;
    {
        let mut archive = tar::Archive::new(GzDecoder::new(BufReader::new(&mut *file)));
        for entry in archive.entries()? {
            let entry = entry?;
            let ty = entry.header().entry_type();
            ensure!(
                ty.is_dir() || ty.is_file(),
                "bundle entries must be regular files or directories"
            );
            if let Some(key) = entry_path(&entry.path()?, ty.is_dir())? {
                ensure!(ty.is_file(), "invalid bundle directory");
                ensure!(
                    files.insert(key, entry.size()).is_none(),
                    "duplicate bundle file"
                );
                size = size
                    .checked_add(entry.size())
                    .context("bundle is too large")?;
                ensure!(files.len() <= 320, "too many bundle files");
            }
        }
    }
    let ids: BTreeSet<_> = files.keys().map(|(id, _)| id.as_str()).collect();
    ensure!(!ids.is_empty(), "empty library bundle");
    for id in &ids {
        for name in REQUIRED {
            ensure!(
                files.contains_key(&(id.to_string(), name.to_string())),
                "incomplete collection {id}"
            );
        }
    }
    let disk = rustix::fs::statvfs(into)?;
    let free = disk.f_bavail.saturating_mul(disk.f_frsize);
    ensure!(
        free >= size.saturating_add(128 * 1024 * 1024),
        "Waiting for enough space to unpack the library (need {size} bytes plus headroom)."
    );
    file.rewind()?;
    let mut archive = tar::Archive::new(GzDecoder::new(BufReader::new(file)));
    for entry in archive.entries()? {
        let mut entry = entry?;
        let ty = entry.header().entry_type();
        if let Some((id, name)) = entry_path(&entry.path()?, ty.is_dir())? {
            let folder = into.join(id);
            fs::create_dir_all(&folder)?;
            let path = folder.join(name);
            let mut output = File::create_new(&path)?;
            io::copy(&mut entry, &mut output)?;
        }
    }
    for id in ids {
        let folder = into.join(id);
        let manifest: Manifest =
            serde_json::from_reader(File::open(folder.join("manifest.json"))?)?;
        manifest.validate()?;
        for (name, expected) in [
            ("spectra.f16", manifest.rows * WIDTH * 2),
            ("bounds.u16", manifest.rows * 4),
        ] {
            ensure!(
                fs::metadata(folder.join(name))?.len() == expected as u64,
                "invalid size for {id}/{name}"
            );
        }
        if folder.join(postings::FILE).is_file() {
            postings::Postings::open(&folder.join(postings::FILE), manifest.rows)?;
        }
    }
    Ok(())
}

pub fn install(state: PathBuf) -> Result<()> {
    let archive = state.join("library.tar.gz");
    let checksum = state.join("library.sha256");
    if !archive.exists() {
        return Ok(());
    }
    let expected = fs::read_to_string(&checksum)?;
    let expected = expected
        .split_whitespace()
        .next()
        .context("missing library checksum")?;
    let mut file = File::open(&archive)?;
    let mut sha = Sha256::new();
    io::copy(&mut file, &mut sha)?;
    let digest = format!("{:x}", sha.finalize());
    ensure!(
        digest.eq_ignore_ascii_case(expected),
        "reference bundle checksum mismatch"
    );
    let index = state.join("index");
    let receipt = index.join(".bundle-sha256");
    if fs::read_to_string(&receipt)
        .ok()
        .is_none_or(|old| old.trim() != digest)
    {
        ensure!(
            !index.exists() || fs::read_dir(&index)?.next().is_none(),
            "Move the existing index before installing a new bundle."
        );
        let staging = tempfile::Builder::new()
            .prefix(".library-")
            .tempdir_in(&state)?;
        file.rewind()?;
        unpack(&mut file, staging.path())?;
        fs::write(staging.path().join(".bundle-sha256"), format!("{digest}\n"))?;
        fs::rename(staging.path(), &index)?;
    }
    fs::remove_file(archive)?;
    fs::remove_file(checksum)?;
    println!("Installed the local Raman reference library");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn path_check_rejects_traversal_and_unknown_files() {
        assert!(entry_path(Path::new("source/../manifest.json"), false).is_err());
        assert!(entry_path(Path::new("/source/manifest.json"), false).is_err());
        assert!(entry_path(Path::new("source/private.txt"), false).is_err());
        assert!(entry_path(Path::new("source/peaks.lxo"), false).is_ok());
    }
}
