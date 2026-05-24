use crate::graph::{PortDef, PortKind, PortValue};
use crate::node_trait::{NodeBehavior, RenderContext};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use eframe::egui;
use std::collections::HashMap;
use std::sync::Arc;

// ── TTS Synthesis Request (stored in egui temp data) ─────────────────────────

#[derive(Clone, Debug)]
pub struct TtsSynthRequest {
    pub node_id: crate::graph::NodeId,
    pub text: String,
    pub model_path: String,
    pub config_path: String,
    pub speaker_id: i64,
    pub noise_scale: f32,
    pub length_scale: f32,  // 1.0 / speed
    pub noise_scale_w: f32,
    pub engine_sample_rate: f32,
    /// espeak voice name read from .onnx.json (e.g. "en-us", "en-gb")
    pub espeak_voice: String,
}

// ── TTS Synthesis Result (sent back via mpsc) ────────────────────────────────

#[derive(Debug)]
pub struct TtsSynthResult {
    pub node_id: crate::graph::NodeId,
    pub status: String,
    pub success: bool,
}

// ── Piper config JSON (partial) ──────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct PiperAudioConfig {
    sample_rate: u32,
}

#[derive(Debug, Deserialize, Default)]
struct PiperEspeakConfig {
    /// e.g. "en-us", "en-gb", "de", "fr" — matches the espeak-ng voice name
    #[serde(default)]
    voice: String,
}

#[derive(Debug, Deserialize)]
struct PiperConfig {
    audio: PiperAudioConfig,
    #[serde(default)]
    phoneme_type: String,
    #[serde(default)]
    num_speakers: u32,
    #[serde(default)]
    phoneme_id_map: HashMap<String, Vec<i64>>,
    #[serde(default)]
    speaker_id_map: HashMap<String, i64>,
    /// espeak block — contains the voice name used during training
    #[serde(default)]
    espeak: PiperEspeakConfig,
}

// ── Phonemization ────────────────────────────────────────────────────────────

fn text_to_phoneme_ids(
    text: &str,
    phoneme_type: &str,
    phoneme_id_map: &HashMap<String, Vec<i64>>,
    espeak_voice: &str,
) -> Result<Vec<i64>, String> {
    // BOS marker
    let bos_ids: Vec<i64> = phoneme_id_map.get("^")
        .or_else(|| phoneme_id_map.get("BOS"))
        .cloned()
        .unwrap_or_default();
    // EOS marker
    let eos_ids: Vec<i64> = phoneme_id_map.get("$")
        .or_else(|| phoneme_id_map.get("EOS"))
        .cloned()
        .unwrap_or_default();
    // Word boundary
    let space_ids: Vec<i64> = phoneme_id_map.get(" ")
        .cloned()
        .unwrap_or_default();

    // Helper: convert an IPA string to phoneme IDs using the model's map
    let ipa_to_ids = |ipa: &str, bos: &[i64], eos: &[i64], space: &[i64]| -> Vec<i64> {
        let mut ids = bos.to_vec();
        for line in ipa.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                ids.extend_from_slice(space);
                continue;
            }
            for ch in trimmed.chars() {
                if ch.is_whitespace() {
                    ids.extend_from_slice(space);
                    continue;
                }
                let key = ch.to_string();
                if let Some(ph_ids) = phoneme_id_map.get(&key) {
                    ids.extend_from_slice(ph_ids);
                }
            }
        }
        ids.extend_from_slice(eos);
        ids
    };

    match phoneme_type {
        "text" | "" => {
            let mut ids = bos_ids;
            for ch in text.chars() {
                let key = ch.to_string();
                if let Some(ch_ids) = phoneme_id_map.get(&key) {
                    ids.extend_from_slice(ch_ids);
                }
            }
            ids.extend(eos_ids);
            Ok(ids)
        }
        "espeak" => {
            // Try espeak-ng first (best quality) — fall back to built-in G2P
            let ipa = match try_espeak_ng(text, espeak_voice) {
                Ok(s) => s,
                Err(_) => {
                    // espeak-ng not installed → use built-in pure-Rust G2P
                    builtin_english_g2p(text)
                }
            };
            Ok(ipa_to_ids(&ipa, &bos_ids, &eos_ids, &space_ids))
        }
        other => Err(format!("Unknown phoneme_type: '{}' — expected 'text' or 'espeak'", other)),
    }
}

/// Try to call espeak-ng as a subprocess. Returns the raw IPA output on success.
fn try_espeak_ng(text: &str, voice: &str) -> Result<String, ()> {
    let voice = if voice.is_empty() { "en-us" } else { voice };
    let output = std::process::Command::new("espeak-ng")
        .args(["-q", "--ipa", "-v", voice])
        .arg(text)
        .output()
        .map_err(|_| ())?;
    if !output.status.success() { return Err(()); }
    // Strip ANSI escape codes and control chars that some platforms emit
    let raw = String::from_utf8_lossy(&output.stdout);
    let clean: String = raw.chars()
        .filter(|&c| c == '\n' || c == '\r' || c == ' ' || c == '\t'
            || (c as u32 >= 0x20 && c as u32 != 0x7F))
        .collect();
    Ok(clean)
}

