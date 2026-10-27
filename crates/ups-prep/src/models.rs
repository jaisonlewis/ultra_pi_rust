//! Model + binary acquisition — `ups-prep models` downloads the
//! external pieces the ML stages need into the models dir:
//!   sherpa-onnx binaries (platform tarball from k2-fsa releases)
//!   spleeter int8 separation models
//!   parakeet-tdt-0.6b-v3 int8 ASR model
//!   rmvpe.onnx pitch model (served from our own release)

use anyhow::{bail, Context, Result};
use crate::say;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

const SHERPA_VER: &str = "v2.0.1";
const BASE: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/\
                    download";
const RMVPE_URL: &str = "https://github.com/jaisonlewis/ultra_pi_rust/\
                        releases/download/v0.1.0/rmvpe.onnx";

/// sherpa binary tarball for this OS/arch, as it appears in the
/// sherpa-onnx release assets.
fn sherpa_asset() -> Result<&'static str> {
    let asset = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => "win-x64-shared-MD-Release",
        ("windows", "aarch64") => "win-arm64-shared-MD-Release",
        ("linux", "x86_64") => "linux-x64-shared",
        ("linux", "aarch64") => "linux-aarch64-shared-cpu",
        ("macos", _) => "osx-universal2-shared",
        (os, arch) => bail!("no sherpa-onnx build for {os}/{arch}"),
    };
    Ok(asset)
}

/// Locate a sherpa binary: explicit `models/bin/`, then any unpacked
/// `sherpa-onnx-*/bin/` dir (keeps its ../lib rpath intact), then PATH.
pub fn find_bin(models: &Path, name: &str) -> Option<PathBuf> {
    let local = models.join("bin").join(name);
    if local.exists() {
        return Some(local);
    }
    if let Ok(rd) = std::fs::read_dir(models) {
        for e in rd.flatten() {
            let p = e.path();
            if !p.is_dir() {
                continue;
            }
            let is_sherpa = p
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with("sherpa-onnx-"))
                .unwrap_or(false);
            if is_sherpa {
                let b = p.join("bin").join(name);
                if b.exists() {
                    return Some(b);
                }
            }
        }
    }
    Command::new(name)
        .arg("--help")
        .output()
        .ok()
        .map(|_| PathBuf::from(name))
}

fn fetch(url: &str, dest: &Path, label: &str) -> Result<()> {
    say!("downloading {label}");
    let mut resp = reqwest::blocking::get(url)
        .with_context(|| format!("{label}: request"))?;
    if !resp.status().is_success() {
        bail!("{label}: http {}", resp.status());
    }
    let mut f = std::fs::File::create(dest)
        .with_context(|| format!("{label}: create {}", dest.display()))?;
    let (mut buf, mut got, mut mark) = (vec![0u8; 1 << 20], 0u64, 0u64);
    loop {
        let n = resp.read(&mut buf)?;
        if n == 0 {
            break;
        }
        f.write_all(&buf[..n])?;
        got += n as u64;
        if got - mark >= 128 << 20 {
            say!("  {label}: {} MB", got >> 20);
            mark = got;
        }
    }
    say!("  {label}: {} MB done", got >> 20);
    Ok(())
}

fn untar(archive: &Path, into: &Path, label: &str) -> Result<()> {
    let st = Command::new("tar")
        .args(["-xjf"])
        .arg(archive)
        .arg("-C")
        .arg(into)
        .status()
        .context("spawn tar")?;
    if !st.success() {
        bail!("{label}: tar extraction failed (need bsdtar — ships \
               with Windows 10+ and every unix)");
    }
    Ok(())
}

/// Download everything the ML stages need that isn't already under
/// `models`. Idempotent — existing dirs/files are skipped.
pub fn download(models: &Path) -> Result<()> {
    std::fs::create_dir_all(models)?;
    let tmp = models.join(".dl-tmp");
    std::fs::create_dir_all(&tmp)?;

    // sherpa binaries — platform tarball, unpacked as its own dir so
    // binaries keep their ../lib sibling rpaths; find_bin resolves it
    let have_sherpa = find_bin(
        models,
        if cfg!(windows) {
            "sherpa-onnx-offline.exe"
        } else {
            "sherpa-onnx-offline"
        },
    )
    .is_some();
    if have_sherpa {
        say!("sherpa-onnx binaries: present, skipping");
    } else {
        let asset = sherpa_asset()?;
        let url = format!(
            "{BASE}/{SHERPA_VER}/sherpa-onnx-{SHERPA_VER}-{asset}.tar.bz2"
        );
        let arc = tmp.join("sherpa.tar.bz2");
        fetch(&url, &arc, "sherpa-onnx binaries")?;
        untar(&arc, models, "sherpa-onnx")?;
        let _ = std::fs::remove_file(&arc);
    }

    for (dir, name, label) in [
        ("sherpa-onnx-spleeter-2stems-int8",
         "source-separation-models/\
          sherpa-onnx-spleeter-2stems-int8.tar.bz2",
         "spleeter separation models"),
        ("sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8",
         "asr-models/\
          sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8.tar.bz2",
         "parakeet ASR model (~490 MB)"),
    ] {
        if models.join(dir).is_dir() {
            say!("{label}: present, skipping");
            continue;
        }
        let arc = tmp.join(format!("{dir}.tar.bz2"));
        fetch(&format!("{BASE}/{name}"), &arc, label)?;
        untar(&arc, models, label)?;
        let _ = std::fs::remove_file(&arc);
    }

    if models.join("rmvpe.onnx").is_file() {
        say!("rmvpe.onnx: present, skipping");
    } else {
        fetch(RMVPE_URL, &models.join("rmvpe.onnx"),
              "rmvpe pitch model (~345 MB)")?;
    }

    let _ = std::fs::remove_dir(&tmp);
    say!("models ready under {}", models.display());
    Ok(())
}
