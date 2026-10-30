//! Romanized alternates for non-Latin lyric text — port of
//! `ultrapistar/romanize.py` (see NOTICE). Fills `Note.alt` so players
//! can offer native / romanized per song.
//!
//! Built-in tables cover kana→hepburn and Cyrillic. Kanji readings and
//! hangul need real dictionaries — those go through an optional Python
//! helper (pykakasi + hangul-romanize, same deps as the Python prep)
//! batched once per chart. Without it, kanji/hangul notes keep the
//! native text as their alt source (rendered via CJK font fallback)
//! while kana/cyrillic notes still romanize.

use crate::chart::Chart;
use std::process::{Command, Stdio};

fn is_hira(c: char) -> bool { ('\u{3040}'..='\u{309f}').contains(&c) }
fn is_kata(c: char) -> bool {
    ('\u{30a0}'..='\u{30ff}').contains(&c) || ('\u{ff66}'..='\u{ff9f}').contains(&c)
}
fn is_kanji(c: char) -> bool { ('\u{4e00}'..='\u{9fff}').contains(&c) }
fn is_hangul(c: char) -> bool { ('\u{ac00}'..='\u{d7af}').contains(&c) }
fn is_cyr(c: char) -> bool { ('\u{0400}'..='\u{04ff}').contains(&c) }
fn is_deva(c: char) -> bool { ('\u{0900}'..='\u{097f}').contains(&c) }
fn is_beng(c: char) -> bool { ('\u{0980}'..='\u{09ff}').contains(&c) }
fn is_guru(c: char) -> bool { ('\u{0a00}'..='\u{0a7f}').contains(&c) }
fn is_gujr(c: char) -> bool { ('\u{0a80}'..='\u{0aff}').contains(&c) }
fn is_orya(c: char) -> bool { ('\u{0b00}'..='\u{0b7f}').contains(&c) }
fn is_taml(c: char) -> bool { ('\u{0b80}'..='\u{0bff}').contains(&c) }
fn is_telu(c: char) -> bool { ('\u{0c00}'..='\u{0c7f}').contains(&c) }
fn is_knda(c: char) -> bool { ('\u{0c80}'..='\u{0cff}').contains(&c) }
fn is_mlym(c: char) -> bool { ('\u{0d00}'..='\u{0d7f}').contains(&c) }
fn is_sinh(c: char) -> bool { ('\u{0d80}'..='\u{0dff}').contains(&c) }
fn is_thai(c: char) -> bool { ('\u{0e00}'..='\u{0e7f}').contains(&c) }
fn is_lao(c: char)  -> bool { ('\u{0e80}'..='\u{0eff}').contains(&c) }
fn is_tibt(c: char) -> bool { ('\u{0f00}'..='\u{0fff}').contains(&c) }
fn is_mymr(c: char) -> bool { ('\u{1000}'..='\u{109f}').contains(&c) }
fn is_geor(c: char) -> bool { ('\u{10a0}'..='\u{10ff}').contains(&c) }
fn is_ethi(c: char) -> bool { ('\u{1200}'..='\u{137f}').contains(&c) }
fn is_khmr(c: char) -> bool { ('\u{1780}'..='\u{17ff}').contains(&c) }
fn is_armn(c: char) -> bool { ('\u{0530}'..='\u{058f}').contains(&c) }
fn is_hebr(c: char) -> bool { ('\u{0590}'..='\u{05ff}').contains(&c) }
fn is_arab(c: char) -> bool {
    ('\u{0600}'..='\u{06ff}').contains(&c)
        || ('\u{0750}'..='\u{077f}').contains(&c)
        || ('\u{08a0}'..='\u{08ff}').contains(&c)
        || ('\u{fb50}'..='\u{fdff}').contains(&c)
        || ('\u{fe70}'..='\u{feff}').contains(&c)
}
fn is_grek(c: char) -> bool {
    ('\u{0370}'..='\u{03ff}').contains(&c)
        || ('\u{1f00}'..='\u{1fff}').contains(&c)
}
fn nonlatin(c: char) -> bool {
    is_hira(c) || is_kata(c) || is_hangul(c) || is_cyr(c) || is_kanji(c)
        || is_deva(c) || is_beng(c) || is_guru(c) || is_gujr(c)
        || is_orya(c) || is_taml(c) || is_telu(c) || is_knda(c)
        || is_mlym(c) || is_sinh(c) || is_thai(c) || is_lao(c)
        || is_tibt(c) || is_mymr(c) || is_geor(c) || is_ethi(c)
        || is_khmr(c) || is_armn(c) || is_hebr(c) || is_arab(c)
        || is_grek(c)
}

/// First non-Latin script found in `text` — for per-run answers call
/// it on a single-script run; `scripts_in` collects them all.
pub fn script_of(text: &str) -> &'static str {
    let chars = || text.chars();
    if chars().any(|c| is_hira(c) || is_kata(c)) {
        return "jp";
    }
    if chars().any(is_hangul) { return "kr"; }
    if chars().any(is_cyr)    { return "cyr"; }
    if chars().any(is_deva)   { return "deva"; }
    if chars().any(is_beng)   { return "beng"; }
    if chars().any(is_guru)   { return "guru"; }
    if chars().any(is_gujr)   { return "gujr"; }
    if chars().any(is_orya)   { return "orya"; }
    if chars().any(is_taml)   { return "taml"; }
    if chars().any(is_telu)   { return "telu"; }
    if chars().any(is_knda)   { return "knda"; }
    if chars().any(is_mlym)   { return "mlym"; }
    if chars().any(is_sinh)   { return "sinh"; }
    if chars().any(is_thai)   { return "thai"; }
    if chars().any(is_lao)    { return "lao"; }
    if chars().any(is_tibt)   { return "tibt"; }
    if chars().any(is_mymr)   { return "mymr"; }
    if chars().any(is_geor)   { return "geor"; }
    if chars().any(is_ethi)   { return "ethi"; }
    if chars().any(is_khmr)   { return "khmr"; }
    if chars().any(is_armn)   { return "armn"; }
    if chars().any(is_hebr)   { return "hebr"; }
    if chars().any(is_arab)   { return "arab"; }
    if chars().any(is_grek)   { return "grek"; }
    if chars().any(is_kanji)  { return "zh"; }
    "latin"
}

/// Every distinct non-Latin script present — for coverage alerts.
pub fn scripts_in(text: &str) -> Vec<&'static str> {
    const FNS: &[(&str, fn(char) -> bool)] = &[
        ("jp",  |c| is_hira(c) || is_kata(c)),
        ("kr",  is_hangul), ("cyr", is_cyr),   ("deva", is_deva),
        ("beng", is_beng), ("guru", is_guru),  ("gujr", is_gujr),
        ("orya", is_orya), ("taml", is_taml),  ("telu", is_telu),
        ("knda", is_knda), ("mlym", is_mlym),  ("sinh", is_sinh),
        ("thai", is_thai), ("lao", is_lao),    ("tibt", is_tibt),
        ("mymr", is_mymr), ("geor", is_geor),  ("ethi", is_ethi),
        ("khmr", is_khmr), ("armn", is_armn),  ("hebr", is_hebr),
        ("arab", is_arab), ("grek", is_grek),  ("zh", is_kanji),
    ];
    FNS.iter()
        .filter(|(_, f)| text.chars().any(|c| f(c)))
        .map(|(tag, _)| *tag)
        .collect()
}

