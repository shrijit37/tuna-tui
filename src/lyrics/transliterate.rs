//! Transliteration for Indic and non-Latin scripts to Latin (Romanized English alphabet).
//!
//! Terminal emulators often lack complex Indic text shaping and dedicated font glyphs
//! for Devanagari (Hindi) and Gurmukhi (Punjabi), resulting in broken rendering.
//! This module provides Latin preference ranking and fallback transliteration.

/// Check if text is primarily Latin (English alphabet / ASCII).
pub fn is_latin_text(s: &str) -> bool {
    let mut latin = 0usize;
    let mut non_latin = 0usize;
    for c in s.chars() {
        let code = c as u32;
        if c.is_ascii_alphabetic() {
            latin += 1;
        } else if matches!(code, 0x0900..=0x0D7F | 0x0400..=0x04FF | 0x4E00..=0x9FFF | 0xAC00..=0xD7AF | 0x3040..=0x30FF) {
            non_latin += 1;
        }
    }
    non_latin == 0 || latin > non_latin * 2
}

/// Returns true if the text contains Indic script characters (Devanagari, Gurmukhi, etc.).
pub fn contains_indic(s: &str) -> bool {
    s.chars().any(|c| matches!(c as u32, 0x0900..=0x0D7F))
}

/// Transliterate Indic characters (Devanagari / Gurmukhi) in a string to Romanized Latin text.
pub fn transliterate_indic(s: &str) -> String {
    if !contains_indic(s) {
        return s.to_string();
    }
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();
    let mut out = String::with_capacity(s.len() * 2);
    let mut i = 0;
    while i < len {
        let ch = chars[i];
        let code = ch as u32;

        // Devanagari (Hindi, Marathi, etc. 0x0900..=0x097F)
        if (0x0900..=0x097F).contains(&code) {
            if let Some(v) = devanagari_vowel(ch) {
                out.push_str(v);
                i += 1;
                continue;
            }
            if let Some(c) = devanagari_consonant(ch) {
                let mut advance = 1;
                let mut consonant_str = c;
                // Check for nukta modifier (\u{093C})
                if chars.get(i + 1) == Some(&'\u{093C}') {
                    consonant_str = match ch {
                        'क' => "q",
                        'ख' => "kh",
                        'ग' => "g",
                        'ज' => "z",
                        'ड' => "r",
                        'ढ' => "rh",
                        'फ' => "f",
                        _ => c,
                    };
                    advance = 2;
                }
                out.push_str(consonant_str);

                let next = chars.get(i + advance).copied();
                if let Some(n) = next {
                    if n == '्' {
                        // Halant / virama: skip without inherent 'a'
                        i += advance + 1;
                        continue;
                    } else if let Some(m) = devanagari_matra(n) {
                        out.push_str(m);
                        i += advance + 1;
                        continue;
                    }
                }
                // Inherent 'a' if not followed by punctuation or whitespace or word end
                if let Some(n) = next {
                    if !n.is_whitespace() && !n.is_ascii_punctuation() && !matches!(n, '।' | '॥' | ',' | '.' | '?' | '!') {
                        out.push('a');
                    }
                }
                i += advance;
                continue;
            }
            if let Some(m) = devanagari_matra(ch) {
                out.push_str(m);
                i += 1;
                continue;
            }
            if ch == '।' || ch == '॥' {
                out.push('.');
                i += 1;
                continue;
            }
        }

        // Gurmukhi (Punjabi 0x0A00..=0x0A7F)
        if (0x0A00..=0x0A7F).contains(&code) {
            if let Some(v) = gurmukhi_vowel(ch) {
                out.push_str(v);
                i += 1;
                continue;
            }
            if let Some(c) = gurmukhi_consonant(ch) {
                out.push_str(c);
                let next = chars.get(i + 1).copied();
                if let Some(n) = next {
                    if n == '੍' {
                        i += 2;
                        continue;
                    } else if let Some(m) = gurmukhi_matra(n) {
                        out.push_str(m);
                        i += 2;
                        continue;
                    }
                }
                if let Some(n) = next {
                    if !n.is_whitespace() && !n.is_ascii_punctuation() && !matches!(n, '।' | '॥' | ',' | '.' | '?' | '!') {
                        out.push('a');
                    }
                }
                i += 1;
                continue;
            }
            if let Some(m) = gurmukhi_matra(ch) {
                out.push_str(m);
                i += 1;
                continue;
            }
        }

        out.push(ch);
        i += 1;
    }
    out
}

fn devanagari_vowel(c: char) -> Option<&'static str> {
    match c {
        'अ' => Some("a"),
        'आ' => Some("aa"),
        'इ' => Some("i"),
        'ई' => Some("ee"),
        'उ' => Some("u"),
        'ऊ' => Some("oo"),
        'ऋ' => Some("ri"),
        'ए' => Some("e"),
        'ऐ' => Some("ai"),
        'ओ' => Some("o"),
        'औ' => Some("au"),
        _ => None,
    }
}