/// Check whether espeak-ng is available on this system.
pub fn espeak_ng_available() -> bool {
    std::process::Command::new("espeak-ng")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

// ── Built-in pure-Rust English G2P ──────────────────────────────────────────
//
// Converts English text → espeak-compatible IPA without any system deps.
// Quality is good for common words (lookup table) and reasonable for others
// (rule-based). Not as accurate as espeak-ng for rare/complex words.

fn builtin_english_g2p(text: &str) -> String {
    let mut result = String::new();
    for (i, word) in text.split_whitespace().enumerate() {
        if i > 0 { result.push(' '); }
        // Strip punctuation for lookup/rules, but keep structure
        let clean: String = word.chars()
            .filter(|c| c.is_alphabetic())
            .collect::<String>()
            .to_lowercase();
        if clean.is_empty() { continue; }
        let ipa = lookup_word(&clean)
            .map(|s| s.to_string())
            .unwrap_or_else(|| rules_g2p(&clean));
        result.push_str(&ipa);
    }
    result
}

/// Common English words → IPA (espeak en-us style)
fn lookup_word(w: &str) -> Option<&'static str> {
    Some(match w {
        // Articles / determiners
        "the" => "ðə", "a" => "ə", "an" => "æn",
        // Conjunctions / prepositions
        "and" => "ænd", "or" => "ɔːɹ", "but" => "bʌt", "nor" => "nɔːɹ",
        "in" => "ɪn", "on" => "ɒn", "at" => "æt", "to" => "tuː",
        "of" => "əv", "for" => "fɔːɹ", "with" => "wɪð", "by" => "baɪ",
        "from" => "fɹɒm", "up" => "ʌp", "out" => "aʊt", "as" => "æz",
        "into" => "ɪntuː", "through" => "θɹuː", "over" => "oʊvɜːɹ",
        "after" => "æftɜːɹ", "before" => "bɪfɔːɹ", "between" => "bɪtwiːn",
        "under" => "ʌndɜːɹ", "about" => "əbaʊt", "against" => "əɡɛnst",
        "without" => "wɪðaʊt", "within" => "wɪðɪn", "along" => "əlɒŋ",
        "across" => "əkɹɒs", "behind" => "bɪhaɪnd", "beyond" => "bɪjɒnd",
        "during" => "djʊɹɪŋ", "near" => "nɪɹ", "off" => "ɒf",
        // Auxiliary verbs
        "is" => "ɪz", "are" => "ɑːɹ", "was" => "wɒz", "were" => "wɜːɹ",
        "be" => "biː", "been" => "biːn", "being" => "biːɪŋ",
        "have" => "hæv", "has" => "hæz", "had" => "hæd", "having" => "hævɪŋ",
        "do" => "duː", "does" => "dʌz", "did" => "dɪd", "done" => "dʌn",
        "will" => "wɪl", "would" => "wʊd", "shall" => "ʃæl", "should" => "ʃʊd",
        "can" => "kæn", "could" => "kʊd", "may" => "meɪ", "might" => "maɪt",
        "must" => "mʌst", "ought" => "ɔːt",
        // Negations
        "not" => "nɒt", "no" => "noʊ", "never" => "nɛvɜːɹ", "neither" => "niːðɜːɹ",
        "nothing" => "nʌθɪŋ", "nobody" => "noʊbɒdi", "none" => "nʌn",
        // Pronouns
        "i" => "aɪ", "you" => "juː", "he" => "hiː", "she" => "ʃiː",
        "it" => "ɪt", "we" => "wiː", "they" => "ðeɪ",
        "me" => "miː", "him" => "hɪm", "her" => "hɜːɹ",
        "us" => "ʌs", "them" => "ðɛm",
        "my" => "maɪ", "your" => "jɔːɹ", "his" => "hɪz", "its" => "ɪts",
        "our" => "aʊɹ", "their" => "ðɛɹ",
        "myself" => "maɪsɛlf", "yourself" => "jɔːɹsɛlf", "himself" => "hɪmsɛlf",
        "herself" => "hɜːɹsɛlf", "itself" => "ɪtsɛlf", "themselves" => "ðɛmsɛlvz",
        // Demonstratives / interrogatives
        "this" => "ðɪs", "that" => "ðæt", "these" => "ðiːz", "those" => "ðoʊz",
        "what" => "wɒt", "which" => "wɪtʃ", "who" => "huː", "whom" => "huːm",
        "whose" => "huːz", "how" => "haʊ", "when" => "wɛn",
        "where" => "wɛɹ", "why" => "waɪ", "whether" => "wɛðɜːɹ",
        // Common adverbs
        "all" => "ɔːl", "also" => "ɔːlsoʊ", "again" => "əɡɛn", "back" => "bæk",
        "here" => "hɪɹ", "there" => "ðɛɹ", "just" => "dʒʌst", "very" => "vɛɹi",
        "more" => "mɔːɹ", "most" => "moʊst", "much" => "mʌtʃ", "many" => "mɛni",
        "even" => "iːvən", "only" => "oʊnli", "well" => "wɛl", "still" => "stɪl",
        "then" => "ðɛn", "now" => "naʊ", "already" => "ɔːlɹɛdi",
        "always" => "ɔːlweɪz", "often" => "ɒfən", "sometimes" => "sʌmtaɪmz",
        "really" => "ɹɪəli", "quite" => "kwaɪt",
        "rather" => "ɹæðɜːɹ", "almost" => "ɔːlmoʊst", "enough" => "ɪnʌf",
        "too" => "tuː", "so" => "soʊ", "yet" => "jɛt", "soon" => "suːn",
        "once" => "wʌns", "twice" => "twaɪs", "together" => "təɡɛðɜːɹ",
        "away" => "əweɪ", "ever" => "ɛvɜːɹ", "instead" => "ɪnstɛd",
        "perhaps" => "pɜːhæps", "maybe" => "meɪbi", "probably" => "pɹɒbəbli",
        "certainly" => "sɜːtənli", "clearly" => "klɪɹli", "finally" => "faɪnəli",
        "actually" => "æktʃuəli", "usually" => "juːʒuəli", "especially" => "ɪspɛʃəli",
        // Numbers
        "zero" => "zɪɹoʊ", "one" => "wʌn", "two" => "tuː", "three" => "θɹiː",
        "four" => "fɔːɹ", "five" => "faɪv", "six" => "sɪks", "seven" => "sɛvən",
        "eight" => "eɪt", "nine" => "naɪn", "ten" => "tɛn",
        "eleven" => "ɪlɛvən", "twelve" => "twɛlv", "thirteen" => "θɜːtiːn",
        "fourteen" => "fɔːɹtiːn", "fifteen" => "fɪftiːn", "sixteen" => "sɪkstiːn",
        "seventeen" => "sɛvəntiːn", "eighteen" => "eɪtiːn", "nineteen" => "naɪntiːn",
        "twenty" => "twɛnti", "thirty" => "θɜːti", "forty" => "fɔːɹti",
        "fifty" => "fɪfti", "sixty" => "sɪksti", "seventy" => "sɛvənti",
        "eighty" => "eɪti", "ninety" => "naɪnti", "hundred" => "hʌndɹəd",
        "thousand" => "θaʊzənd", "million" => "mɪljən", "billion" => "bɪljən",
        "first" => "fɜːst", "second" => "sɛkənd", "third" => "θɜːd",
        "fourth" => "fɔːɹθ", "fifth" => "fɪfθ", "last" => "læst",
        // Greetings / common phrases
        "hello" => "həloʊ", "hi" => "haɪ", "hey" => "heɪ", "bye" => "baɪ",
        "goodbye" => "ɡʊdbaɪ", "yes" => "jɛs", "yeah" => "jɛə",
        "ok" => "oʊkeɪ", "okay" => "oʊkeɪ", "please" => "pliːz",
        "thank" => "θæŋk", "thanks" => "θæŋks", "sorry" => "sɒɹi",
        "welcome" => "wɛlkəm",
        // Common nouns
        "world" => "wɜːld", "time" => "taɪm", "year" => "jɪɹ", "day" => "deɪ",
        "way" => "weɪ", "man" => "mæn", "woman" => "wʊmən", "child" => "tʃaɪld",
        "life" => "laɪf", "hand" => "hænd", "part" => "pɑːɹt", "place" => "pleɪs",
        "case" => "keɪs", "week" => "wiːk", "company" => "kʌmpəni",
        "system" => "sɪstəm", "program" => "pɹoʊɡɹæm", "question" => "kwɛstʃən",
        "work" => "wɜːk", "government" => "ɡʌvɜːnmənt", "number" => "nʌmbɜːɹ",
        "night" => "naɪt", "point" => "pɔɪnt", "home" => "hoʊm", "water" => "wɔːtɜːɹ",
        "room" => "ɹuːm", "mother" => "mʌðɜːɹ", "area" => "ɛɹɪə",
        "money" => "mʌni", "story" => "stɔːɹi", "fact" => "fækt",
        "month" => "mʌnθ", "lot" => "lɒt", "right" => "ɹaɪt", "study" => "stʌdi",
        "book" => "bʊk", "eye" => "aɪ", "job" => "dʒɒb", "word" => "wɜːd",
        "business" => "bɪznɪs", "issue" => "ɪʃuː", "side" => "saɪd",
        "kind" => "kaɪnd", "head" => "hɛd", "house" => "haʊs", "service" => "sɜːvɪs",
        "friend" => "fɹɛnd", "father" => "fɑːðɜːɹ", "power" => "paʊɜːɹ",
        "hour" => "aʊɜːɹ", "game" => "ɡeɪm", "line" => "laɪn", "end" => "ɛnd",
        "among" => "əmʌŋ", "example" => "ɪɡzæmpəl",
        "family" => "fæməli", "group" => "ɡɹuːp", "problem" => "pɹɒbləm",
        "city" => "sɪti", "community" => "kəmjuːnɪti", "name" => "neɪm",
        "president" => "pɹɛzɪdənt", "team" => "tiːm", "minute" => "mɪnɪt",
        "idea" => "aɪdɪə", "body" => "bɒdi", "information" => "ɪnfɜːmeɪʃən",
        "parent" => "pɛɹənt", "face" => "feɪs",
        "others" => "ʌðɜːɹz", "level" => "lɛvəl", "office" => "ɒfɪs",
        "door" => "dɔːɹ", "health" => "hɛlθ", "person" => "pɜːɹsən",
        "art" => "ɑːɹt", "war" => "wɔːɹ", "history" => "hɪstɜːɹi",
        "party" => "pɑːɹti", "result" => "ɹɪzʌlt", "change" => "tʃeɪndʒ",
        "morning" => "mɔːɹnɪŋ", "reason" => "ɹiːzən", "research" => "ɹɪsɜːtʃ",
        "girl" => "ɡɜːl", "guy" => "ɡaɪ", "moment" => "moʊmənt",
        "air" => "ɛɹ", "teacher" => "tiːtʃɜːɹ", "force" => "fɔːɹs",
        "education" => "ɛdʒukeɪʃən", "nation" => "neɪʃən", "car" => "kɑːɹ",
        "record" => "ɹɛkɜːd", "type" => "taɪp", "position" => "pəzɪʃən",
        "rate" => "ɹeɪt", "food" => "fuːd", "economy" => "ɪkɒnəmi",
        "value" => "væljuː", "control" => "kəntɹoʊl", "process" => "pɹɒsɛs",
        "data" => "deɪtə", "cost" => "kɒst", "plan" => "plæn",
        // Common adjectives
        "good" => "ɡʊd", "new" => "njuː", "old" => "oʊld", "great" => "ɡɹeɪt",
        "big" => "bɪɡ", "small" => "smɔːl", "large" => "lɑːɹdʒ",
        "long" => "lɒŋ", "little" => "lɪtəl", "own" => "oʊn",
        "other" => "ʌðɜːɹ", "same" => "seɪm", "different" => "dɪfɹənt",
        "high" => "haɪ", "low" => "loʊ", "next" => "nɛkst",
        "early" => "ɜːli", "young" => "jʌŋ", "important" => "ɪmpɔːɹtənt",
        "few" => "fjuː", "public" => "pʌblɪk", "bad" => "bæd",
        "social" => "soʊʃəl", "free" => "fɹiː", "real" => "ɹɪəl",
        "best" => "bɛst", "sure" => "ʃʊɹ", "hard" => "hɑːɹd",
        "true" => "tɹuː", "white" => "waɪt", "black" => "blæk",
        "red" => "ɹɛd", "blue" => "bluː", "green" => "ɡɹiːn",
        "full" => "fʊl", "special" => "spɛʃəl", "easy" => "iːzi",
        "clear" => "klɪɹ", "recent" => "ɹiːsənt", "certain" => "sɜːtən",
        "open" => "oʊpən", "possible" => "pɒsɪbəl", "whole" => "hoʊl",
        "common" => "kɒmən", "poor" => "pʊɹ", "natural" => "nætʃɹəl",
        "significant" => "sɪɡnɪfɪkənt", "similar" => "sɪmɪlɜːɹ",
        "hot" => "hɒt", "cold" => "koʊld", "happy" => "hæpi",
        "beautiful" => "bjuːtɪfəl", "ready" => "ɹɛdi", "nice" => "naɪs",
        "strong" => "stɹɒŋ", "late" => "leɪt", "wrong" => "ɹɒŋ",
        "simple" => "sɪmpəl", "able" => "eɪbəl", "local" => "loʊkəl",
        "political" => "pəlɪtɪkəl", "financial" => "faɪnænʃəl",
        "deep" => "diːp", "dark" => "dɑːɹk", "dead" => "dɛd",
        "light" => "laɪt", "rich" => "ɹɪtʃ",
        "short" => "ʃɔːɹt", "wide" => "waɪd", "fast" => "fæst",
        "slow" => "sloʊ", "safe" => "seɪf", "smart" => "smɑːɹt",
        "funny" => "fʌni", "fine" => "faɪn", "fair" => "fɛɹ",
        // Common verbs
        "like" => "laɪk", "know" => "noʊ", "make" => "meɪk", "say" => "seɪ",
        "get" => "ɡɛt", "go" => "ɡoʊ", "see" => "siː", "come" => "kʌm",
        "think" => "θɪŋk", "look" => "lʊk", "want" => "wɒnt", "give" => "ɡɪv",
        "use" => "juːz", "find" => "faɪnd", "tell" => "tɛl", "ask" => "æsk",
        "seem" => "siːm", "feel" => "fiːl", "try" => "tɹaɪ", "leave" => "liːv",
        "call" => "kɔːl", "keep" => "kiːp", "let" => "lɛt", "begin" => "bɪɡɪn",
        "show" => "ʃoʊ", "hear" => "hɪɹ", "play" => "pleɪ", "run" => "ɹʌn",
        "move" => "muːv", "live" => "lɪv", "believe" => "bɪliːv",
        "hold" => "hoʊld", "bring" => "bɹɪŋ", "happen" => "hæpən",
        "write" => "ɹaɪt", "sit" => "sɪt", "stand" => "stænd",
        "lose" => "luːz", "pay" => "peɪ", "meet" => "miːt",
        "set" => "sɛt", "learn" => "lɜːn", "lead" => "liːd",
        "understand" => "ʌndɜːstænd", "watch" => "wɒtʃ", "follow" => "fɒloʊ",
        "stop" => "stɒp", "create" => "kɹieɪt", "speak" => "spiːk",
        "read" => "ɹiːd", "spend" => "spɛnd", "grow" => "ɡɹoʊ",
        "walk" => "wɔːk", "win" => "wɪn", "offer" => "ɒfɜːɹ",
        "remember" => "ɹɪmɛmbɜːɹ", "love" => "lʌv", "consider" => "kənsɪdɜːɹ",
        "appear" => "əpɪɹ", "buy" => "baɪ", "wait" => "weɪt",
        "serve" => "sɜːv", "die" => "daɪ", "send" => "sɛnd",
        "expect" => "ɪkspɛkt", "build" => "bɪld", "stay" => "steɪ",
        "fall" => "fɔːl", "cut" => "kʌt", "reach" => "ɹiːtʃ",
        "remain" => "ɹɪmeɪn", "suggest" => "səɡdʒɛst", "raise" => "ɹeɪz",
        "pass" => "pæs", "sell" => "sɛl", "require" => "ɹɪkwaɪɜːɹ",
        "report" => "ɹɪpɔːɹt", "decide" => "dɪsaɪd", "pull" => "pʊl",
        "break" => "bɹeɪk", "carry" => "kæɹi",
        "include" => "ɪnkluːd", "continue" => "kəntɪnjuː",
        "add" => "æd", "eat" => "iːt", "sleep" => "sliːp", "help" => "hɛlp",
        "start" => "stɑːɹt", "turn" => "tɜːn", "drive" => "dɹaɪv",
        "close" => "kloʊz", "return" => "ɹɪtɜːn",
        "need" => "niːd", "put" => "pʊt", "take" => "teɪk",
        "hit" => "hɪt", "join" => "dʒɔɪn",
        "complete" => "kəmpliːt", "check" => "tʃɛk", "save" => "seɪv",
        "connect" => "kənɛkt", "share" => "ʃɛɹ", "enable" => "ɪneɪbəl",
        "press" => "pɹɛs", "enter" => "ɛntɜːɹ", "select" => "sɪlɛkt",
        "load" => "loʊd", "launch" => "lɔːntʃ", "click" => "klɪk",
        "manage" => "mænɪdʒ", "update" => "ʌpdeɪt", "install" => "ɪnstɔːl",
        "download" => "daʊnloʊd", "upload" => "ʌploʊd", "delete" => "dɪliːt",
        "search" => "sɜːtʃ", "edit" => "ɛdɪt", "copy" => "kɒpi", "paste" => "peɪst",
        "sort" => "sɔːɹt", "filter" => "fɪltɜːɹ", "reset" => "ɹɪsɛt",
        "submit" => "səbmɪt", "cancel" => "kænsəl", "confirm" => "kənfɜːm",
        _ => return None,
    })
}