/// Does the built-in stack produce romanized alternates for `script`?
/// (jp/kr kanji+hangul need the optional python helper for full
/// coverage but still romanize their phonetic runs without it.)
pub fn romanizable(script: &str) -> bool {
    matches!(script,
        "jp" | "kr" | "cyr"
        | "deva" | "beng" | "guru" | "gujr" | "taml" | "telu"
        | "knda" | "mlym" | "arab" | "hebr" | "grek" | "armn"
        | "geor" | "thai")
}

/// Human-readable name for alerts.
pub fn script_name(script: &str) -> &'static str {
    match script {
        "jp" => "Japanese", "kr" => "Korean", "cyr" => "Cyrillic",
        "zh" => "Chinese", "deva" => "Devanagari (Hindi/Marathi)",
        "beng" => "Bengali", "guru" => "Gurmukhi (Punjabi)",
        "gujr" => "Gujarati", "orya" => "Odia", "taml" => "Tamil",
        "telu" => "Telugu", "knda" => "Kannada", "mlym" => "Malayalam",
        "sinh" => "Sinhala", "thai" => "Thai", "lao" => "Lao",
        "tibt" => "Tibetan", "mymr" => "Myanmar",
        "geor" => "Georgian", "ethi" => "Ethiopic",
        "khmr" => "Khmer", "armn" => "Armenian",
        "hebr" => "Hebrew", "arab" => "Arabic/Persian/Urdu",
        "grek" => "Greek", _ => "unknown",
    }
}

const CYR: [(&str, &str); 33] = [
    ("а", "a"), ("б", "b"), ("в", "v"), ("г", "g"), ("д", "d"),
    ("е", "e"), ("ё", "yo"), ("ж", "zh"), ("з", "z"), ("и", "i"),
    ("й", "y"), ("к", "k"), ("л", "l"), ("м", "m"), ("н", "n"),
    ("о", "o"), ("п", "p"), ("р", "r"), ("с", "s"), ("т", "t"),
    ("у", "u"), ("ф", "f"), ("х", "kh"), ("ц", "ts"), ("ч", "ch"),
    ("ш", "sh"), ("щ", "shch"), ("ъ", "'"), ("ы", "y"), ("ь", "'"),
    ("э", "e"), ("ю", "yu"), ("я", "ya"),
];

fn cyr(s: &str) -> String {
    s.chars()
        .map(|c| {
            let lo = c.to_lowercase().to_string();
            CYR.iter()
                .find(|(k, _)| *k == lo)
                .map(|(_, v)| {
                    if c.is_uppercase() {
                        v.to_uppercase()
                    } else {
                        v.to_string()
                    }
                })
                .unwrap_or_else(|| c.to_string())
        })
        .collect()
}

// rows: (kana, hepburn). Hiragana and katakana share sound values —
// the lookup normalizes katakana to hiragana before matching.
const KANA: &[(&str, &str)] = &[
    ("あ", "a"), ("い", "i"), ("う", "u"), ("え", "e"), ("お", "o"),
    ("か", "ka"), ("き", "ki"), ("く", "ku"), ("け", "ke"), ("こ", "ko"),
    ("が", "ga"), ("ぎ", "gi"), ("ぐ", "gu"), ("げ", "ge"), ("ご", "go"),
    ("さ", "sa"), ("し", "shi"), ("す", "su"), ("せ", "se"), ("そ", "so"),
    ("ざ", "za"), ("じ", "ji"), ("ず", "zu"), ("ぜ", "ze"), ("ぞ", "zo"),
    ("た", "ta"), ("ち", "chi"), ("つ", "tsu"), ("て", "te"), ("と", "to"),
    ("だ", "da"), ("ぢ", "ji"), ("づ", "zu"), ("で", "de"), ("ど", "do"),
    ("な", "na"), ("に", "ni"), ("ぬ", "nu"), ("ね", "ne"), ("の", "no"),
    ("は", "ha"), ("ひ", "hi"), ("ふ", "fu"), ("へ", "he"), ("ほ", "ho"),
    ("ば", "ba"), ("び", "bi"), ("ぶ", "bu"), ("べ", "be"), ("ぼ", "bo"),
    ("ぱ", "pa"), ("ぴ", "pi"), ("ぷ", "pu"), ("ぺ", "pe"), ("ぽ", "po"),
    ("ま", "ma"), ("み", "mi"), ("む", "mu"), ("め", "me"), ("も", "mo"),
    ("や", "ya"), ("ゆ", "yu"), ("よ", "yo"),
    ("ら", "ra"), ("り", "ri"), ("る", "ru"), ("れ", "re"), ("ろ", "ro"),
    ("わ", "wa"), ("ゐ", "wi"), ("ゑ", "we"), ("を", "o"), ("ん", "n"),
];

/// yōon digraphs — consonant + small ya/yu/yo.
const DIGRAPH: &[(&str, &str)] = &[
    ("きゃ", "kya"), ("きゅ", "kyu"), ("きょ", "kyo"),
    ("ぎゃ", "gya"), ("ぎゅ", "gyu"), ("ぎょ", "gyo"),
    ("しゃ", "sha"), ("しゅ", "shu"), ("しょ", "sho"),
    ("じゃ", "ja"), ("じゅ", "ju"), ("じょ", "jo"),
    ("ちゃ", "cha"), ("ちゅ", "chu"), ("ちょ", "cho"),
    ("ぢゃ", "ja"), ("ぢゅ", "ju"), ("ぢょ", "jo"),
    ("にゃ", "nya"), ("にゅ", "nyu"), ("にょ", "nyo"),
    ("ひゃ", "hya"), ("ひゅ", "hyu"), ("ひょ", "hyo"),
    ("びゃ", "bya"), ("びゅ", "byu"), ("びょ", "byo"),
    ("ぴゃ", "pya"), ("ぴゅ", "pyu"), ("ぴょ", "pyo"),
    ("みゃ", "mya"), ("みゅ", "myu"), ("みょ", "myo"),
    ("りゃ", "rya"), ("りゅ", "ryu"), ("りょ", "ryo"),
];

fn to_hira(c: char) -> char {
    if ('\u{30a1}'..='\u{30f6}').contains(&c) {
        char::from_u32(c as u32 - 0x60).unwrap_or(c)
    } else {
        c
    }
}

