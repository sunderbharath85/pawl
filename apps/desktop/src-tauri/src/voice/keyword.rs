//! The wake word is the pet's name, turned into keyword-spotter tokens at runtime: the name is folded to the
//! model's alphabet (upper-case English letters and the apostrophe), encoded with the model's own sentencepiece
//! BPE (`bpe.model`, through kitoken: upstream ships this step only in Python) and each piece checked against the
//! model's `tokens.txt`. The spotter listens for "<name>" and "hey <name>".

use kitoken::Kitoken;
use std::collections::HashMap;
use std::path::Path;

/// Longest keyword we build, in words (a long "say it like" spelling is a sentence, not a name).
const MAX_WORDS: usize = 6;

fn digit_word(d: char) -> &'static str {
    match d {
        '0' => "ZERO",
        '1' => "ONE",
        '2' => "TWO",
        '3' => "THREE",
        '4' => "FOUR",
        '5' => "FIVE",
        '6' => "SIX",
        '7' => "SEVEN",
        '8' => "EIGHT",
        _ => "NINE",
    }
}

/// A lower-case letter outside ASCII folded to ASCII (common Latin accents and ligatures), or `None`.
fn fold_latin(c: char) -> Option<&'static str> {
    Some(match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => "a",
        'ç' | 'ć' | 'č' => "c",
        'ď' | 'đ' | 'ð' => "d",
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ė' | 'ę' | 'ě' => "e",
        'ğ' => "g",
        'ì' | 'í' | 'î' | 'ï' | 'ī' | 'į' | 'ı' => "i",
        'ł' | 'ľ' => "l",
        'ñ' | 'ń' | 'ň' => "n",
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ő' => "o",
        'ř' => "r",
        'ś' | 'š' | 'ş' => "s",
        'ť' | 'ţ' => "t",
        'ù' | 'ú' | 'û' | 'ü' | 'ū' | 'ů' | 'ű' => "u",
        'ý' | 'ÿ' => "y",
        'ź' | 'ż' | 'ž' => "z",
        'ß' => "ss",
        'æ' => "ae",
        'œ' => "oe",
        'þ' => "th",
        _ => return None,
    })
}

/// The words of `name` in the spotter's alphabet: upper-case ASCII letters (accents folded), digits spelled out
/// ("R2" -> "R TWO"), an apostrophe kept only inside a word ("O'Neil"); anything else separates words. `None` when
/// nothing speakable is left (emoji, other scripts): the user needs a "say it like" spelling then.
pub fn normalize(name: &str) -> Option<Vec<String>> {
    let mut words: Vec<String> = Vec::new();
    let mut cur = String::new();
    let flush = |cur: &mut String, words: &mut Vec<String>| {
        let w = cur.trim_matches('\'').to_string();
        if !w.is_empty() {
            words.push(w);
        }
        cur.clear();
    };
    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_lowercase() {
            cur.push(c.to_ascii_uppercase());
        } else if let Some(f) = fold_latin(c) {
            cur.push_str(&f.to_ascii_uppercase());
        } else if c.is_ascii_digit() {
            flush(&mut cur, &mut words);
            words.push(digit_word(c).to_string());
        } else if (c == '\'' || c == '’') && !cur.is_empty() {
            cur.push('\'');
        } else {
            flush(&mut cur, &mut words);
        }
    }
    flush(&mut cur, &mut words);
    // An apostrophe run inside a word collapses to one.
    for w in &mut words {
        while w.contains("''") {
            *w = w.replace("''", "'");
        }
    }
    (!words.is_empty()).then_some(words)
}

/// Keyword boost and trigger threshold for a sensitivity in 0..1: at 0.5 the spotter's own defaults scaled for a
/// name (boost 1.5, threshold 0.25); higher wakes more easily (and more often by mistake).
pub fn sensitivity_params(sensitivity: f32) -> (f32, f32) {
    let s = if sensitivity.is_finite() { sensitivity.clamp(0.0, 1.0) } else { 0.5 };
    (0.5 + 2.0 * s, 0.40 - 0.30 * s)
}

