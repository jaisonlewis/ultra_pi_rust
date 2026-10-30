//! Model + binary acquisition — `ups-prep models` downloads the
//! external pieces the ML stages need into the models dir:
//!   sherpa-onnx binaries (platform tarball from k2-fsa releases)
//!   spleeter int8 separation models
//!   parakeet-tdt-0.6b-v3 int8 ASR model  (English)
//!   whisper multilingual ASR models      (language packs)
//!   rmvpe.onnx pitch model (served from our own release)
//!   per-script Noto fonts                (language packs)
//!
//! Language packs are selectable: the GUI persists the chosen set to
//! `<models>/langs.json`; `download` with `packs=None` fetches the
//! default set (whisper-tiny + all fonts — the "from the get go"
//! bundle).

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
const FONT_BASE: &str =
    "https://raw.githubusercontent.com/google/fonts/main/ofl";

/// Selectable multilingual ASR packs — (id, model dir, tarball leaf,
/// label). Language-specific engines beat whisper on their turf:
/// canary owns es/de/fr (best sung text), sense-voice owns
/// zh/ja/ko/yue, omnilingual covers ~1600 languages incl. all of
/// South Asia. Whisper stays for language detection + fallback;
/// bigger sizes trade download weight for accuracy.
pub const ASR_PACKS: &[(&str, &str, &str, &str)] = &[
    ("whisper-tiny",
     "sherpa-onnx-whisper-tiny",
     "sherpa-onnx-whisper-tiny.tar.bz2",
     "whisper-tiny (~75 MB) — language detection + fallback"),
    ("whisper-base",
     "sherpa-onnx-whisper-base",
     "sherpa-onnx-whisper-base.tar.bz2",
     "whisper-base (~145 MB) — better fallback accuracy"),
    ("whisper-small",
     "sherpa-onnx-whisper-small",
     "sherpa-onnx-whisper-small.tar.bz2",
     "whisper-small (~460 MB) — best multilingual whisper"),
    ("canary",
     "sherpa-onnx-nemo-canary-180m-flash-en-es-de-fr-int8",
     "sherpa-onnx-nemo-canary-180m-flash-en-es-de-fr-int8.tar.bz2",
     "canary (~180 MB) — Spanish/German/French, best on singing"),
    ("sensevoice",
     "sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2025-09-09",
     "sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2025-09-09.tar.bz2",
     "sense-voice (~230 MB) — Chinese/Japanese/Korean/Cantonese"),
    ("omnilingual",
     "sherpa-onnx-omnilingual-asr-1600-languages-300M-ctc-v2-int8-\
      2026-02-05",
     "sherpa-onnx-omnilingual-asr-1600-languages-300M-ctc-v2-int8-\
      2026-02-05.tar.bz2",
     "omnilingual (~300 MB) — Hindi/Tamil/Thai/everything else"),
];

/// Per-script fonts — (script tag, ofl/<family>, primary file,
/// fallback file). `[wght]` variable fonts are tried first, the
/// `-Regular` static next; whichever lands is saved under the clean
/// static name into `<models>/fonts/` (and the app fonts dir when
/// discoverable — the renderer scans both).
pub const FONT_PACKS: &[(&str, &str, &str, &str)] = &[
    ("deva", "notosansdevanagari",
     "NotoSansDevanagari[wght].ttf", "NotoSansDevanagari-Regular.ttf"),
    ("beng", "notosansbengali",
     "NotoSansBengali[wght].ttf",    "NotoSansBengali-Regular.ttf"),
    ("guru", "notosansgurmukhi",
     "NotoSansGurmukhi[wght].ttf",   "NotoSansGurmukhi-Regular.ttf"),
    ("gujr", "notosansgujarati",
     "NotoSansGujarati[wght].ttf",   "NotoSansGujarati-Regular.ttf"),
    ("orya", "notosansoriya",
     "NotoSansOriya[wght].ttf",      "NotoSansOriya-Regular.ttf"),
    ("taml", "notosanstamil",
     "NotoSansTamil[wght].ttf",      "NotoSansTamil-Regular.ttf"),
    ("telu", "notosanstelugu",
     "NotoSansTelugu[wght].ttf",     "NotoSansTelugu-Regular.ttf"),
    ("knda", "notosanskannada",
     "NotoSansKannada[wght].ttf",    "NotoSansKannada-Regular.ttf"),
    ("mlym", "notosansmalayalam",
     "NotoSansMalayalam[wght].ttf",  "NotoSansMalayalam-Regular.ttf"),
    ("sinh", "notosanssinhala",
     "NotoSansSinhala[wght].ttf",    "NotoSansSinhala-Regular.ttf"),
    ("thai", "notosansthai",
     "NotoSansThai[wght].ttf",       "NotoSansThai-Regular.ttf"),
    ("lao",  "notosanslao",
     "NotoSansLao[wght].ttf",        "NotoSansLao-Regular.ttf"),
    ("khmr", "notosanskhmer",
     "NotoSansKhmer[wght].ttf",      "NotoSansKhmer-Regular.ttf"),
    ("mymr", "notosansmyanmar",
     "NotoSansMyanmar[wght].ttf",    "NotoSansMyanmar-Regular.ttf"),
    ("tibt", "notosanstibetan",
     "NotoSansTibetan[wght].ttf",    "NotoSansTibetan-Regular.ttf"),
    ("arab", "notosansarabic",
     "NotoSansArabic[wght].ttf",     "NotoSansArabic-Regular.ttf"),
    ("hebr", "notosanshebrew",
     "NotoSansHebrew[wght].ttf",     "NotoSansHebrew-Regular.ttf"),
    ("armn", "notosansarmenian",
     "NotoSansArmenian[wght].ttf",   "NotoSansArmenian-Regular.ttf"),
    ("geor", "notosansgeorgian",
     "NotoSansGeorgian[wght].ttf",   "NotoSansGeorgian-Regular.ttf"),
    ("ethi", "notosansethiopic",
     "NotoSansEthiopic[wght].ttf",   "NotoSansEthiopic-Regular.ttf"),
];