/// Rule-based G2P for words not in the lookup table.
/// Handles common English letter-to-sound patterns → espeak-compatible IPA.
fn rules_g2p(word: &str) -> String {
    let chars: Vec<char> = word.chars().collect();
    let n = chars.len();
    let mut ipa = String::new();
    let mut i = 0;

    while i < n {
        let c = chars[i];
        let p = if i > 0 { Some(chars[i - 1]) } else { None };
        let nx = chars.get(i + 1).copied();
        let nx2 = chars.get(i + 2).copied();

        let is_vowel_c = |c: char| matches!(c, 'a' | 'e' | 'i' | 'o' | 'u');
        let next_vowel = nx.map_or(false, is_vowel_c);
        let next_cons  = nx.map_or(false, |c| !is_vowel_c(c));

        // ── Multi-char sequences first ────────────────────────────────
        // Three-char
        if nx.is_some() && nx2.is_some() {
            match (c, nx.unwrap(), nx2.unwrap()) {
                ('t', 'i', 'o') => { ipa.push_str("ʃ"); i += 3; continue; } // -tion
                ('s', 'i', 'o') => { ipa.push_str("ʒ"); i += 3; continue; } // -sion
                _ => {}
            }
        }

        // Two-char digraphs
        if let Some(nx_c) = nx {
            let skip2 = match (c, nx_c) {
                ('c', 'h') => Some("tʃ"),
                ('s', 'h') => Some("ʃ"),
                ('t', 'h') => Some(if i == 0 || matches!(p, Some('t'|'s')) { "θ" } else { "ð" }),
                ('w', 'h') => Some("w"),
                ('p', 'h') => Some("f"),
                ('c', 'k') => Some("k"),
                ('q', 'u') => Some("kw"),
                ('g', 'n') if i == 0 => Some("n"),   // gn- silent g
                ('k', 'n') if i == 0 => Some("n"),   // kn- silent k
                ('w', 'r') if i == 0 => Some("ɹ"),   // wr- silent w
                ('n', 'g') if !next_vowel || i + 2 >= n => Some("ŋ"),
                ('g', 'h') => Some(""),               // usually silent
                ('a', 'i') | ('a', 'y') => Some("eɪ"),
                ('e', 'a') => Some("iː"),
                ('e', 'e') => Some("iː"),
                ('o', 'a') => Some("oʊ"),
                ('o', 'o') => {
                    // oo → uː usually, but foot/good → ʊ
                    Some("uː")
                }
                ('o', 'u') => Some("aʊ"),
                ('o', 'w') => Some("oʊ"),
                ('a', 'w') => Some("ɔː"),
                ('a', 'u') => Some("ɔː"),
                ('e', 'w') => Some("juː"),
                ('e', 'y') => Some("eɪ"),
                ('i', 'e') => Some("iː"),
                ('u', 'e') => Some("uː"),
                ('i', 'r') => Some("ɜːɹ"),
                ('e', 'r') => Some("ɜːɹ"),
                ('u', 'r') => Some("ɜːɹ"),
                ('o', 'r') => Some("ɔːɹ"),
                ('a', 'r') => Some("ɑːɹ"),
                _ => None,
            };
            if let Some(ph) = skip2 {
                ipa.push_str(ph);
                i += 2;
                continue;
            }
        }

        // ── Single character ──────────────────────────────────────────
        let ph: &str = match c {
            // Consonants
            'b' => "b",
            'c' => {
                if matches!(nx, Some('e' | 'i' | 'y')) { "s" } else { "k" }
            }
            'd' => "d",
            'f' => "f",
            'g' => {
                if matches!(nx, Some('e' | 'i' | 'y')) { "dʒ" } else { "ɡ" }
            }
            'h' => "h",
            'j' => "dʒ",
            'k' => "k",
            'l' => "l",
            'm' => "m",
            'n' => "n",
            'p' => "p",
            'r' => "ɹ",
            's' => {
                // s between vowels is often /z/
                if matches!(p, Some('a'|'e'|'i'|'o'|'u')) && next_vowel { "z" } else { "s" }
            }
            't' => "t",
            'v' => "v",
            'w' => "w",
            'x' => "ks",
            'y' => {
                if i == 0 || matches!(p, Some(' ')) { "j" } else { "i" }
            }
            'z' => "z",

            // Vowels — simplified patterns
            'a' => {
                // CVCe: vowel before single consonant then final 'e' → long
                let cvc_e = next_cons
                    && nx2 == Some('e')
                    && i + 3 >= n;
                if cvc_e { "eɪ" } else if next_vowel { "eɪ" } else { "æ" }
            }
            'e' => {
                if i + 1 >= n { "" }  // silent final 'e'
                else if next_cons { "ɛ" } else { "iː" }
            }
            'i' => {
                let cvc_e = next_cons && nx2 == Some('e') && i + 3 >= n;
                if cvc_e { "aɪ" } else if next_vowel { "aɪ" } else { "ɪ" }
            }
            'o' => {
                let cvc_e = next_cons && nx2 == Some('e') && i + 3 >= n;
                if cvc_e { "oʊ" } else if next_vowel { "oʊ" } else { "ɒ" }
            }
            'u' => {
                let cvc_e = next_cons && nx2 == Some('e') && i + 3 >= n;
                if cvc_e { "juː" } else if next_vowel { "uː" } else { "ʌ" }
            }

            _ => "",
        };

        ipa.push_str(ph);
        i += 1;
    }

    ipa
}