fn kana_val(hira_pair: &str) -> Option<&'static str> {
    if hira_pair.chars().count() == 2 {
        if let Some((_, v)) = DIGRAPH.iter().find(|(k, _)| *k == hira_pair) {
            return Some(v);
        }
    }
    if hira_pair.chars().count() == 1 {
        if let Some((_, v)) = KANA.iter().find(|(k, _)| *k == hira_pair) {
            return Some(v);
        }
    }
    None
}

/// Hepburn for a hiragana/katakana run — handles sokuon (っ),
/// chōonpu (ー) and yōon digraphs. Returns None when any non-kana
/// character shows up (kanji runs need a dictionary).
fn kana_to_hepburn(s: &str) -> Option<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    let mut geminate = false;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() || c.is_ascii_punctuation() {
            out.push(c);
            i += 1;
            continue;
        }
        if c == 'っ' || c == 'ッ' {
            geminate = true;
            i += 1;
            continue;
        }
        if c == 'ー' {
            if let Some(v) = out.chars().last().filter(|v| "aeiou".contains(*v)) {
                out.push(v);
            }
            i += 1;
            continue;
        }
        let mut cur = String::new();
        cur.push(to_hira(c));
        if i + 1 < chars.len() {
            let nxt = chars[i + 1];
            if matches!(nxt, 'ゃ' | 'ゅ' | 'ょ' | 'ャ' | 'ュ' | 'ョ'
                              | 'ぁ' | 'ァ') {
                cur.push(to_hira(nxt));
                // ぁ after し → she etc. falls through to single below
                i += 1;
            }
        }
        match kana_val(&cur) {
            Some(v) => {
                if geminate {
                    if let Some(first) = v.chars().next() {
                        if first.is_ascii_alphabetic() && !"aeioun".contains(first) {
                            out.push(first);
                        }
                    }
                }
                out.push_str(v);
            }
            None => {
                // lone small kana or unknown → try the first char alone
                if let Some(v) = kana_val(&cur[..1]) {
                    out.push_str(v);
                    if cur.chars().count() == 2 {
                        i -= 1; // reprocess the small kana on its own
                    }
                } else {
                    return None; // kanji or non-kana — bail out
                }
            }
        }
        geminate = false;
        i += 1;
    }
    Some(out)
}

/// Optional Python helper — mirrors the Python pipeline's converters
/// (pykakasi for jp incl. kanji, hangul-romanize for kr). One call per
/// chart: JSON {jp: [...], kr: [...]} on stdin → {jp: {t: rom}, ...}
const PY_HELPER: &str = r#"
import json, sys
req = json.load(sys.stdin)
out = {"jp": {}, "kr": {}}
try:
    import pykakasi
    k = pykakasi.kakasi()
    for t in req.get("jp", []):
        out["jp"][t] = " ".join(i["hepburn"] for i in k.convert(t) if i["hepburn"])
except Exception:
    pass
try:
    from hangul_romanize import Transliter
    from hangul_romanize.rule import academic
    tr = Transliter(academic)
    for t in req.get("kr", []):
        out["kr"][t] = tr.translit(t)
except Exception:
    pass
print(json.dumps(out))
"#;

fn py_romanize(texts: &[&str], kind: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    let probe = Command::new("python")
        .arg("-X").arg("utf8").arg("-c")
        .arg("import pykakasi, hangul_romanize")
        .stdout(Stdio::null()).stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !probe {
        return map;
    }
    let payload = serde_json::json!({ kind: texts }).to_string();
    let mut child = match Command::new("python")
        .arg("-X").arg("utf8").arg("-c").arg(PY_HELPER)
        .stdin(Stdio::piped()).stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return map,
    };
    use std::io::Write;
    if let Some(mut si) = child.stdin.take() {
        let _ = si.write_all(payload.as_bytes());
    }
    if let Ok(o) = child.wait_with_output() {
        if let Ok(d) = serde_json::from_slice::<serde_json::Value>(&o.stdout) {
            if let Some(m) = d.get(kind).and_then(|v| v.as_object()) {
                for (t, r) in m {
                    if let Some(s) = r.as_str() {
                        if !s.is_empty() {
                            map.insert(t.clone(), s.to_string());
                        }
                    }
                }
            }
        }
    }
    map
}

/// Split a lyric chunk into (run, is_latin) script runs — latin and
/// non-latin characters never share a run.
fn script_runs(text: &str) -> Vec<(String, bool)> {
    let mut runs: Vec<(String, bool)> = Vec::new();
    for ch in text.chars() {
        let latin = !nonlatin(ch);
        if let Some(last) = runs.last_mut() {
            if last.1 == latin {
                last.0.push(ch);
                continue;
            }
        }
        runs.push((ch.to_string(), latin));
    }
    runs
}

/// Romanized rendering of a lyric chunk — per-script run like the
/// Python version; kanji inherits the document `hint` (jp/kr) via the
/// caller-batched `py_map`.
fn romanize_text(
    text: &str,
    hint: &str,
    py_map: &std::collections::HashMap<String, String>,
) -> String {
    if !text.chars().any(nonlatin) {
        return String::new();
    }
    let mut doc = script_of(text);
    if doc == "latin" || doc == "zh" {
        doc = if hint.is_empty() { doc } else { hint };
    }
    script_runs(text)
        .iter()
        .map(|(run, latin)| {
            if *latin {
                return run.clone();
            }
            // per-run script — mixed-script docs transliterate each
            // run with its own table; ambiguous kanji runs defer to
            // the document hint (jp vs zh)
            let rs = script_of(run);
            let kind = if rs == "latin" || rs == "zh" { doc } else { rs };
            romanize_run(run, kind, py_map)
        })
        .collect::<Vec<_>>()
        .iter()
        .map(|x| x.trim())
        .filter(|x| !x.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn romanize_run(
    run: &str,
    kind: &str,
    py_map: &std::collections::HashMap<String, String>,
) -> String {
    match kind {
        "jp" => {
            if let Some(v) = py_map.get(run) {
                return v.clone();
            }
            // built-in: pure-kana runs still romanize w/o a dictionary
            if run.chars().any(is_kanji) {
                run.to_string()
            } else {
                kana_to_hepburn(run).unwrap_or_else(|| run.to_string())
            }
        }
        "kr" => py_map.get(run).cloned().unwrap_or_else(|| run.to_string()),
        "cyr" => cyr(run),
        "deva" => abugida(run, &DEVA),
        "beng" => abugida(run, &BENG),
        "guru" => abugida(run, &GURU),
        "gujr" => abugida(run, &GUJR),
        "taml" => abugida(run, &TAML),
        "telu" => abugida(run, &TELU),
        "knda" => abugida(run, &KNDA),
        "mlym" => abugida(run, &MLYM),
        "arab" => map_chars(run, ARAB),
        "hebr" => map_chars(run, HEBR),
        "grek" => map_chars(run, GREK),
        "armn" => map_chars(run, ARMN),
        "geor" => map_chars(run, GEOR),
        "thai" => map_chars(run, THAI),
        _ => run.to_string(),
    }
}

// ---- abugida engine (Indic family) ---------------------------------
// Consonant letters carry an inherent vowel; a following dependent
// vowel sign (matra) replaces it, a following virama kills it.

struct Abugida {
    /// inherent vowel of consonant letters ("a" for Devanagari, "o" for Bengali)
    inherent: &'static str,
    virama: char,
    cons: &'static [(char, &'static str)],
    /// consonants that change sound with a following nukta — keyed by
    /// the BASE char (nukta is a combining mark, so 'क़' can't be a
    /// single table key)
    nukta: &'static [(char, &'static str)],
    indep: &'static [(char, &'static str)],
    matra: &'static [(char, &'static str)],
    marks: &'static [(char, &'static str)],
}

