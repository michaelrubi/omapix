//! The byte-pair tokenizer of Qwen's language models (FLUX.2 klein's text
//! encoder is Qwen3), read from a Hugging Face `tokenizer.json`: text is
//! split into words, each word into bytes, and the pairs of pieces the
//! file lists are joined, the earliest listed first.

use std::collections::HashMap;
use std::path::Path;

use regex::Regex;
use serde_json::Value;

use crate::Result;

pub struct Tokenizer {
    /// Each piece's id.
    vocab: HashMap<String, u32>,
    /// The pairs that join, with how early each is listed and what it
    /// joins into.
    merges: HashMap<(u32, u32), (u32, u32)>,
    /// Tokens that stand for themselves, such as `<|im_start|>`.
    added: Vec<(String, u32)>,
    words: Regex,
}

impl Tokenizer {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    fn parse(text: &str) -> Result<Self> {
        let bad = |what: &str| what.to_owned();
        let json: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
        let vocab: HashMap<String, u32> = json["model"]["vocab"]
            .as_object()
            .ok_or_else(|| bad("no vocabulary"))?
            .iter()
            .filter_map(|(piece, id)| Some((piece.clone(), id.as_u64()? as u32)))
            .collect();
        let mut merges = HashMap::new();
        for (rank, pair) in json["model"]["merges"].as_array().ok_or_else(|| bad("no merges"))?.iter().enumerate() {
            // Either "a b" or ["a", "b"], depending on the file's age.
            let (a, b) = match pair {
                Value::String(pair) => pair.split_once(' '),
                pair => pair[0].as_str().zip(pair[1].as_str()),
            }
            .ok_or_else(|| bad("a merge that isn't a pair"))?;
            if let (Some(&ia), Some(&ib), Some(&joined)) = (vocab.get(a), vocab.get(b), vocab.get(&format!("{a}{b}"))) {
                merges.insert((ia, ib), (rank as u32, joined));
            }
        }
        let added = json["added_tokens"]
            .as_array()
            .map(|tokens| {
                tokens
                    .iter()
                    .filter_map(|t| Some((t["content"].as_str()?.to_owned(), t["id"].as_u64()? as u32)))
                    .collect()
            })
            .unwrap_or_default();
        // The file's own pattern, less its `\s+(?!\S)` (whitespace up to
        // the last before a word), which `regex` can't say: `ids` does it.
        let words = Regex::new(r"(?i:'s|'t|'re|'ve|'m|'ll|'d)|[^\r\n\p{L}\p{N}]?\p{L}+|\p{N}| ?[^\s\p{L}\p{N}]+[\r\n]*|\s*[\r\n]+|\s+")
            .map_err(|e| e.to_string())?;
        Ok(Self { vocab, merges, added, words })
    }

    /// `text` as token ids. The text is taken to be in Unicode's composed
    /// form (NFC) already, as typed text is.
    pub fn ids(&self, text: &str) -> Vec<u32> {
        let mut ids = Vec::new();
        let mut rest = text;
        while !rest.is_empty() {
            // The next added token, the longest if several start together.
            let next = self
                .added
                .iter()
                .filter_map(|(token, id)| Some((rest.find(token.as_str())?, token.len(), *id)))
                .min_by_key(|&(at, len, _)| (at, usize::MAX - len));
            let (plain, token) = match next {
                Some((at, len, id)) => (&rest[..at], Some((at + len, id))),
                None => (rest, None),
            };
            self.plain(plain, &mut ids);
            match token {
                Some((end, id)) => {
                    ids.push(id);
                    rest = &rest[end..];
                }
                None => break,
            }
        }
        ids
    }

    /// The id of a single token, such as `<|endoftext|>`.
    pub fn id(&self, token: &str) -> Option<u32> {
        self.added.iter().find(|(t, _)| t == token).map(|&(_, id)| id).or_else(|| self.vocab.get(token).copied())
    }

    /// Text with no added tokens in it.
    fn plain(&self, text: &str, ids: &mut Vec<u32>) {
        let mut at = 0;
        while let Some(found) = self.words.find_at(text, at) {
            let mut end = found.end();
            let word = found.as_str();
            // Whitespace before a word leaves its last character to the word.
            if end < text.len() && !word.ends_with(['\r', '\n']) && word.chars().all(char::is_whitespace) && word.chars().count() > 1 {
                end -= word.chars().next_back().map_or(0, char::len_utf8);
            }
            self.word(&text[found.start()..end], ids);
            at = end;
        }
    }

    fn word(&self, word: &str, ids: &mut Vec<u32>) {
        let mut pieces: Vec<u32> = word.bytes().filter_map(|b| self.vocab.get(byte_piece(b).encode_utf8(&mut [0; 4])).copied()).collect();
        while let Some((_, joined, pair)) = pieces
            .windows(2)
            .filter_map(|w| self.merges.get(&(w[0], w[1])).map(|&(rank, joined)| (rank, joined, (w[0], w[1]))))
            .min()
        {
            let mut i = 0;
            while i + 1 < pieces.len() {
                if (pieces[i], pieces[i + 1]) == pair {
                    pieces[i] = joined;
                    pieces.remove(i + 1);
                }
                i += 1;
            }
        }
        ids.extend(pieces);
    }
}