// ── Synthesis thread ─────────────────────────────────────────────────────────

pub fn run_tts_synthesis(
    buffer: Arc<crate::audio::buffers::FilePlayerBuffer>,
    req: TtsSynthRequest,
) -> TtsSynthResult {
    let fail = |msg: String| TtsSynthResult {
        node_id: req.node_id,
        status: msg,
        success: false,
    };

    // 1. Parse config JSON
    let config_str = match std::fs::read_to_string(&req.config_path) {
        Ok(s) => s,
        Err(e) => return fail(format!("Config read error: {}", e)),
    };
    let config: PiperConfig = match serde_json::from_str(&config_str) {
        Ok(c) => c,
        Err(e) => return fail(format!("Config parse error: {}", e)),
    };

    let model_sr = config.audio.sample_rate as f32;
    let phoneme_type = if config.phoneme_type.is_empty() {
        "espeak".to_string()
    } else {
        config.phoneme_type.clone()
    };

    // Espeak voice from config (prefer config over request field as source of truth)
    let espeak_voice = if !config.espeak.voice.is_empty() {
        config.espeak.voice.clone()
    } else if !req.espeak_voice.is_empty() {
        req.espeak_voice.clone()
    } else {
        "en-us".to_string()
    };

    // 2. Phonemize
    let phoneme_ids = match text_to_phoneme_ids(&req.text, &phoneme_type, &config.phoneme_id_map, &espeak_voice) {
        Ok(ids) => ids,
        Err(e) => return fail(format!("Phonemization error: {}", e)),
    };

    if phoneme_ids.is_empty() {
        return fail("No phoneme IDs — text may be empty or all unknown characters".to_string());
    }

    let n = phoneme_ids.len();

    // 3. Build ONNX tensors. Cached session: avoids per-inference rebuild of
    // the (large) TTS model — was reloading on every utterance.
    let session = match crate::ml::session_cache::get_file_session(&req.model_path) {
        Ok(s) => s,
        Err(e) => return fail(format!("ONNX load error: {}", e)),
    };

    // input: shape [1, N] int64
    let input_tensor = match ort::value::Tensor::<i64>::from_array((
        vec![1usize, n],
        phoneme_ids.into_boxed_slice(),
    )) {
        Ok(t) => t,
        Err(e) => return fail(format!("Input tensor error: {}", e)),
    };

    // input_lengths: shape [1] int64
    let lengths_tensor = match ort::value::Tensor::<i64>::from_array((
        vec![1usize],
        vec![n as i64].into_boxed_slice(),
    )) {
        Ok(t) => t,
        Err(e) => return fail(format!("Lengths tensor error: {}", e)),
    };

    // scales: [noise_scale, length_scale, noise_scale_w] shape [3] f32
    let scales_tensor = match ort::value::Tensor::<f32>::from_array((
        vec![3usize],
        vec![req.noise_scale, req.length_scale, req.noise_scale_w].into_boxed_slice(),
    )) {
        Ok(t) => t,
        Err(e) => return fail(format!("Scales tensor error: {}", e)),
    };

    // 4. Run inference + extract waveform inside the session lock scope.
    //    `outputs` borrows from the locked session, so we extract the waveform
    //    into an owned `Vec<f32>` before dropping the guard.
    let waveform: Vec<f32> = {
        let mut sess = match session.lock() {
            Ok(g) => g,
            Err(_) => return fail("TTS session lock poisoned".to_string()),
        };
        let outputs = if config.num_speakers > 1 {
            let sid_tensor = match ort::value::Tensor::<i64>::from_array((
                vec![1usize],
                vec![req.speaker_id].into_boxed_slice(),
            )) {
                Ok(t) => t,
                Err(e) => return fail(format!("Speaker ID tensor error: {}", e)),
            };
            match sess.run(ort::inputs![
                "input" => input_tensor,
                "input_lengths" => lengths_tensor,
                "scales" => scales_tensor,
                "sid" => sid_tensor,
            ]) {
                Ok(o) => o,
                Err(e) => return fail(format!("ONNX inference error: {}", e)),
            }
        } else {
            match sess.run(ort::inputs![
                "input" => input_tensor,
                "input_lengths" => lengths_tensor,
                "scales" => scales_tensor,
            ]) {
                Ok(o) => o,
                Err(e) => return fail(format!("ONNX inference error (no sid): {}", e)),
            }
        };

        // Extract waveform — output[0] shape [1, 1, T]
        let (_shape, data) = match outputs[0].try_extract_tensor::<f32>() {
            Ok(t) => t,
            Err(e) => return fail(format!("Output extraction error: {}", e)),
        };
        data.to_vec()
    };

    if waveform.is_empty() {
        return fail("Empty waveform from model".to_string());
    }

    // 6. Reset buffer state so a second synthesis starts from the beginning
    //    (frac_read_pos is UnsafeCell, reset() zeroes it safely from the write side)
    buffer.reset();
    // Set playback rate for sample-rate conversion (model_sr → engine_sr)
    let rate_x1000 = ((model_sr / req.engine_sample_rate) * 1000.0).round() as u32;
    buffer.playback_rate_x1000.store(rate_x1000, std::sync::atomic::Ordering::Relaxed);
    buffer.total_samples.store(waveform.len(), std::sync::atomic::Ordering::Relaxed);

    // 7. Write samples into ring buffer in chunks
    const CHUNK: usize = 4096;
    for chunk in waveform.chunks(CHUNK) {
        if buffer.stop_requested.load(std::sync::atomic::Ordering::Relaxed) {
            return TtsSynthResult {
                node_id: req.node_id,
                status: "Stopped".to_string(),
                success: false,
            };
        }
        buffer.write(chunk);
        // Small yield to avoid starving audio callback
        std::thread::yield_now();
    }

    // 8. Mark finished
    buffer.finished.store(true, std::sync::atomic::Ordering::Release);

    TtsSynthResult {
        node_id: req.node_id,
        status: format!("Done ({:.1}s)", waveform.len() as f32 / model_sr),
        success: true,
    }
}

