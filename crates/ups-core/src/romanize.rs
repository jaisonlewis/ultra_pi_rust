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
fn nonlatin(c: char) -> bool {
    is_hira(c) || is_kata(c) || is_hangul(c) || is_cyr(c) || is_kanji(c)
}

/// Dominant non-latin script: jp | kr | cyr | zh | latin.
pub fn script_of(text: &str) -> &'static str {
    if text.chars().any(|c| is_hira(c) || is_kata(c)) {
        return "jp";
    }
    if text.chars().any(is_hangul) {
        return "kr";
    }
    if text.chars().any(is_cyr) {
        return "cyr";
    }
    if text.chars().any(is_kanji) {
        return "zh"; // kanji w/o kana → likely zh
    }
    "latin"
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
    if !["jp", "kr", "cyr"].contains(&doc) {
        return String::new();
    }
    script_runs(text)
        .iter()
        .map(|(run, latin)| {
            if *latin {
                run.clone()
            } else {
                romanize_run(run, doc, py_map)
            }
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
        "latin" => run.to_string(),
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
        _ => run.to_string(),
    }
}

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
    let hint = if ["jp", "kr", "cyr"].contains(&doc) { doc } else { "" };
    if hint.is_empty() {
        return false;
    }
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