fn is_nukta(c: char) -> bool {
    matches!(c, '\u{093c}' | '\u{09bc}' | '\u{0a3c}' | '\u{0abc}'
                | '\u{0b3c}' | '\u{0cbc}')
}

fn abugida(s: &str, t: &Abugida) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if let Some((_, v)) = t.cons.iter().find(|(k, _)| *k == c) {
            let mut j = i + 1;
            let mut val = *v;
            if chars.get(j).is_some_and(|n| is_nukta(*n)) {
                if let Some((_, nv)) = t.nukta.iter()
                        .find(|(k, _)| *k == c) {
                    val = nv;
                }
                j += 1;
            }
            out.push_str(val);
            match chars.get(j) {
                Some(&n) if n == t.virama => j += 1,
                Some(&n) if t.matra.iter().any(|(k, _)| *k == n) => {}
                // word-final schwa deletion — Hindi "तुम" reads
                // "tum" not "tuma"; the inherent vowel is silent at
                // word end
                Some(&n) if !n.is_whitespace()
                        && !n.is_ascii_punctuation() => {
                    out.push_str(t.inherent)
                }
                _ => {}
            }
            i = j;
            continue;
        }
        if let Some((_, v)) = t.matra.iter().find(|(k, _)| *k == c) {
            out.push_str(v);
        } else if let Some((_, v)) = t.indep.iter().find(|(k, _)| *k == c) {
            out.push_str(v);
        } else if let Some((_, v)) = t.marks.iter().find(|(k, _)| *k == c) {
            out.push_str(v);
        } else if c == t.virama || ('\u{200c}'..='\u{200d}').contains(&c) {
            // bare virama / ZWNJ / ZWJ — silent
        } else {
            out.push(c);
        }
        i += 1;
    }
    out
}