// ── TTS Node ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TtsNode {
    pub model_path: String,
    pub config_path: String,
    pub speaker_id: u32,
    pub speed: f32,
    pub noise_scale: f32,
    pub noise_scale_w: f32,
    pub status: String,

    // Runtime — not serialized
    #[serde(skip)]
    pub last_trigger: f32,
    #[serde(skip)]
    pub synthesis_done: bool,
    #[serde(skip)]
    pub has_audio: bool,  // true once synthesis has completed at least once
    // Detected config info (read from .onnx.json)
    #[serde(skip)]
    pub detected_phoneme_type: String,
    #[serde(skip)]
    pub detected_sample_rate: u32,
    #[serde(skip)]
    pub detected_num_speakers: u32,
    #[serde(skip)]
    pub detected_espeak_voice: String,
}

impl Default for TtsNode {
    fn default() -> Self {
        Self {
            model_path: String::new(),
            config_path: String::new(),
            speaker_id: 0,
            speed: 1.0,
            noise_scale: 0.667,
            noise_scale_w: 0.8,
            status: "No model loaded".to_string(),
            last_trigger: 0.0,
            synthesis_done: false,
            has_audio: false,
            detected_phoneme_type: String::new(),
            detected_sample_rate: 0,
            detected_num_speakers: 0,
            detected_espeak_voice: String::new(),
        }
    }
}

