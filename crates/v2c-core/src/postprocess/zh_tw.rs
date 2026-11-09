//! Simplified -> Traditional Chinese (Taiwan phrasing). English is left unchanged.
//!
//! Uses `ferrous-opencc` (a pure-Rust OpenCC, so it also builds for Android) with the
//! `s2twp` configuration: Simplified -> Traditional characters, then Taiwan phrases
//! (软件 -> 軟體, 视频 -> 影片, 内存 -> 記憶體) and Taiwan variants. `zhconv` was evaluated
//! first and rejected because it leaves common phrases such as 视频 / 内存 / 信息 unconverted
//! (see `tasks/plan.md` section 2).
//!
//! Breeze already outputs Traditional Chinese most of the time, so this is a safety net that
//! must not damage text that is already Traditional:
//! - Text without any Simplified character is returned unchanged (the Taiwan phrase table would
//!   otherwise rewrite valid Traditional words, e.g. 文件 -> 檔案, 了解 -> 瞭解).
//! - OpenCC rewrites 台 to 臺 (台灣 -> 臺灣), which is valid but not how most people in Taiwan
//!   write it, so 台 is kept as written.

use std::sync::OnceLock;

use ferrous_opencc::{config::BuiltinConfig, OpenCC};

/// Private-use placeholder that shields 台 from OpenCC's 台 -> 臺 rewrite.
const TAI_GUARD: char = '\u{E000}';

fn converter() -> Option<&'static OpenCC> {
    static CC: OnceLock<Option<OpenCC>> = OnceLock::new();
    // The dictionaries are compiled into the binary, so this only fails on a broken build.
    CC.get_or_init(|| OpenCC::from_config(BuiltinConfig::S2twp).ok())
        .as_ref()
}

/// Plain Simplified -> Traditional, used only to detect Simplified characters.
fn char_converter() -> Option<&'static OpenCC> {
    static CC: OnceLock<Option<OpenCC>> = OnceLock::new();
    CC.get_or_init(|| OpenCC::from_config(BuiltinConfig::S2t).ok())
        .as_ref()
}

/// True if any character on its own converts to a different Traditional character, i.e. the
/// text contains Simplified characters. Checked per character so that phrase rules that also
/// fire on valid Traditional text (了解 -> 瞭解, 文件 -> 檔案) do not count.
fn has_simplified(text: &str, s2t: &OpenCC) -> bool {
    let mut buf = [0u8; 4];
    text.chars().filter(|&c| is_cjk(c) && c != '台').any(|c| {
        let s = c.encode_utf8(&mut buf);
        s2t.convert(s) != *s
    })
}

/// True for CJK ideographs (the only characters OpenCC changes).
fn is_cjk(c: char) -> bool {
    matches!(c,
        '\u{3400}'..='\u{4DBF}'
        | '\u{4E00}'..='\u{9FFF}'
        | '\u{F900}'..='\u{FAFF}'
        | '\u{20000}'..='\u{3134F}')
}

/// Converts Chinese text to Taiwan Traditional Chinese, leaving English untouched.
pub fn to_zh_tw(text: &str) -> String {
    // Fast path; also guarantees pure English / ASCII text comes back byte-for-byte.
    if !text.chars().any(is_cjk) {
        return text.to_owned();
    }
    let (Some(cc), Some(s2t)) = (converter(), char_converter()) else {
        return text.to_owned();
    };
    // Already Traditional (Breeze's normal output): leave it exactly as recognised.
    if !has_simplified(text, s2t) {
        return text.to_owned();
    }
    if text.contains(TAI_GUARD) {
        // Cannot shield 台 safely; convert without the guard.
        return cc.convert(text);
    }
    let guarded: String = text
        .chars()
        .map(|c| if c == '台' { TAI_GUARD } else { c })
        .collect();
    cc.convert(&guarded)
        .chars()
        .map(|c| if c == TAI_GUARD { '台' } else { c })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simplified_becomes_taiwan_traditional_with_taiwan_phrasing() {
        let cases = [
            ("软件开发", "軟體開發"),
            ("我在看视频", "我在看影片"),
            ("这个程序的内存不够", "這個程式的記憶體不夠"),
            ("打印机和鼠标", "印表機和滑鼠"),
            ("网络信息", "網路資訊"),
            ("服务器的默认设置", "伺服器的預設設定"),
            ("出租车", "計程車"),
            ("头发 发展", "頭髮 發展"),
        ];
        for (input, expected) in cases {
            assert_eq!(to_zh_tw(input), expected, "input: {input}");
        }
    }

    #[test]
    fn fully_traditional_text_is_never_rephrased() {
        // OpenCC's Taiwan phrase table would rewrite these, but they are valid Traditional
        // text (文件 = document), so text without Simplified characters is left alone.
        for s in [
            "寫進 v2c 的文件",
            "我們先了解一下",
            "這個週末",
            "我們的軟件",
        ] {
            assert_eq!(to_zh_tw(s), s);
        }
    }

    #[test]
    fn detects_simplified_characters() {
        let s2t = char_converter().unwrap();
        assert!(has_simplified("这个", s2t));
        assert!(has_simplified("我們的视频", s2t));
        assert!(!has_simplified("這個影片", s2t));
        assert!(!has_simplified("台灣 English", s2t));
    }

    #[test]
    fn already_traditional_text_is_unchanged() {
        let cases = [
            "今天下午三點要開會，記得把報告準備好。",
            "這家餐廳的牛肉麵很好吃，下次我們再一起來。",
            "請幫我確認一下明天的高鐵票是幾點出發。",
            "我們後天早上十點見面。",
            "我覺得這個方案還不錯",
            "他著急地跑出去，頭髮很亂。",
            "皇后、乾杯、一隻貓、老闆、鬱悶、什麼、甚麼、裡面",
            "臺灣的資訊與影片",
        ];
        for s in cases {
            assert_eq!(to_zh_tw(s), s);
        }
    }

    #[test]
    fn tai_is_kept_as_written() {
        assert_eq!(to_zh_tw("台灣"), "台灣");
        assert_eq!(to_zh_tw("台北的台积电"), "台北的台積電");
        assert_eq!(to_zh_tw("臺北"), "臺北");
    }

    #[test]
    fn english_is_untouched() {
        for s in [
            "Please send me the latest version of the slides before Friday.",
            "I think we should test this on a real device first.",
            "",
            "  spaces\tand\nnewlines  ",
        ] {
            assert_eq!(to_zh_tw(s), s);
        }
    }

    #[test]
    fn mixed_text_keeps_english_and_converts_chinese() {
        assert_eq!(
            to_zh_tw("我今天用 Rust 写了一个 API server"),
            "我今天用 Rust 寫了一個 API server"
        );
        for s in [
            "這個 meeting 我們 delay 一下，改到下禮拜。",
            "我已經把 code push 上去了，你再幫我 review 一下。",
            "這個 feature 的 deadline 是月底，但 QA 還沒排進去。",
            "請把 Breeze ASR 跟 LM Studio 的設定寫進 v2c 的文件。",
        ] {
            assert_eq!(to_zh_tw(s), s);
        }
    }
}