/// How the vocabulary writes a byte: printable ones as themselves, the
/// rest (a space is `Ġ`) from U+0100 on, as GPT-2 did.
fn byte_piece(byte: u8) -> char {
    let other = match byte {
        0..=32 => u32::from(byte),
        127..=160 => u32::from(byte) - 127 + 33,
        173 => 67,
        _ => return char::from(byte),
    };
    char::from_u32(256 + other).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny tokenizer.json: bytes, a few merges and one added token.
    fn tiny() -> Tokenizer {
        let pieces = ["a", "b", "c", "Ġ", "ab", "Ġab", "abc", "1", "Ċ", "ĠĠ", "!"];
        let vocab: Vec<String> = pieces.iter().enumerate().map(|(i, p)| format!("{p:?}: {i}")).collect();
        let json = format!(
            r#"{{"added_tokens": [{{"id": 100, "content": "<|x|>"}}],
                "model": {{"vocab": {{{}}}, "merges": ["a b", ["Ġ", "ab"], "ab c", "Ġ Ġ"]}}}}"#,
            vocab.join(", ")
        );
        Tokenizer::parse(&json).unwrap()
    }

    #[test]
    fn pairs_join_in_the_order_the_file_lists_them() {
        let t = tiny();
        // "a b" first, so "abc" is ab + c, then "ab c".
        assert_eq!(t.ids("abc"), [6]);
        // A word takes the space before it: Ġ + ab, not ab alone.
        assert_eq!(t.ids("ab ab"), [4, 5]);
        assert_eq!(t.ids("cab"), [2, 4]);
    }

    #[test]
    fn words_numbers_and_spaces_are_split_as_qwen_splits_them() {
        let t = tiny();
        // Digits one at a time, and each added token whole.
        assert_eq!(t.ids("11<|x|>a"), [7, 7, 100, 0]);
        // Of three spaces before a word, the last is the word's.
        assert_eq!(t.ids("a   ab"), [0, 9, 5]);
        // A line break ends its run of whitespace.
        assert_eq!(t.ids("a \nb!"), [0, 3, 8, 1, 10]);
        assert_eq!(t.id("<|x|>"), Some(100));
    }

    /// Needs the FLUX.2 model's tokenizer.json. The ids are what Hugging
    /// Face's `tokenizers` gives for it.
    #[test]
    #[ignore]
    fn qwens_tokenizer_gives_the_ids_hugging_faces_does() {
        let files = crate::find_model(crate::flux::MODEL).unwrap();
        let t = Tokenizer::load(&files["tokenizer.json"]).unwrap();
        let expected = [
            ("<|im_start|>user\na bare wall<|im_end|>\n<|im_start|>assistant\n<think>\n\n</think>\n\n", &[151644, 872, 198, 64, 12461, 7002, 151645, 198, 151644, 77091, 198, 151667, 271, 151668, 271][..]),
            ("Remove the person, and fill with more of the backdrop.", &[13021, 279, 1697, 11, 323, 5155, 448, 803, 315, 279, 38477, 13][..]),
            ("A red fox in snow, 35mm photo; it's 12:45pm!  Two  spaces\tand tabs\n\nnew paragraph   ", &[32, 2518, 38835, 304, 11794, 11, 220, 18, 20, 3821, 6548, 26, 432, 594, 220, 16, 17, 25, 19, 20, 5187, 0, 220, 9043, 220, 12621, 52477, 22398, 271, 931, 14311, 262][..]),
            ("don't I'LL café naïve — “quoted” 1234567 3.14 e-mail@x.org https://a.b/c?d=e", &[15007, 944, 358, 6, 4086, 51950, 94880, 586, 1959, 1036, 63725, 854, 220, 16, 17, 18, 19, 20, 21, 22, 220, 18, 13, 16, 19, 384, 11468, 31, 87, 2659, 3703, 1110, 64, 948, 2899, 30, 67, 40391][..]),
            ("grauer Hintergrund, 写真 スタジオ, фон без людей 😀", &[901, 27097, 472, 2245, 59785, 11, 68739, 247, 88051, 78950, 46207, 75328, 89862, 11, 140406, 91357, 131007, 90316][..]),
            ("   leading spaces\r\nCRLF line\n  indented", &[256, 6388, 12621, 319, 34, 80658, 1555, 198, 220, 1257, 15864][..]),
        ];
        for (text, ids) in expected {
            assert_eq!(t.ids(text), ids, "{text:?}");
        }
        assert_eq!(t.id("<|endoftext|>"), Some(151643));
    }
}
