//! Rule-based filler removal, used when LLM polish is off, unavailable, or times out.
//!
//! Deliberately conservative: it only removes a filler when its position makes the filler
//! reading certain, and prefers leaving a filler in over deleting a content word.
//! - A clause (text between punctuation marks) made only of fillers is dropped.
//!   嗯／呃／欸 and um／uh／hmm always count; 啊、那個、就是、就是說、you know only when a comma-like
//!   mark follows (so 「就是。」 "exactly" and 「啊！」 stay).
//! - 嗯／呃／欸 at the start of a clause are dropped (「嗯我們走」→「我們走」), except 「嗯哼」.
//! - The English words um, umm, uh, uhh, uhm, erm, hmm are dropped anywhere as whole words.
//!
//! Self-corrections (「明天，不對，是後天」) are NOT resolved: that needs the LLM.

/// Fillers that are never content words.
const STRONG: &[&str] = &[
    "嗯", "呃", "欸", "誒", "um", "umm", "uh", "uhh", "uhm", "erm", "hmm",
];
/// Fillers that can also be content, dropped only before a comma-like mark.
const WEAK: &[&str] = &["就是說", "就是", "那個", "啊", "you know"];
/// Clause-initial interjections removed even when the clause continues.
const LEADING: &[char] = &['嗯', '呃', '欸', '誒'];
/// English filler words removed anywhere (as whole words; lowercase or titlecase only, so an
/// all-caps acronym such as "UM" stays).
const EN_WORDS: &[&str] = &["um", "umm", "uh", "uhh", "uhm", "erm", "hmm"];

const COMMA_LIKE: &[char] = &['，', ',', '、', '；', ';', '：', ':'];
const TERMINAL: &[char] = &['。', '.', '！', '!', '？', '?', '\n', '…'];

struct Clause<'a> {
    body: &'a str,
    delim: Option<char>,
}

fn split_clauses(text: &str) -> Vec<Clause<'_>> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, c) in text.char_indices() {
        if COMMA_LIKE.contains(&c) || TERMINAL.contains(&c) {
            out.push(Clause {
                body: &text[start..i],
                delim: Some(c),
            });
            start = i + c.len_utf8();
        }
    }
    if start < text.len() {
        out.push(Clause {
            body: &text[start..],
            delim: None,
        });
    }
    out
}

/// True when `s` (trimmed, non-empty) is nothing but filler tokens separated by whitespace.
fn is_filler_clause(s: &str, delim: Option<char>) -> bool {
    let allow_weak = delim.is_some_and(|d| COMMA_LIKE.contains(&d));
    let mut rest = s.trim();
    if rest.is_empty() {
        return false;
    }
    'outer: while !rest.is_empty() {
        let tokens = STRONG
            .iter()
            .chain(if allow_weak { WEAK } else { &[] }.iter());
        for tok in tokens {
            let title = titlecase(tok);
            if let Some(after) = rest
                .strip_prefix(tok)
                .or_else(|| rest.strip_prefix(title.as_str()))
            {
                // An ASCII token must end at a word boundary ("um" is not "umbrella").
                let ascii = tok.is_ascii();
                if ascii && after.chars().next().is_some_and(|c| c.is_alphanumeric()) {
                    continue;
                }
                rest = after.trim_start();
                continue 'outer;
            }
        }
        return false;
    }
    true
}

/// Removes clause-initial 嗯/呃/欸 and English filler words inside a clause that has content.
fn strip_inline(body: &str) -> String {
    let lead_ws = body.len() - body.trim_start().len();
    let (ws, mut rest) = body.split_at(lead_ws);
    loop {
        let mut chars = rest.chars();
        match (chars.next(), chars.next()) {
            (Some(c), next) if LEADING.contains(&c) && next != Some('哼') => {
                let after = &rest[c.len_utf8()..];
                if after.trim().is_empty() {
                    break;
                }
                rest = after.trim_start();
            }
            _ => break,
        }
    }
    let joined = format!("{ws}{rest}");
    if !joined.contains(' ') {
        return joined;
    }
    let words: Vec<&str> = joined
        .split(' ')
        .filter(|w| !EN_WORDS.iter().any(|f| *w == *f || *w == titlecase(f)))
        .collect();
    words.join(" ")
}