fn devanagari_matra(c: char) -> Option<&'static str> {
    match c {
        'ा' => Some("aa"),
        'ि' => Some("i"),
        'ी' => Some("ee"),
        'ु' => Some("u"),
        'ू' => Some("oo"),
        'ृ' => Some("ri"),
        'े' => Some("e"),
        'ै' => Some("ai"),
        'ो' => Some("o"),
        'ौ' => Some("au"),
        'ं' | 'ँ' => Some("n"),
        'ः' => Some("h"),
        _ => None,
    }
}

fn devanagari_consonant(c: char) -> Option<&'static str> {
    match c {
        'क' => Some("k"),
        'ख' => Some("kh"),
        'ग' => Some("g"),
        'घ' => Some("gh"),
        'ङ' => Some("ng"),
        'च' => Some("ch"),
        'छ' => Some("chh"),
        'ज' => Some("j"),
        'झ' => Some("jh"),
        'ञ' => Some("ny"),
        'ट' => Some("t"),
        'ठ' => Some("th"),
        'ड' => Some("d"),
        'ढ' => Some("dh"),
        'ण' => Some("n"),
        'त' => Some("t"),
        'थ' => Some("th"),
        'द' => Some("d"),
        'ध' => Some("dh"),
        'न' => Some("n"),
        'प' => Some("p"),
        'फ' => Some("ph"),
        'ब' => Some("b"),
        'भ' => Some("bh"),
        'म' => Some("m"),
        'य' => Some("y"),
        'र' => Some("r"),
        'ल' => Some("l"),
        'व' => Some("v"),
        'श' => Some("sh"),
        'ष' => Some("sh"),
        'स' => Some("s"),
        'ह' => Some("h"),
        '\u{0958}' => Some("q"),
        '\u{0959}' => Some("kh"),
        '\u{095A}' => Some("g"),
        '\u{095B}' => Some("z"),
        '\u{095C}' => Some("r"),
        '\u{095D}' => Some("rh"),
        '\u{095E}' => Some("f"),
        _ => None,
    }
}

fn gurmukhi_vowel(c: char) -> Option<&'static str> {
    match c {
        'ਅ' => Some("a"),
        'ਆ' => Some("aa"),
        'ਇ' => Some("i"),
        'ਈ' => Some("ee"),
        'ਉ' => Some("u"),
        'ਊ' => Some("oo"),
        'ਏ' => Some("e"),
        'ਐ' => Some("ai"),
        'ਓ' => Some("o"),
        'ਔ' => Some("au"),
        _ => None,
    }
}

fn gurmukhi_matra(c: char) -> Option<&'static str> {
    match c {
        'ਾ' => Some("aa"),
        'ਿ' => Some("i"),
        'ੀ' => Some("ee"),
        'ੁ' => Some("u"),
        'ੂ' => Some("oo"),
        'ੇ' => Some("e"),
        'ੈ' => Some("ai"),
        'ੋ' => Some("o"),
        'ੌ' => Some("au"),
        'ੰ' | 'ਂ' => Some("n"),
        _ => None,
    }
}

fn gurmukhi_consonant(c: char) -> Option<&'static str> {
    match c {
        'ਕ' => Some("k"),
        'ਖ' => Some("kh"),
        'ਗ' => Some("g"),
        'ਘ' => Some("gh"),
        'ਙ' => Some("ng"),
        'ਚ' => Some("ch"),
        'ਛ' => Some("chh"),
        'ਜ' => Some("j"),
        'ਝ' => Some("jh"),
        'ਞ' => Some("ny"),
        'ਟ' => Some("t"),
        'ਠ' => Some("th"),
        'ਡ' => Some("d"),
        'ਢ' => Some("dh"),
        'ਣ' => Some("n"),
        'ਤ' => Some("t"),
        'ਥ' => Some("th"),
        'ਦ' => Some("d"),
        'ਧ' => Some("dh"),
        'ਨ' => Some("n"),
        'ਪ' => Some("p"),
        'ਫ' => Some("ph"),
        'ਬ' => Some("b"),
        'ਭ' => Some("bh"),
        'ਮ' => Some("m"),
        'ਯ' => Some("y"),
        'ਰ' => Some("r"),
        'ਲ' => Some("l"),
        'ਵ' => Some("v"),
        'ੜ' => Some("r"),
        'ਸ' => Some("s"),
        'ਹ' => Some("h"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_latin_text() {
        assert!(is_latin_text("Mujhko itna bataaye koi"));
        assert!(is_latin_text("Hum tere bin ab reh nahi sakte"));
        assert!(!is_latin_text("मुझको इतना बताए कोई"));
        assert!(!is_latin_text("ਗੱਲ ਸੁਣ ਲ ਲਲਾਰੀਆਂ ਵੇ"));
    }

    #[test]
    fn test_transliterate_devanagari() {
        let hindi = "मुझको इतना बताए कोई";
        let latin = transliterate_indic(hindi);
        assert!(is_latin_text(&latin));
        assert!(latin.contains("mujh") || latin.contains("itan"));
    }

    #[test]
    fn test_transliterate_gurmukhi() {
        let punjabi = "ਗੱਲ ਸੁਣ ਲਲਾਰੀਆਂ";
        let latin = transliterate_indic(punjabi);
        assert!(is_latin_text(&latin));
        assert!(latin.contains("sun") || latin.contains("lalaari"));
    }
}