/// Font filename the renderer will find for `script` (wght marker
/// stripped), when a pack exists for it.
pub fn font_for_script(script: &str) -> Option<String> {
    FONT_PACKS.iter().find(|(s, ..)| *s == script)
        .map(|(.., f, _)| f.replace("[wght]", ""))
}

/// Font already available — models/fonts or a discoverable app fonts
/// dir (source checkout / exe-adjacent).
pub fn font_installed(models: &Path, script: &str) -> bool {
    let Some(name) = font_for_script(script) else { return false };
    if models.join("fonts").join(&name).exists() {
        return true;
    }
    font_dirs(models).iter().any(|d| d.join(&name).exists())
}

/// Every dir fonts can live in: <models>/fonts, the repo fonts/ next
/// to a dev binary, fonts/ next to the exe, $UPS_FONTS_DIR.
pub fn font_dirs(models: &Path) -> Vec<PathBuf> {
    let mut v = vec![models.join("fonts")];
    if let Ok(e) = std::env::var("UPS_FONTS_DIR") {
        if !e.is_empty() {
            v.push(PathBuf::from(e));
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(bin) = exe.parent() {
            v.push(bin.join("fonts"));
            // dev layout: target/release/<exe> → <repo>/fonts
            v.push(bin.join("../../../fonts"));
        }
    }
    v
}

/// Persisted pack selection — `<models>/langs.json`. Defaults when
/// absent: whisper-tiny + every font pack.
pub fn load_packs(models: &Path)
        -> std::collections::HashSet<String> {
    let defaults = || -> std::collections::HashSet<String> {
        ASR_PACKS.iter().take(1).map(|(id, ..)| id.to_string())
            .chain(FONT_PACKS.iter()
                .map(|(s, ..)| format!("font:{s}")))
            .collect()
    };
    let Ok(bytes) = std::fs::read(models.join("langs.json")) else {
        return defaults();
    };
    let Ok(j) = serde_json::from_slice::<serde_json::Value>(&bytes)
        else { return defaults() };
    j["packs"].as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string))
            .collect())
        .unwrap_or_else(defaults)
}

/// Save the pack selection the GUI checkboxes produce.
#[allow(dead_code)] // GUI-only caller (feature-gated)
pub fn save_packs(models: &Path,
                  packs: &std::collections::HashSet<String>)
        -> Result<()> {
    std::fs::create_dir_all(models)?;
    let j = serde_json::json!({
        "packs": packs.iter().collect::<Vec<_>>(),
    });
    std::fs::write(models.join("langs.json"),
                   serde_json::to_string_pretty(&j)?)?;
    Ok(())
}

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
/// `models`. `packs=None` uses the persisted/default selection
/// (langs.json, else whisper-tiny + all fonts). Idempotent —
/// existing dirs/files are skipped.
pub fn download(models: &Path,
                packs: Option<&std::collections::HashSet<String>>)
        -> Result<()> {
    let owned;
    let packs = match packs {
        Some(p) => p,
        None => { owned = load_packs(models); &owned }
    };
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

    // ---- language packs ----------------------------------------------
    for (id, dir, leaf, label) in ASR_PACKS {
        if !packs.contains(*id) {
            continue;
        }
        if models.join(dir).is_dir() {
            say!("{label}: present, skipping");
            continue;
        }
        let arc = tmp.join(leaf);
        fetch(&format!("{BASE}/asr-models/{leaf}"), &arc, label)?;
        untar(&arc, models, label)?;
        let _ = std::fs::remove_file(&arc);
    }

    // script fonts → <models>/fonts (+ the app fonts dir when this is
    // a dev checkout, so the native renderer sees them immediately)
    let fdir = models.join("fonts");
    for (script, fam, wght, regular) in FONT_PACKS {
        if !packs.contains(&format!("font:{script}")) {
            continue;
        }
        let clean = wght.replace("[wght]", "");
        let dest = fdir.join(&clean);
        if dest.exists() {
            continue;
        }
        std::fs::create_dir_all(&fdir)?;
        let mut ok = false;
        for file in [wght, regular] {
            let url = format!("{FONT_BASE}/{fam}/{}",
                              file.replace('[', "%5B").replace(']', "%5D"));
            match fetch(&url, &dest, &format!("{script} font")) {
                Ok(()) => { ok = true; break; }
                Err(e) => {
                    say!("  {script} font: {e} (trying next variant)");
                }
            }
        }
        if !ok {
            continue;
        }
        for d in font_dirs(models) {
            let alt = d.join(&clean);
            if d == fdir || alt.exists() {
                continue;
            }
            if d.is_dir() {
                let _ = std::fs::copy(&dest, &alt);
            }
        }
    }

    let _ = std::fs::remove_dir(&tmp);
    say!("models ready under {}", models.display());
    Ok(())
}