impl TtsNode {
    /// Auto-detect config from model path (.onnx → .onnx.json)
    fn auto_detect_config(&self) -> Option<String> {
        if self.model_path.is_empty() { return None; }
        let candidate = format!("{}.json", self.model_path);
        if std::path::Path::new(&candidate).exists() {
            return Some(candidate);
        }
        // Also try replacing .onnx extension
        if self.model_path.ends_with(".onnx") {
            let base = &self.model_path[..self.model_path.len() - 5];
            let candidate2 = format!("{}.onnx.json", base);
            if std::path::Path::new(&candidate2).exists() {
                return Some(candidate2);
            }
        }
        None
    }

    /// Load and cache config metadata (phoneme_type, sample_rate, num_speakers, espeak_voice)
    fn refresh_config_info(&mut self) {
        if self.config_path.is_empty() { return; }
        if let Ok(s) = std::fs::read_to_string(&self.config_path) {
            if let Ok(cfg) = serde_json::from_str::<PiperConfig>(&s) {
                self.detected_phoneme_type = if cfg.phoneme_type.is_empty() {
                    "espeak".to_string()
                } else {
                    cfg.phoneme_type
                };
                self.detected_sample_rate = cfg.audio.sample_rate;
                self.detected_num_speakers = cfg.num_speakers;
                self.detected_espeak_voice = cfg.espeak.voice.clone();
            }
        }
    }
}