/// Plain char→string transliteration for non-abugida scripts.
fn map_chars(s: &str, t: &[(char, &'static str)]) -> String {
    s.chars().map(|c| {
        let lo = c.to_lowercase().next().unwrap_or(c);
        t.iter().find(|(k, _)| *k == lo)
            .map(|(_, v)| if c.is_uppercase() {
                v.to_uppercase()
            } else {
                v.to_string()
            })
            .unwrap_or_else(|| c.to_string())
    }).collect()
}

// Devanagari — Hindi, Marathi, Nepali. Singable ASCII romanization
// (ā→aa, ī→ee, retroflex unmarked — this is for karaoke, not linguistics).
const DEVA: Abugida = Abugida {
    inherent: "a",
    virama: '\u{094d}',
    cons: &[
        ('क', "k"), ('ख', "kh"), ('ग', "g"), ('घ', "gh"), ('ङ', "n"),
        ('च', "ch"), ('छ', "chh"), ('ज', "j"), ('झ', "jh"), ('ञ', "n"),
        ('ट', "t"), ('ठ', "th"), ('ड', "d"), ('ढ', "dh"), ('ण', "n"),
        ('त', "t"), ('थ', "th"), ('द', "d"), ('ध', "dh"), ('न', "n"),
        ('प', "p"), ('फ', "ph"), ('ब', "b"), ('भ', "bh"), ('म', "m"),
        ('य', "y"), ('र', "r"), ('ल', "l"), ('व', "v"),
        ('श', "sh"), ('ष', "sh"), ('स', "s"), ('ह', "h"),
        ('ळ', "l"), ('ऩ', "n"), ('ऱ', "r"), ('ऴ', "l"),
    ],
    nukta: &[
        ('क', "q"), ('ख', "kh"), ('ग', "gh"), ('ज', "z"),
        ('झ', "zh"), ('ड', "r"), ('ढ', "rh"), ('फ', "f"),
    ],
    indep: &[
        ('अ', "a"), ('आ', "aa"), ('इ', "i"), ('ई', "ee"),
        ('उ', "u"), ('ऊ', "oo"), ('ऋ', "ri"), ('ॠ', "ri"),
        ('ऌ', "lri"), ('ए', "e"), ('ऐ', "ai"), ('ओ', "o"),
        ('औ', "au"), ('ऍ', "e"), ('ऑ', "o"), ('ऎ', "e"), ('ऒ', "o"),
    ],
    matra: &[
        ('\u{093e}', "aa"), ('\u{093f}', "i"), ('\u{0940}', "ee"),
        ('\u{0941}', "u"), ('\u{0942}', "oo"), ('\u{0943}', "ri"),
        ('\u{0944}', "ri"), ('\u{0945}', "e"), ('\u{0946}', "e"),
        ('\u{0947}', "e"), ('\u{0948}', "ai"), ('\u{0949}', "o"),
        ('\u{094a}', "o"), ('\u{094b}', "o"), ('\u{094c}', "au"),
        ('\u{094f}', "o"), ('\u{0955}', "o"), ('\u{0956}', "ue"),
        ('\u{0957}', "ue"),
    ],
    marks: &[
        ('\u{0902}', "n"), ('\u{0901}', "n"), ('\u{0903}', "h"),
        ('\u{093d}', "'"), ('ऀ', ""), ('ऽ', "'"), ('ॐ', "om"),
        ('०', "0"), ('१', "1"), ('२', "2"), ('३', "3"), ('४', "4"),
        ('५', "5"), ('६', "6"), ('७', "7"), ('८', "8"), ('९', "9"),
        ('।', "."), ('॥', "."),
    ],
};

// Bengali — inherent vowel is "o".
const BENG: Abugida = Abugida {
    inherent: "o",
    virama: '\u{09cd}',
    cons: &[
        ('ক', "k"), ('খ', "kh"), ('গ', "g"), ('ঘ', "gh"), ('ঙ', "ng"),
        ('চ', "ch"), ('ছ', "chh"), ('জ', "j"), ('ঝ', "jh"), ('ঞ', "ny"),
        ('ট', "t"), ('ঠ', "th"), ('ড', "d"), ('ঢ', "dh"), ('ণ', "n"),
        ('ত', "t"), ('থ', "th"), ('দ', "d"), ('ধ', "dh"), ('ন', "n"),
        ('প', "p"), ('ফ', "ph"), ('ব', "b"), ('ভ', "bh"), ('ম', "m"),
        ('য', "j"), ('র', "r"), ('ল', "l"), ('শ', "sh"), ('ষ', "sh"),
        ('স', "s"), ('হ', "h"), ('ৰ', "r"), ('ৱ', "w"),
    ],
    nukta: &[('ড', "r"), ('ঢ', "rh"), ('য', "y")],
    indep: &[
        ('অ', "o"), ('আ', "a"), ('ই', "i"), ('ঈ', "ee"),
        ('উ', "u"), ('ঊ', "oo"), ('ঋ', "ri"), ('এ', "e"),
        ('ঐ', "oi"), ('ও', "o"), ('ঔ', "ou"),
    ],
    matra: &[
        ('\u{09be}', "a"), ('\u{09bf}', "i"), ('\u{09c0}', "ee"),
        ('\u{09c1}', "u"), ('\u{09c2}', "oo"), ('\u{09c3}', "ri"),
        ('\u{09c7}', "e"), ('\u{09c8}', "oi"), ('\u{09cb}', "o"),
        ('\u{09cc}', "ou"), ('\u{09c4}', "ri"),
    ],
    marks: &[
        ('\u{0982}', "n"), ('\u{0981}', "n"), ('\u{0983}', "h"),
        ('\u{09ce}', "t"), ('\u{09fa}',""),
        ('০', "0"), ('১', "1"), ('২', "2"), ('৩', "3"), ('৪', "4"),
        ('৫', "5"), ('৬', "6"), ('৭', "7"), ('৮', "8"), ('৯', "9"),
        ('।', "."), ('॥', "."),
    ],
};

// Gurmukhi — Punjabi.
const GURU: Abugida = Abugida {
    inherent: "a",
    virama: '\u{0a4d}',
    cons: &[
        ('ਕ', "k"), ('ਖ', "kh"), ('ਗ', "g"), ('ਘ', "gh"), ('ਙ', "ng"),
        ('ਚ', "ch"), ('ਛ', "chh"), ('ਜ', "j"), ('ਝ', "jh"), ('ਞ', "ny"),
        ('ਟ', "t"), ('ਠ', "th"), ('ਡ', "d"), ('ਢ', "dh"), ('ਣ', "n"),
        ('ਤ', "t"), ('ਥ', "th"), ('ਦ', "d"), ('ਧ', "dh"), ('ਨ', "n"),
        ('ਪ', "p"), ('ਫ', "ph"), ('ਬ', "b"), ('ਭ', "bh"), ('ਮ', "m"),
        ('ਯ', "y"), ('ਰ', "r"), ('ਲ', "l"), ('ਵ', "v"),
        ('ਸ', "s"), ('ਹ', "h"), ('ੜ', "r"),
    ],
    nukta: &[
        ('ਸ', "sh"), ('ਖ', "kh"), ('ਗ', "gh"), ('ਜ', "z"),
        ('ਫ', "f"), ('ਲ', "l"),
    ],
    indep: &[
        ('ਅ', "a"), ('ਆ', "aa"), ('ਇ', "i"), ('ਈ', "ee"),
        ('ਉ', "u"), ('ਊ', "oo"), ('ਏ', "e"), ('ਐ', "ai"),
        ('ਓ', "o"), ('ਔ', "au"),
    ],
    matra: &[
        ('\u{0a3e}', "aa"), ('\u{0a3f}', "i"), ('\u{0a40}', "ee"),
        ('\u{0a41}', "u"), ('\u{0a42}', "oo"), ('\u{0a47}', "e"),
        ('\u{0a48}', "ai"), ('\u{0a4b}', "o"), ('\u{0a4c}', "au"),
    ],
    marks: &[
        ('\u{0a02}', "n"), ('\u{0a03}', "h"), ('\u{0a70}', "n"),
        ('\u{0a71}', ""), ('\u{0a05}', ""),
        ('੦', "0"), ('੧', "1"), ('੨', "2"), ('੩', "3"), ('੪', "4"),
        ('੫', "5"), ('੬', "6"), ('੭', "7"), ('੮', "8"), ('੯', "9"),
    ],
};

// Gujarati.
const GUJR: Abugida = Abugida {
    inherent: "a",
    virama: '\u{0acd}',
    cons: &[
        ('ક', "k"), ('ખ', "kh"), ('ગ', "g"), ('ઘ', "gh"), ('ઙ', "n"),
        ('ચ', "ch"), ('છ', "chh"), ('જ', "j"), ('ઝ', "jh"), ('ઞ', "ny"),
        ('ટ', "t"), ('ઠ', "th"), ('ડ', "d"), ('ઢ', "dh"), ('ણ', "n"),
        ('ત', "t"), ('થ', "th"), ('દ', "d"), ('ધ', "dh"), ('ન', "n"),
        ('પ', "p"), ('ફ', "ph"), ('બ', "b"), ('ભ', "bh"), ('મ', "m"),
        ('ય', "y"), ('ર', "r"), ('લ', "l"), ('વ', "v"),
        ('શ', "sh"), ('ષ', "sh"), ('સ', "s"), ('હ', "h"), ('ળ', "l"),
    ],
    nukta: &[],
    indep: &[
        ('અ', "a"), ('આ', "aa"), ('ઇ', "i"), ('ઈ', "ee"),
        ('ઉ', "u"), ('ઊ', "oo"), ('ઋ', "ri"), ('એ', "e"),
        ('ઐ', "ai"), ('ઓ', "o"), ('ઔ', "au"), ('ઍ', "e"), ('ઑ', "o"),
    ],
    matra: &[
        ('\u{0abe}', "aa"), ('\u{0abf}', "i"), ('\u{0ac0}', "ee"),
        ('\u{0ac1}', "u"), ('\u{0ac2}', "oo"), ('\u{0ac3}', "ri"),
        ('\u{0ac5}', "e"), ('\u{0ac7}', "e"), ('\u{0ac8}', "ai"),
        ('\u{0ac9}', "o"), ('\u{0acb}', "o"), ('\u{0acc}', "au"),
    ],
    marks: &[
        ('\u{0a82}', "n"), ('\u{0a81}', "n"), ('\u{0a83}', "h"),
        ('ઽ', "'"),
        ('૦', "0"), ('૧', "1"), ('૨', "2"), ('૩', "3"), ('૪', "4"),
        ('૫', "5"), ('૬', "6"), ('૭', "7"), ('૮', "8"), ('૯', "9"),
        ('।', "."), ('॥', "."),
    ],
};

// Tamil — few consonant letters; voicing is positional.
const TAML: Abugida = Abugida {
    inherent: "a",
    virama: '\u{0bcd}',
    cons: &[
        ('க', "k"), ('ங', "ng"), ('ச', "ch"), ('ஞ', "ny"),
        ('ட', "t"), ('ண', "n"), ('த', "th"), ('ந', "n"), ('ன', "n"),
        ('ப', "p"), ('ம', "m"), ('ய', "y"), ('ர', "r"), ('ற', "r"),
        ('ல', "l"), ('ள', "l"), ('ழ', "zh"), ('வ', "v"),
        ('ஷ', "sh"), ('ஸ', "s"), ('ஹ', "h"), ('ஜ', "j"), ('ஶ', "sh"),
    ],
    nukta: &[],
    indep: &[
        ('அ', "a"), ('ஆ', "aa"), ('இ', "i"), ('ஈ', "ee"),
        ('உ', "u"), ('ஊ', "oo"), ('எ', "e"), ('ஏ', "ee"),
        ('ஐ', "ai"), ('ஒ', "o"), ('ஓ', "oo"), ('ஔ', "au"),
    ],
    matra: &[
        ('\u{0bbe}', "aa"), ('\u{0bbf}', "i"), ('\u{0bc0}', "ee"),
        ('\u{0bc1}', "u"), ('\u{0bc2}', "oo"), ('\u{0bc6}', "e"),
        ('\u{0bc7}', "ee"), ('\u{0bc8}', "ai"), ('\u{0bca}', "o"),
        ('\u{0bcb}', "oo"), ('\u{0bcc}', "au"),
    ],
    marks: &[
        ('\u{0b82}', "m"), ('\u{0b83}', "h"),
        ('௦', "0"), ('௧', "1"), ('௨', "2"), ('௩', "3"), ('௪', "4"),
        ('௫', "5"), ('௬', "6"), ('௭', "7"), ('௮', "8"), ('௯', "9"),
    ],
};

// Telugu.
const TELU: Abugida = Abugida {
    inherent: "a",
    virama: '\u{0c4d}',
    cons: &[
        ('క', "k"), ('ఖ', "kh"), ('గ', "g"), ('ఘ', "gh"), ('ఙ', "n"),
        ('చ', "ch"), ('ఛ', "chh"), ('జ', "j"), ('ఝ', "jh"), ('ఞ', "ny"),
        ('ట', "t"), ('ఠ', "th"), ('డ', "d"), ('ఢ', "dh"), ('ణ', "n"),
        ('త', "t"), ('థ', "th"), ('ద', "d"), ('ధ', "dh"), ('న', "n"),
        ('ప', "p"), ('ఫ', "ph"), ('బ', "b"), ('భ', "bh"), ('మ', "m"),
        ('య', "y"), ('ర', "r"), ('ఱ', "r"), ('ల', "l"), ('ళ', "l"),
        ('వ', "v"), ('శ', "sh"), ('ష', "sh"), ('స', "s"), ('హ', "h"),
        ('ఴ', "zh"),
    ],
    nukta: &[],
    indep: &[
        ('అ', "a"), ('ఆ', "aa"), ('ఇ', "i"), ('ఈ', "ee"),
        ('ఉ', "u"), ('ఊ', "oo"), ('ఋ', "ri"), ('ఎ', "e"),
        ('ఏ', "ee"), ('ఐ', "ai"), ('ఒ', "o"), ('ఓ', "oo"), ('ఔ', "au"),
    ],
    matra: &[
        ('\u{0c3e}', "aa"), ('\u{0c3f}', "i"), ('\u{0c40}', "ee"),
        ('\u{0c41}', "u"), ('\u{0c42}', "oo"), ('\u{0c43}', "ri"),
        ('\u{0c46}', "e"), ('\u{0c47}', "ee"), ('\u{0c48}', "ai"),
        ('\u{0c4a}', "o"), ('\u{0c4b}', "oo"), ('\u{0c4c}', "au"),
        ('\u{0c44}', "ri"), ('\u{0c55}', "ee"), ('\u{0c56}', "ai"),
    ],
    marks: &[
        ('\u{0c02}', "m"), ('\u{0c01}', "n"), ('\u{0c03}', "h"),
        ('౦', "0"), ('౧', "1"), ('౨', "2"), ('౩', "3"), ('౪', "4"),
        ('౫', "5"), ('౬', "6"), ('౭', "7"), ('౮', "8"), ('౯', "9"),
    ],
};

// Kannada.
const KNDA: Abugida = Abugida {
    inherent: "a",
    virama: '\u{0ccd}',
    cons: &[
        ('ಕ', "k"), ('ಖ', "kh"), ('ಗ', "g"), ('ಘ', "gh"), ('ಙ', "n"),
        ('ಚ', "ch"), ('ಛ', "chh"), ('ಜ', "j"), ('ಝ', "jh"), ('ಞ', "ny"),
        ('ಟ', "t"), ('ಠ', "th"), ('ಡ', "d"), ('ಢ', "dh"), ('ಣ', "n"),
        ('ತ', "t"), ('ಥ', "th"), ('ದ', "d"), ('ಧ', "dh"), ('ನ', "n"),
        ('ಪ', "p"), ('ಫ', "ph"), ('ಬ', "b"), ('ಭ', "bh"), ('ಮ', "m"),
        ('ಯ', "y"), ('ರ', "r"), ('ಱ', "r"), ('ಲ', "l"), ('ಳ', "l"),
        ('ವ', "v"), ('ಶ', "sh"), ('ಷ', "sh"), ('ಸ', "s"), ('ಹ', "h"),
    ],
    nukta: &[('ಫ', "f"), ('ಜ', "z")],
    indep: &[
        ('ಅ', "a"), ('ಆ', "aa"), ('ಇ', "i"), ('ಈ', "ee"),
        ('ಉ', "u"), ('ಊ', "oo"), ('ಋ', "ri"), ('ಎ', "e"),
        ('ಏ', "ee"), ('ಐ', "ai"), ('ಒ', "o"), ('ಓ', "oo"), ('ಔ', "au"),
    ],
    matra: &[
        ('\u{0cbe}', "aa"), ('\u{0cbf}', "i"), ('\u{0cc0}', "ee"),
        ('\u{0cc1}', "u"), ('\u{0cc2}', "oo"), ('\u{0cc3}', "ri"),
        ('\u{0cc6}', "e"), ('\u{0cc7}', "ee"), ('\u{0cc8}', "ai"),
        ('\u{0cca}', "o"), ('\u{0ccb}', "oo"), ('\u{0ccc}', "au"),
        ('\u{0cc4}', "ri"), ('\u{0cd5}', "ee"), ('\u{0cd6}', "ai"),
    ],
    marks: &[
        ('\u{0c82}', "m"), ('\u{0c81}', "n"), ('\u{0c83}', "h"),
        ('೦', "0"), ('೧', "1"), ('೨', "2"), ('೩', "3"), ('೪', "4"),
        ('೫', "5"), ('೬', "6"), ('೭', "7"), ('೮', "8"), ('೯', "9"),
    ],
};

// Malayalam.
const MLYM: Abugida = Abugida {
    inherent: "a",
    virama: '\u{0d4d}',
    cons: &[
        ('ക', "k"), ('ഖ', "kh"), ('ഗ', "g"), ('ഘ', "gh"), ('ങ', "n"),
        ('ച', "ch"), ('ഛ', "chh"), ('ജ', "j"), ('ഝ', "jh"), ('ഞ', "ny"),
        ('ട', "t"), ('ഠ', "th"), ('ഡ', "d"), ('ഢ', "dh"), ('ണ', "n"),
        ('ത', "th"), ('ഥ', "th"), ('ദ', "d"), ('ധ', "dh"), ('ന', "n"),
        ('ഩ', "n"), ('പ', "p"), ('ഫ', "ph"), ('ബ', "b"), ('ഭ', "bh"),
        ('മ', "m"), ('യ', "y"), ('ര', "r"), ('റ', "r"), ('ല', "l"),
        ('ള', "l"), ('ഴ', "zh"), ('വ', "v"), ('ശ', "sh"), ('ഷ', "sh"),
        ('സ', "s"), ('ഹ', "h"), ('ഺ', "t"),
    ],
    nukta: &[],
    indep: &[
        ('അ', "a"), ('ആ', "aa"), ('ഇ', "i"), ('ഈ', "ee"),
        ('ഉ', "u"), ('ഊ', "oo"), ('ഋ', "ri"), ('ൠ', "ri"),
        ('എ', "e"), ('ഏ', "ee"), ('ഐ', "ai"), ('ഒ', "o"),
        ('ഓ', "oo"), ('ഔ', "au"),
    ],
    matra: &[
        ('\u{0d3e}', "aa"), ('\u{0d3f}', "i"), ('\u{0d40}', "ee"),
        ('\u{0d41}', "u"), ('\u{0d42}', "oo"), ('\u{0d43}', "ri"),
        ('\u{0d46}', "e"), ('\u{0d47}', "ee"), ('\u{0d48}', "ai"),
        ('\u{0d4a}', "o"), ('\u{0d4b}', "oo"), ('\u{0d4c}', "au"),
        ('\u{0d57}', "au"),
    ],
    marks: &[
        ('\u{0d02}', "m"), ('\u{0d03}', "h"),
        ('൦', "0"), ('൧', "1"), ('൨', "2"), ('൩', "3"), ('൪', "4"),
        ('൫', "5"), ('൬', "6"), ('൭', "7"), ('൮', "8"), ('൯', "9"),
    ],
};

// ---- simple char-map scripts ----------------------------------------

// Arabic incl. Persian/Urdu letters; harakat vowel marks map to their
// vowel, shadda/sukun/tatweel go silent.
const ARAB: &[(char, &str)] = &[
    ('ا', "a"), ('ب', "b"), ('ت', "t"), ('ث', "th"), ('ج', "j"),
    ('ح', "h"), ('خ', "kh"), ('د', "d"), ('ذ', "dh"), ('ر', "r"),
    ('ز', "z"), ('س', "s"), ('ش', "sh"), ('ص', "s"), ('ض', "d"),
    ('ط', "t"), ('ظ', "z"), ('ع', "'"), ('غ', "gh"), ('ف', "f"),
    ('ق', "q"), ('ك', "k"), ('ک', "k"), ('ل', "l"), ('م', "m"),
    ('ن', "n"), ('ه', "h"), ('ھ', "h"), ('ہ', "h"), ('و', "w"),
    ('ي', "y"), ('ی', "y"), ('ے', "e"), ('ء', "'"), ('ئ', "'"),
    ('ؤ', "'"), ('ة', "a"), ('ى', "a"), ('آ', "aa"), ('إ', "i"),
    ('أ', "a"), ('ٱ', "a"), ('پ', "p"), ('چ', "ch"), ('ژ', "zh"),
    ('گ', "g"), ('ڤ', "v"), ('ڨ', "g"), ('ٹ', "t"), ('ڈ', "d"),
    ('ڑ', "r"), ('ں', "n"), ('ٻ', "b"), ('ڄ', "j"), ('ڃ', "ny"),
    ('ټ', "t"), ('څ', "ts"), ('ځ', "dz"), ('ګ', "g"), ('ڼ', "n"),
    ('\u{064b}', "an"), ('\u{064c}', "un"), ('\u{064d}', "in"),
    ('\u{064e}', "a"), ('\u{064f}', "u"), ('\u{0650}', "i"),
    ('\u{0651}', ""), ('\u{0652}', ""), ('\u{0670}', "a"),
    ('ـ', ""), ('٠', "0"), ('١', "1"), ('٢', "2"), ('٣', "3"),
    ('٤', "4"), ('٥', "5"), ('٦', "6"), ('٧', "7"), ('٨', "8"),
    ('٩', "9"), ('۰', "0"), ('۱', "1"), ('۲', "2"), ('۳', "3"),
    ('۴', "4"), ('۵', "5"), ('۶', "6"), ('۷', "7"), ('۸', "8"),
    ('۹', "9"),
];

// Hebrew consonants; niqqud → approximate vowels.
const HEBR: &[(char, &str)] = &[
    ('א', "'"), ('ב', "v"), ('ג', "g"), ('ד', "d"), ('ה', "h"),
    ('ו', "v"), ('ז', "z"), ('ח', "kh"), ('ט', "t"), ('י', "y"),
    ('כ', "kh"), ('ך', "kh"), ('ל', "l"), ('מ', "m"), ('ם', "m"),
    ('נ', "n"), ('ן', "n"), ('ס', "s"), ('ע', "'"), ('פ', "f"),
    ('ף', "f"), ('צ', "ts"), ('ץ', "ts"), ('ק', "k"), ('ר', "r"),
    ('ש', "sh"), ('ת', "t"),
    ('\u{05b0}', "e"), ('\u{05b1}', "e"), ('\u{05b2}', "a"),
    ('\u{05b3}', "o"), ('\u{05b4}', "i"), ('\u{05b5}', "e"),
    ('\u{05b6}', "e"), ('\u{05b7}', "a"), ('\u{05b8}', "a"),
    ('\u{05b9}', "o"), ('\u{05ba}', "o"), ('\u{05bb}', "u"),
    ('\u{05bc}', ""), ('\u{05bd}', ""), ('\u{05bf}', ""),
    ('\u{05c1}', ""), ('\u{05c2}', ""), ('\u{05c7}', "a"),
];

// Greek (modern).
const GREK: &[(char, &str)] = &[
    ('α', "a"), ('β', "v"), ('γ', "g"), ('δ', "d"), ('ε', "e"),
    ('ζ', "z"), ('η', "i"), ('θ', "th"), ('ι', "i"), ('κ', "k"),
    ('λ', "l"), ('μ', "m"), ('ν', "n"), ('ξ', "x"), ('ο', "o"),
    ('π', "p"), ('ρ', "r"), ('σ', "s"), ('ς', "s"), ('τ', "t"),
    ('υ', "y"), ('φ', "f"), ('χ', "ch"), ('ψ', "ps"), ('ω', "o"),
    ('ά', "a"), ('έ', "e"), ('ή', "i"), ('ί', "i"), ('ό', "o"),
    ('ύ', "y"), ('ώ', "o"), ('ϊ', "i"), ('ϋ', "y"), ('ΐ', "i"),
    ('ΰ', "y"), ('ϐ', "v"), ('ϑ', "th"), ('ϰ', "k"), ('ϱ', "r"),
    ('ἀ', "a"), ('ἁ', "a"), ('ἐ', "e"), ('ἑ', "e"), ('ἠ', "i"),
    ('ἡ', "i"), ('ἰ', "i"), ('ἱ', "i"), ('ὀ', "o"), ('ὁ', "o"),
    ('ὐ', "y"), ('ὑ', "y"), ('ὠ', "o"), ('ὡ', "o"),
];

// Armenian.
const ARMN: &[(char, &str)] = &[
    ('ա', "a"), ('բ', "b"), ('գ', "g"), ('դ', "d"), ('ե', "e"),
    ('զ', "z"), ('է', "e"), ('ը', "e"), ('թ', "t"), ('ժ', "zh"),
    ('ի', "i"), ('լ', "l"), ('խ', "kh"), ('ծ', "ts"), ('կ', "k"),
    ('հ', "h"), ('ձ', "dz"), ('ղ', "gh"), ('ճ', "ch"), ('մ', "m"),
    ('յ', "y"), ('ն', "n"), ('շ', "sh"), ('ո', "o"), ('չ', "ch"),
    ('պ', "p"), ('ջ', "j"), ('ռ', "r"), ('ս', "s"), ('վ', "v"),
    ('տ', "t"), ('ր', "r"), ('ց', "ts"), ('ւ', "u"), ('փ', "p"),
    ('ք', "k"), ('օ', "o"), ('ֆ', "f"), ('և', "ew"), ('ֈ', "v"),
];

// Georgian.
const GEOR: &[(char, &str)] = &[
    ('ა', "a"), ('ბ', "b"), ('გ', "g"), ('დ', "d"), ('ე', "e"),
    ('ვ', "v"), ('ზ', "z"), ('თ', "t"), ('ი', "i"), ('კ', "k"),
    ('ლ', "l"), ('მ', "m"), ('ნ', "n"), ('ო', "o"), ('პ', "p"),
    ('ჟ', "zh"), ('რ', "r"), ('ს', "s"), ('ტ', "t"), ('უ', "u"),
    ('ფ', "p"), ('ქ', "k"), ('ღ', "gh"), ('ყ', "q"), ('შ', "sh"),
    ('ჩ', "ch"), ('ც', "ts"), ('ძ', "dz"), ('წ', "ts"), ('ჭ', "ch"),
    ('ხ', "kh"), ('ჯ', "j"), ('ჰ', "h"), ('ჱ', "e"), ('ჲ', "y"),
    ('ჳ', "w"), ('ჴ', "q"), ('ჵ', "o"),
];

// Thai — approximate: consonants + explicit vowel signs; tone marks
// and the silent-marker drop out. Leading-vowel signs (เ, แ, โ, ใ,
// ไ) sit before their consonant in the stream, so the romanized
// vowel lands one slot early — readable, not positional-perfect.
const THAI: &[(char, &str)] = &[
    ('ก', "k"), ('ข', "kh"), ('ฃ', "kh"), ('ค', "kh"), ('ฅ', "kh"),
    ('ฆ', "kh"), ('ง', "ng"), ('จ', "ch"), ('ฉ', "ch"), ('ช', "ch"),
    ('ซ', "s"), ('ฌ', "ch"), ('ญ', "y"), ('ฎ', "d"), ('ฏ', "t"),
    ('ฐ', "th"), ('ฑ', "th"), ('ฒ', "th"), ('ณ', "n"), ('ด', "d"),
    ('ต', "t"), ('ถ', "th"), ('ท', "th"), ('ธ', "th"), ('น', "n"),
    ('บ', "b"), ('ป', "p"), ('ผ', "ph"), ('ฝ', "f"), ('พ', "ph"),
    ('ฟ', "f"), ('ภ', "ph"), ('ม', "m"), ('ย', "y"), ('ร', "r"),
    ('ฤ', "rue"), ('ล', "l"), ('ฦ', "lue"), ('ว', "w"), ('ศ', "s"),
    ('ษ', "s"), ('ส', "s"), ('ห', "h"), ('ฬ', "l"), ('อ', "'"),
    ('ฮ', "h"), ('ฯ', ""), ('ะ', "a"), ('ั', "a"), ('า', "aa"),
    ('ำ', "am"), ('ิ', "i"), ('ี', "ii"), ('ึ', "ue"), ('ื', "ue"),
    ('ุ', "u"), ('ู', "uu"), ('ฺ', ""), ('็', ""), ('่', ""),
    ('้', ""), ('๊', ""), ('๋', ""), ('์', ""), ('ํ', "am"),
    ('๎', ""), ('ๆ', " (repeat)"), ('เ', "e"), ('แ', "ae"),
    ('โ', "o"), ('ใ', "ai"), ('ไ', "ai"), ('ๅ', "o"),
    ('๐', "0"), ('๑', "1"), ('๒', "2"), ('๓', "3"), ('๔', "4"),
    ('๕', "5"), ('๖', "6"), ('๗', "7"), ('๘', "8"), ('๙', "9"),
];

/// Fill `Note.alt` for every non-Latin note — the Python pipeline's
/// `romanize_chart`. Returns true when any alternate was written.
pub fn romanize_chart(chart: &mut Chart) -> bool {
    let all: String = chart
        .tracks
        .values()
        .flatten()
        .map(|n| n.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let doc = script_of(&all);
    if !romanizable(doc) && scripts_in(&all).iter()
            .all(|s| !romanizable(s)) {
        return false;
    }
    let hint = if romanizable(doc) { doc } else { "" };
    // one python call per chart when a dictionary is needed —
    // batch the non-latin runs, which is what the map is keyed on
    let mut py_map = std::collections::HashMap::new();
    if hint == "jp" || hint == "kr" {
        let mut runs: Vec<String> = Vec::new();
        for n in chart.tracks.values().flatten() {
            for (run, latin) in script_runs(&n.text) {
                if !latin {
                    runs.push(run);
                }
            }
        }
        runs.sort();
        runs.dedup();
        let refs: Vec<&str> = runs.iter().map(|s| s.as_str()).collect();
        py_map = py_romanize(&refs, hint);
    }
    let mut any_alt = false;
    for notes in chart.tracks.values_mut() {
        for n in notes.iter_mut() {
            if !n.alt.is_empty() {
                continue;
            }
            let alt = romanize_text(&n.text, hint, &py_map);
            if !alt.is_empty() && alt != n.text {
                n.alt = alt;
                any_alt = true;
            }
        }
    }
    if any_alt {
        chart
            .headers
            .entry("ALTSCRIPT".to_string())
            .or_insert_with(|| "romaji".to_string());
    }
    any_alt
}