/// `um` -> `Um`; non-ASCII tokens are returned unchanged.
fn titlecase(tok: &str) -> String {
    let mut chars = tok.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Removes Chinese and English filler words (嗯、呃、那個、um、uh…) without changing meaning.
/// A non-empty input that would become empty (e.g. 「嗯。」 said as an answer) is returned unchanged.
pub fn strip_fillers(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for clause in split_clauses(text) {
        if is_filler_clause(clause.body, clause.delim) {
            match clause.delim {
                // A dropped clause before a sentence end hands the end mark to the previous
                // clause: 「好，嗯。」→「好。」.
                Some(d) if TERMINAL.contains(&d) => {
                    let trimmed = out.trim_end();
                    match trimmed.chars().last() {
                        Some(last) if COMMA_LIKE.contains(&last) => {
                            out.truncate(trimmed.len() - last.len_utf8());
                            out.push(d);
                        }
                        _ => {}
                    }
                }
                Some(_) => {}
                // Trailing filler without punctuation: drop the dangling comma before it.
                None => {
                    let trimmed = out.trim_end().trim_end_matches(COMMA_LIKE);
                    out.truncate(trimmed.len());
                }
            }
            continue;
        }
        out.push_str(&strip_inline(clause.body));
        if let Some(d) = clause.delim {
            out.push(d);
        }
    }
    let out = out.trim();
    if out.is_empty() {
        return text.trim().to_string();
    }
    out.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const RAW: &str = include_str!("../../../../tests/fixtures/audio/filler-01.raw.txt");
    const TARGET: &str = include_str!("../../../../tests/fixtures/audio/filler-01.txt");

    /// Character-level edit distance, ignoring punctuation and whitespace.
    fn distance(a: &str, b: &str) -> usize {
        let a: Vec<char> = a.chars().filter(|c| c.is_alphanumeric()).collect();
        let b: Vec<char> = b.chars().filter(|c| c.is_alphanumeric()).collect();
        let mut prev: Vec<usize> = (0..=b.len()).collect();
        for (i, ca) in a.iter().enumerate() {
            let mut cur = vec![i + 1; b.len() + 1];
            for (j, cb) in b.iter().enumerate() {
                cur[j + 1] = (prev[j] + usize::from(ca != cb))
                    .min(prev[j + 1] + 1)
                    .min(cur[j] + 1);
            }
            prev = cur;
        }
        prev[b.len()]
    }

    #[test]
    fn fixture_filler_01_gets_closer_to_target() {
        let out = strip_fillers(RAW.trim());
        assert_eq!(out, "我們明天，不對，是後天早上十點見面。");
        let before = distance(RAW, TARGET);
        let after = distance(&out, TARGET);
        assert!(after < before, "before {before}, after {after}");
        // What is left is only the unresolved self-correction 「明天，不對，是」.
        assert_eq!(after, 5);
    }

    #[test]
    fn drops_filler_clauses() {
        assert_eq!(strip_fillers("嗯，我們走吧。"), "我們走吧。");
        assert_eq!(strip_fillers("我們，呃，走吧。"), "我們，走吧。");
        assert_eq!(strip_fillers("好，嗯。"), "好。");
        assert_eq!(strip_fillers("我們走吧，嗯"), "我們走吧");
        assert_eq!(strip_fillers("嗯嗯，呃 嗯，好"), "好");
        assert_eq!(strip_fillers("就是說，這個方案不錯。"), "這個方案不錯。");
        assert_eq!(strip_fillers("啊，我忘了。"), "我忘了。");
        assert_eq!(strip_fillers("嗯。\n下一段"), "下一段");
    }

    #[test]
    fn drops_leading_interjections() {
        assert_eq!(strip_fillers("嗯我們明天見。"), "我們明天見。");
        assert_eq!(strip_fillers("呃 嗯 好啊"), "好啊");
        assert_eq!(strip_fillers("好，嗯那就這樣。"), "好，那就這樣。");
    }

    #[test]
    fn english_fillers() {
        assert_eq!(
            strip_fillers("Um, so we should, uh, ship it."),
            "so we should, ship it."
        );
        assert_eq!(strip_fillers("I um think it works."), "I think it works.");
        assert_eq!(strip_fillers("It is, you know, fine."), "It is, fine.");
        assert_eq!(
            strip_fillers("這個 meeting uh 我們 delay 一下"),
            "這個 meeting 我們 delay 一下"
        );
    }

    #[test]
    fn never_deletes_content_words() {
        for s in [
            "那個人是誰？",
            "就是他做的。",
            "對，就是。",
            "啊！",
            "好啊。",
            "我嗯不知道", // mid-clause filler: a false negative is fine
            "嗯哼，我知道了。",
            "那個方案就是我們要的。",
            "Bring an umbrella.",
            "The ER is busy, uhura said.",
            "You know what I mean?",
            "You know.",
            "Hummus is 5 mm thick, 1,000.5 grams.",
            "這個 meeting 我們 delay 一下，改到下禮拜。",
            "嗯",
            "嗯。",
            "嗯，呃。",
            "UM, the UH-60 is loud.",
            "",
        ] {
            assert_eq!(strip_fillers(s), s, "input {s:?}");
        }
    }
}