impl NodeBehavior for TtsNode {
    fn title(&self) -> &str { "TTS" }
    fn inline_ports(&self) -> bool { true }

    fn inputs(&self) -> Vec<PortDef> {
        vec![
            PortDef { name: "Text".into(), kind: PortKind::Text },
            PortDef { name: "Trigger".into(), kind: PortKind::Trigger },
            PortDef { name: "Speed".into(), kind: PortKind::Number },
        ]
    }

    fn outputs(&self) -> Vec<PortDef> {
        vec![
            PortDef { name: "Audio".into(), kind: PortKind::Audio },
            PortDef { name: "Done".into(), kind: PortKind::Trigger },
        ]
    }

    fn color_hint(&self) -> [u8; 3] { [80, 130, 200] }

    fn min_width(&self) -> Option<f32> { Some(260.0) }

    fn type_tag(&self) -> &str { "tts" }

    fn save_state(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }

    fn load_state(&mut self, state: &Value) {
        if let Ok(s) = serde_json::from_value::<TtsNode>(state.clone()) {
            self.model_path = s.model_path;
            self.config_path = s.config_path;
            self.speaker_id = s.speaker_id;
            self.speed = s.speed;
            self.noise_scale = s.noise_scale;
            self.noise_scale_w = s.noise_scale_w;
            // Refresh cached config info
            self.refresh_config_info();
        }
    }

    fn evaluate(&mut self, inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        // Read speed from port 2 if wired
        if let Some(PortValue::Float(s)) = inputs.get(2) {
            if *s > 0.0 { self.speed = *s; }
        }

        // Trigger detection: rising edge on port 1
        let trigger_val = match inputs.get(1) {
            Some(PortValue::Float(v)) => *v,
            _ => 0.0,
        };
        let _rising_edge = trigger_val > 0.5 && self.last_trigger <= 0.5;
        self.last_trigger = trigger_val;

        // Done port pulse (set by app/io after synthesis completes)
        let done_val = if self.synthesis_done {
            self.synthesis_done = false;
            PortValue::Float(1.0)
        } else {
            PortValue::Float(0.0)
        };

        vec![(1, done_val)]
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        let node_id = ctx.node_id;

        // ── Read result from app/io (written via egui temp data) ─────
        let result_id = egui::Id::new(("tts_result", node_id));
        if let Some((status, success)) = ui.ctx().data_mut(|d| {
            d.get_temp::<(String, bool)>(result_id)
        }) {
            ui.ctx().data_mut(|d| d.remove::<(String, bool)>(result_id));
            self.status = status;
            if success {
                self.synthesis_done = true;
                self.has_audio = true;
            }
        }

        // ── Model file picker ─────────────────────────────────────────
        ui.horizontal(|ui| {
            ui.label("Model:");
            if ui.button("Load .onnx").clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("Piper ONNX", &["onnx"])
                    .pick_file()
                {
                    self.model_path = path.display().to_string();
                    // Auto-detect config
                    if let Some(cfg) = self.auto_detect_config() {
                        self.config_path = cfg;
                        self.refresh_config_info();
                        self.status = "Config auto-detected".to_string();
                    } else {
                        self.status = "Load .onnx.json config".to_string();
                    }
                }
            }
        });
        if !self.model_path.is_empty() {
            let short = shorten_path(&self.model_path, 28);
            ui.label(egui::RichText::new(short).small().color(egui::Color32::LIGHT_GRAY));
        }