/// The keyword-spotter model's BPE and token table.
pub struct KeywordEncoder {
    bpe: Kitoken,
    pieces: HashMap<u32, String>,
    unk: Option<u32>,
}

/// One line of the spotter's keyword list: `▁BI S C U IT :1.5 #0.25 @BISCUIT`.
#[derive(Clone, Debug, PartialEq)]
pub struct KeywordLine {
    pub pieces: Vec<String>,
    pub phrase: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct KeywordSpec {
    pub lines: Vec<KeywordLine>,
    pub boost: f32,
    pub threshold: f32,
}

impl KeywordSpec {
    /// The spotter's runtime keyword string (lines separated by '/', as `create_stream_with_keywords` takes it).
    pub fn to_keywords(&self) -> String {
        self.lines
            .iter()
            .map(|l| format!("{} :{:.2} #{:.2} @{}", l.pieces.join(" "), self.boost, self.threshold, l.phrase))
            .collect::<Vec<_>>()
            .join("/")
    }
}

/// `tokens.txt` ("<piece> <id>" per line) as id -> piece.
fn parse_tokens(text: &str) -> HashMap<u32, String> {
    text.lines()
        .filter_map(|l| {
            let (piece, id) = l.trim_end().rsplit_once(' ')?;
            Some((id.parse().ok()?, piece.to_string()))
        })
        .collect()
}

impl KeywordEncoder {
    pub fn load(bpe_model: &Path, tokens: &Path) -> Result<Self, String> {
        let bpe = Kitoken::from_sentencepiece_file(bpe_model).map_err(|e| format!("wake word model: {e}"))?;
        let text = std::fs::read_to_string(tokens).map_err(|e| format!("wake word tokens: {e}"))?;
        Self::new(bpe, &text)
    }

    fn new(bpe: Kitoken, tokens: &str) -> Result<Self, String> {
        let pieces = parse_tokens(tokens);
        if pieces.is_empty() {
            return Err("wake word tokens: empty table".into());
        }
        let unk = pieces.iter().find(|(_, p)| p.as_str() == "<unk>").map(|(id, _)| *id);
        Ok(Self { bpe, pieces, unk })
    }

    /// The model pieces for already-normalized words.
    pub fn pieces(&self, words: &[String]) -> Result<Vec<String>, String> {
        let text = words.join(" ");
        let ids = self.bpe.encode(&text, true).map_err(|e| format!("could not spell “{text}”: {e}"))?;
        if ids.is_empty() {
            return Err(format!("could not spell “{text}”"));
        }
        ids.iter()
            .map(|id| match self.pieces.get(id) {
                Some(p) if Some(*id) != self.unk => Ok(p.clone()),
                _ => Err(format!("could not spell “{text}” with the wake word model")),
            })
            .collect()
    }

    /// The keyword list for `spoken` (the pet's name or its "say it like" spelling): "<name>" and "hey <name>".
    pub fn spec(&self, spoken: &str, sensitivity: f32) -> Result<KeywordSpec, String> {
        let words = normalize(spoken).ok_or_else(|| {
            format!("Pawl can't listen for “{spoken}”: add a “say it like” spelling with English letters.")
        })?;
        if words.len() > MAX_WORDS {
            return Err(format!("The wake word can have at most {MAX_WORDS} words."));
        }
        let mut variants = vec![words.clone()];
        if words.first().map(String::as_str) != Some("HEY") {
            let mut hey = vec!["HEY".to_string()];
            hey.extend(words);
            variants.push(hey);
        }
        let (boost, threshold) = sensitivity_params(sensitivity);
        let lines = variants
            .iter()
            .map(|w| Ok(KeywordLine { pieces: self.pieces(w)?, phrase: w.join("_") }))
            .collect::<Result<Vec<_>, String>>()?;
        Ok(KeywordSpec { lines, boost, threshold })
    }
}

fn speakable(word: &str) -> String {
    normalize(word).map(|w| w.join(" ")).unwrap_or_default()
}

/// The transcript without a leading "hey <name>," / "<name>," (an utterance after the wake word can start with
/// it). Only strips when the whole name matched, so "hey, run the tests" stays as it is.
pub fn strip_wake_prefix(text: &str, spoken: &str) -> String {
    let Some(name) = normalize(spoken) else {
        return text.trim().to_string();
    };
    let tokens: Vec<(usize, &str)> = {
        let mut out = Vec::new();
        let mut start = None;
        for (i, c) in text.char_indices() {
            match (c.is_whitespace(), start) {
                (false, None) => start = Some(i),
                (true, Some(s)) => {
                    out.push((s, &text[s..i]));
                    start = None;
                }
                _ => {}
            }
        }
        if let Some(s) = start {
            out.push((s, &text[s..]));
        }
        out
    };
    let mut idx = 0;
    if tokens
        .first()
        .is_some_and(|(_, t)| matches!(speakable(t).as_str(), "HEY" | "HI" | "OK" | "OKAY" | "HELLO"))
    {
        idx = 1;
    }
    // The name's words, one transcript token each (a token may carry punctuation: "Biscuit,").
    for w in &name {
        match tokens.get(idx) {
            Some((_, t)) if speakable(t) == *w => idx += 1,
            _ => return text.trim().to_string(),
        }
    }
    let rest = match tokens.get(idx) {
        Some((pos, _)) => &text[*pos..],
        None => "",
    };
    rest.trim_start_matches(|c: char| c.is_whitespace() || matches!(c, ',' | '.' | '!' | '?' | ';' | ':' | '-'))
        .trim_end()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_names() {
        let n = |s: &str| normalize(s).map(|w| w.join(" "));
        assert_eq!(n("Biscuit").as_deref(), Some("BISCUIT"));
        assert_eq!(n("  mr. biscuit ").as_deref(), Some("MR BISCUIT"));
        assert_eq!(n("Zoë").as_deref(), Some("ZOE"));
        assert_eq!(n("Bjørn").as_deref(), Some("BJORN"));
        assert_eq!(n("Straße").as_deref(), Some("STRASSE"));
        assert_eq!(n("R2-D2").as_deref(), Some("R TWO D TWO"));
        assert_eq!(n("K9").as_deref(), Some("K NINE"));
        assert_eq!(n("O'Neil").as_deref(), Some("O'NEIL"));
        assert_eq!(n("O’’Neil").as_deref(), Some("O'NEIL"));
        assert_eq!(n("'quoted'").as_deref(), Some("QUOTED"));
        assert_eq!(n("Fluffy🐶McFluff").as_deref(), Some("FLUFFY MCFLUFF"));
        assert_eq!(n("Señor_Woof").as_deref(), Some("SENOR WOOF"));
        assert_eq!(n("🐶🐶"), None);
        assert_eq!(n("犬"), None);
        assert_eq!(n("  "), None);
        assert_eq!(n("'"), None);
    }

    #[test]
    fn sensitivity_maps_to_boost_and_threshold() {
        let (b, t) = sensitivity_params(0.5);
        assert!((b - 1.5).abs() < 1e-6 && (t - 0.25).abs() < 1e-6);
        let (b0, t0) = sensitivity_params(0.0);
        let (b1, t1) = sensitivity_params(1.0);
        assert!(b1 > b && b > b0, "more sensitive = more boost");
        assert!(t1 < t && t < t0, "more sensitive = lower threshold");
        assert!(t1 > 0.0 && t0 < 1.0);
        assert_eq!(sensitivity_params(7.0), sensitivity_params(1.0));
        assert_eq!(sensitivity_params(f32::NAN), sensitivity_params(0.5));
    }

    #[test]
    fn keyword_string_format() {
        let spec = KeywordSpec {
            lines: vec![
                KeywordLine { pieces: vec!["▁BI".into(), "S".into()], phrase: "BIS".into() },
                KeywordLine { pieces: vec!["▁HE".into(), "Y".into(), "▁BI".into()], phrase: "HEY_BIS".into() },
            ],
            boost: 1.5,
            threshold: 0.25,
        };
        assert_eq!(spec.to_keywords(), "▁BI S :1.50 #0.25 @BIS/▁HE Y ▁BI :1.50 #0.25 @HEY_BIS");
    }

    #[test]
    fn parses_token_table() {
        let t = parse_tokens("<blk> 0\n<sos/eos> 1\n<unk> 2\nS 3\n▁ 20\n▁HE 77\nbad line\n");
        assert_eq!(t.get(&3).map(String::as_str), Some("S"));
        assert_eq!(t.get(&20).map(String::as_str), Some("▁"));
        assert_eq!(t.get(&1).map(String::as_str), Some("<sos/eos>"));
        assert_eq!(t.len(), 6);
    }

    #[test]
    fn strips_the_wake_phrase() {
        assert_eq!(strip_wake_prefix("Hey Biscuit, run the tests.", "Biscuit"), "run the tests.");
        assert_eq!(strip_wake_prefix("biscuit run the tests", "Biscuit"), "run the tests");
        assert_eq!(strip_wake_prefix("Hey, Mr. Biscuit! what's up", "Mr Biscuit"), "what's up");
        assert_eq!(strip_wake_prefix("Biscuit.", "Biscuit"), "");
        assert_eq!(strip_wake_prefix("Hey, run the tests", "Biscuit"), "Hey, run the tests");
        assert_eq!(strip_wake_prefix("tell Biscuit to stop", "Biscuit"), "tell Biscuit to stop");
        assert_eq!(strip_wake_prefix("  R2 D2 status ", "R2-D2"), "R2 D2 status");
        assert_eq!(strip_wake_prefix("  hello there ", "🐶"), "hello there");
        assert_eq!(strip_wake_prefix("", "Biscuit"), "");
    }

    /// The real model's BPE (from the local model directory, never from the repo). Skipped without models.
    #[test]
    fn encodes_names_with_the_real_bpe() {
        let Some(dir) = super::super::models::test_models_dir() else {
            eprintln!("skipped: set PAWL_VOICE_TEST_MODELS to the directory with the downloaded models");
            return;
        };
        let kws = dir.join(super::super::models::KWS_TEST_SUBDIR);
        let enc = KeywordEncoder::load(&kws.join("bpe.model"), &kws.join("tokens.txt")).unwrap();
        // The same pieces as upstream's Python text2token (checked by t13 and in the model's keywords.txt).
        assert_eq!(enc.pieces(&["HEY".into(), "SIRI".into()]).unwrap().join(" "), "▁HE Y ▁S I RI");
        assert_eq!(enc.pieces(&["LIGHT".into(), "UP".into()]).unwrap().join(" "), "▁ L IGHT ▁UP");
        assert_eq!(enc.pieces(&["BISCUIT".into()]).unwrap().join(" "), "▁BI S C U IT");
        assert_eq!(enc.pieces(&["KRYPTO".into()]).unwrap().join(" "), "▁K RY P T O");
        let spec = enc.spec("Biscuit", 0.5).unwrap();
        assert_eq!(spec.lines.len(), 2);
        assert_eq!(spec.lines[0].phrase, "BISCUIT");
        assert_eq!(spec.lines[1].phrase, "HEY_BISCUIT");
        assert_eq!(spec.lines[1].pieces.join(" "), "▁HE Y ▁BI S C U IT");
        // Odd spellings still produce valid pieces.
        for name in ["Zoë", "R2-D2", "O'Neil", "Xyzzy", "Mr. Fluffington III", "hey pawl"] {
            let spec = enc.spec(name, 0.7).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(spec.lines.iter().all(|l| !l.pieces.is_empty()), "{name}");
        }
        assert_eq!(enc.spec("hey pawl", 0.5).unwrap().lines.len(), 1, "no 'hey hey pawl'");
        assert!(enc.spec("🐶", 0.5).is_err());
        assert!(enc.spec("a b c d e f g", 0.5).is_err());
    }
}