        // ── Config file picker ────────────────────────────────────────
        ui.horizontal(|ui| {
            ui.label("Config:");
            if ui.button("Load .json").clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("Piper Config", &["json"])
                    .pick_file()
                {
                    self.config_path = path.display().to_string();
                    self.refresh_config_info();
                    self.status = "Config loaded".to_string();
                }
            }
        });
        if !self.config_path.is_empty() {
            let short = shorten_path(&self.config_path, 28);
            ui.label(egui::RichText::new(short).small().color(egui::Color32::LIGHT_GRAY));
        }

        // ── Detected config info ──────────────────────────────────────
        if self.detected_sample_rate > 0 {
            ui.separator();
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(format!("{}Hz", self.detected_sample_rate)).small());
                if self.detected_phoneme_type == "espeak" {
                    let has_espeak = espeak_ng_available();
                    let voice_label = if self.detected_espeak_voice.is_empty() {
                        "en-us".to_string()
                    } else {
                        self.detected_espeak_voice.clone()
                    };
                    let (label, color) = if has_espeak {
                        (format!("• espeak-ng ({}) ✓", voice_label), egui::Color32::LIGHT_GREEN)
                    } else {
                        #[cfg(target_os = "windows")]
                        let hint = format!("• built-in G2P ({}) — install espeak-ng for better quality", voice_label);
                        #[cfg(not(target_os = "windows"))]
                        let hint = format!("• built-in G2P ({})", voice_label);
                        (hint, egui::Color32::YELLOW)
                    };
                    ui.label(egui::RichText::new(label).small().color(color));
                } else {
                    ui.label(egui::RichText::new(format!("• {}", self.detected_phoneme_type)).small());
                }
                if self.detected_num_speakers > 1 {
                    ui.label(egui::RichText::new(format!("• {} speakers", self.detected_num_speakers)).small());
                }
            });
        }

        ui.separator();

        // ── Speed slider ──────────────────────────────────────────────
        ui.horizontal(|ui| {
            ui.label("Speed:");
            ui.add(egui::Slider::new(&mut self.speed, 0.25..=3.0)
                .step_by(0.05)
                .suffix("x"));
        });

        // ── Speaker ID (only if multi-speaker) ───────────────────────
        if self.detected_num_speakers > 1 {
            ui.horizontal(|ui| {
                ui.label("Speaker:");
                ui.add(egui::DragValue::new(&mut self.speaker_id)
                    .range(0..=(self.detected_num_speakers - 1)));
            });
        }

        // ── Advanced params ───────────────────────────────────────────
        egui::CollapsingHeader::new("Advanced").default_open(false).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("Noise:");
                ui.add(egui::Slider::new(&mut self.noise_scale, 0.0..=1.5).step_by(0.01));
            });
            ui.horizontal(|ui| {
                ui.label("Noise W:");
                ui.add(egui::Slider::new(&mut self.noise_scale_w, 0.0..=1.5).step_by(0.01));
            });
        });

        ui.separator();

        // ── Resolve input values via connections ──────────────────────
        // ctx.values is keyed by (source_node_id, source_port) — the upstream
        // producer's output. To read what's arriving on our input port N,
        // we find the connection whose to_node==node_id && to_port==N, then
        // look up (from_node, from_port) in the values map.
        let resolve_input = |port: usize| -> PortValue {
            ctx.connections.iter()
                .find(|c| c.to_node == node_id && c.to_port == port)
                .and_then(|c| ctx.values.get(&(c.from_node, c.from_port)).cloned())
                .unwrap_or(PortValue::None)
        };

        // ── Synthesize button (manual trigger) ────────────────────────
        let has_model = !self.model_path.is_empty() && !self.config_path.is_empty();

        let text_str = match resolve_input(0) {
            PortValue::Text(t) => t,
            PortValue::Float(f) => f.to_string(),
            _ => String::new(),
        };

        let synth_enabled = has_model && !text_str.is_empty();
        ui.horizontal(|ui| {
            if ui.add_enabled(synth_enabled, egui::Button::new("⟳ Synthesize")).clicked() {
                let req = TtsSynthRequest {
                    node_id,
                    text: text_str.clone(),
                    model_path: self.model_path.clone(),
                    config_path: self.config_path.clone(),
                    speaker_id: self.speaker_id as i64,
                    noise_scale: self.noise_scale,
                    length_scale: 1.0 / self.speed.max(0.01),
                    noise_scale_w: self.noise_scale_w,
                    engine_sample_rate: 44100.0, // overridden in app/io
                    espeak_voice: self.detected_espeak_voice.clone(),
                };
                ui.ctx().data_mut(|d| {
                    d.insert_temp(egui::Id::new(("tts_request", node_id)), req);
                });
                self.status = "Synthesizing…".to_string();
            }

            // Play button — replays already-synthesized audio from the start
            if ui.add_enabled(self.has_audio, egui::Button::new("▶ Play")).clicked() {
                ui.ctx().data_mut(|d| {
                    d.insert_temp(egui::Id::new(("tts_play", node_id)), true);
                });
                self.status = "Playing…".to_string();
            }
        });

        // Trigger fires synthesis on rising edge
        let trigger_val = match resolve_input(1) {
            PortValue::Float(v) => v,
            _ => 0.0,
        };
        let rising_edge = trigger_val > 0.5 && self.last_trigger <= 0.5;
        self.last_trigger = trigger_val; // track here (evaluate() also tracks but render runs first)
        if rising_edge && has_model && !text_str.is_empty() {
            let req = TtsSynthRequest {
                node_id,
                text: text_str,
                model_path: self.model_path.clone(),
                config_path: self.config_path.clone(),
                speaker_id: self.speaker_id as i64,
                noise_scale: self.noise_scale,
                length_scale: 1.0 / self.speed.max(0.01),
                noise_scale_w: self.noise_scale_w,
                engine_sample_rate: 44100.0,
                espeak_voice: self.detected_espeak_voice.clone(),
            };
            ui.ctx().data_mut(|d| {
                d.insert_temp(egui::Id::new(("tts_request", node_id)), req);
            });
            self.status = "Synthesizing…".to_string();
        }

        // ── Status ────────────────────────────────────────────────────
        let status_color = if self.status.starts_with("Done") {
            egui::Color32::LIGHT_GREEN
        } else if self.status.starts_with("Synth") {
            egui::Color32::YELLOW
        } else if self.status.contains("error") || self.status.contains("Error") || self.status.contains("not found") {
            egui::Color32::LIGHT_RED
        } else {
            egui::Color32::GRAY
        };
        ui.label(egui::RichText::new(&self.status).small().color(status_color));
    }
}

fn shorten_path(path: &str, max_len: usize) -> String {
    if path.len() <= max_len {
        return path.to_string();
    }
    let p = std::path::Path::new(path);
    let file_name = p.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
    if file_name.len() >= max_len {
        format!("…{}", &file_name[file_name.len().saturating_sub(max_len - 1)..])
    } else {
        format!("…/{}", file_name)
    }
}

// ── Registration ─────────────────────────────────────────────────────────────

pub fn register(r: &mut crate::node_trait::NodeRegistryInner) {
    r.register("tts", |state| {
        let mut node = TtsNode::default();
        node.load_state(state);
        Box::new(node)
    });
}
