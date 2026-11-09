//! Simplified -> Traditional Chinese (Taiwan phrasing). English is left unchanged.
//!
//! Uses `ferrous-opencc` (a pure-Rust OpenCC, so it also builds for Android) with the
//! `s2twp` configuration: Simplified -> Traditional characters, then Taiwan phrases
//! (软件 -> 軟體, 视频 -> 影片, 内存 -> 記憶體) and Taiwan variants. `zhconv` was evaluated
//! first and rejected because it leaves common phrases such as 视频 / 内存 / 信息 unconverted
//! (see `tasks/plan.md` section 2).
//!
//! Breeze already outputs Traditional Chinese most of the time, so this is a safety net that
//! must not damage text that is already Traditional. OpenCC applied to Traditional text does
//! damage it (干擾 -> 幹擾, 里長 -> 裡長, 文件 -> 檔案), so conversion is limited to where
//! Simplified text actually is:
//!
//! 1. A character is **Simplified-only** if OpenCC's S2T maps it to something else *and* it is
//!    not itself one of its own Traditional candidates in OpenCC's `STCharacters` table (干 ->
//!    幹 乾 干 lists 干, so 干 is also valid Traditional; 这 -> 這 does not, so 这 is
//!    Simplified-only). Those self-listed characters are [`SHARED_CHARS`].
//! 2. Text without Simplified-only characters is returned unchanged.
//! 3. Otherwise the text is split into runs of CJK characters (punctuation, spaces, and Latin
//!    text are boundaries and copied as is). A run without Simplified-only characters is kept.
//!    A run with no **Traditional-only** character (one that T2S changes, e.g. 們) is treated as
//!    Simplified and converted whole, with full Taiwan phrasing. A run mixing both scripts is
//!    converted only in windows of one character around each Simplified-only character, so
//!    the Traditional words next to it are kept (我們的文件和视频 -> 我們的文件和影片).
//! 4. OpenCC rewrites 台 to 臺 (台灣 -> 臺灣), which is valid but not how most people in Taiwan
//!    write it, so 台 is kept as written.

use std::sync::OnceLock;

use ferrous_opencc::{config::BuiltinConfig, OpenCC};

/// Private-use placeholder that shields 台 from OpenCC's 台 -> 臺 rewrite.
const TAI_GUARD: char = '\u{E000}';

/// Characters that OpenCC's `STCharacters.txt` maps to a list containing the character itself,
/// i.e. characters that are valid in both Simplified and Traditional text. Generated from
/// ferrous-opencc 0.4.0 `assets/dictionaries/STCharacters.txt` with:
/// `awk -F'\t' '{n=split($2,a," "); for(i=1;i<=n;i++) if(a[i]==$1){printf "%s",$1; break}}'`
const SHARED_CHARS: &str = "㐹万丑丰了于云亘仆仇价仿伙余佛佣俊修借僵克党冬准凌几凶出划刮制千升\
卜占卷厂厘只台叶吁吃合吊同后向周咨咸咽哄唇喂噪回困夫夸奸姜娘它家尸局岩岳巨布帘席干幸广庵弦彩征\
御志念恤愈愿戚才扎托扣折抵拐挂挨挽据搜斗旋昆暗曲朱朴杆杠杯杰松板极柜栗核梁欲沈沾泛注涂涌淀游漓\
熏玩璇症皂矩确私秋种筑系胄背胜胡腊腌膻致舍芸苔苹范荐蒙蔑虫蚝蜡蝎表谷跖辟适郁酸采里雕面";

/// Characters of context converted on each side of a Simplified-only character in a run that
/// mixes scripts.
const WINDOW: usize = 1;

struct Converters {
    s2twp: OpenCC,
    s2t: OpenCC,
    t2s: OpenCC,
}

fn converters() -> Option<&'static Converters> {
    static CC: OnceLock<Option<Converters>> = OnceLock::new();
    // The dictionaries are compiled into the binary, so this only fails on a broken build.
    CC.get_or_init(|| {
        Some(Converters {
            s2twp: OpenCC::from_config(BuiltinConfig::S2twp).ok()?,
            s2t: OpenCC::from_config(BuiltinConfig::S2t).ok()?,
            t2s: OpenCC::from_config(BuiltinConfig::T2s).ok()?,
        })
    })
    .as_ref()
}

/// True for CJK ideographs (the only characters OpenCC changes).
fn is_cjk(c: char) -> bool {
    matches!(c,
        '\u{3400}'..='\u{4DBF}'
        | '\u{4E00}'..='\u{9FFF}'
        | '\u{F900}'..='\u{FAFF}'
        | '\u{20000}'..='\u{3134F}')
}

fn changes(cc: &OpenCC, c: char) -> bool {
    let mut buf = [0u8; 4];
    let s = c.encode_utf8(&mut buf);
    cc.convert(s) != *s
}

impl Converters {
    /// Exists only in Simplified Chinese (see module docs, rule 1).
    fn simplified_only(&self, c: char) -> bool {
        is_cjk(c) && !SHARED_CHARS.contains(c) && changes(&self.s2t, c)
    }

    /// Exists only in Traditional Chinese.
    fn traditional_only(&self, c: char) -> bool {
        is_cjk(c) && changes(&self.t2s, c)
    }

    /// Full `s2twp` conversion that keeps 台 as written.
    fn convert(&self, segment: &str) -> String {
        if segment.contains(TAI_GUARD) {
            return self.s2twp.convert(segment);
        }
        let guarded: String = segment
            .chars()
            .map(|c| if c == '台' { TAI_GUARD } else { c })
            .collect();
        self.s2twp
            .convert(&guarded)
            .chars()
            .map(|c| if c == TAI_GUARD { '台' } else { c })
            .collect()
    }

    fn convert_run(&self, run: &[char], out: &mut String) {
        let simplified: Vec<bool> = run.iter().map(|&c| self.simplified_only(c)).collect();
        if !simplified.contains(&true) {
            out.extend(run);
            return;
        }
        if !run.iter().any(|&c| self.traditional_only(c)) {
            out.push_str(&self.convert(&run.iter().collect::<String>()));
            return;
        }
        // Mixed scripts: convert small windows around Simplified-only characters.
        let mut mask = vec![false; run.len()];
        for (i, _) in simplified.iter().enumerate().filter(|(_, &s)| s) {
            let end = (i + WINDOW + 1).min(run.len());
            mask[i.saturating_sub(WINDOW)..end].fill(true);
        }
        let mut i = 0;
        while i < run.len() {
            let j = (i..run.len())
                .find(|&k| mask[k] != mask[i])
                .unwrap_or(run.len());
            let segment: String = run[i..j].iter().collect();
            if mask[i] {
                out.push_str(&self.convert(&segment));
            } else {
                out.push_str(&segment);
            }
            i = j;
        }
    }
}

/// Converts Chinese text to Taiwan Traditional Chinese, leaving English untouched.
pub fn to_zh_tw(text: &str) -> String {
    // Fast path; also guarantees pure English / ASCII text comes back byte-for-byte.
    if !text.chars().any(is_cjk) {
        return text.to_owned();
    }
    let Some(cc) = converters() else {
        return text.to_owned();
    };
    // Already Traditional (Breeze's normal output): leave it exactly as recognised.
    if !text.chars().any(|c| cc.simplified_only(c)) {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut run: Vec<char> = Vec::new();
    for c in text.chars() {
        if is_cjk(c) {
            run.push(c);
        } else {
            cc.convert_run(&run, &mut out);
            run.clear();
            out.push(c);
        }
    }
    cc.convert_run(&run, &mut out);
    out
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
            ("这个干扰很大", "這個干擾很大"),
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
    fn classifies_characters() {
        let cc = converters().unwrap();
        for c in ['这', '视', '频', '发', '们'] {
            assert!(cc.simplified_only(c), "{c}");
        }
        for c in [
            '干', '里', '后', '只', '系', '范', '松', '面', '准', '余', '台', '這', '文',
        ] {
            assert!(!cc.simplified_only(c), "{c}");
        }
        assert!(cc.traditional_only('們'));
        assert!(!cc.traditional_only('文'));
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
            "干擾",
            "這個干擾很大",
            "里長",
            "鄰里長辦公室",
            "只有一個系統的範圍",
        ];
        for s in cases {
            assert_eq!(to_zh_tw(s), s);
        }
    }

    #[test]
    fn mixed_script_converts_only_around_simplified_characters() {
        let cases = [
            ("我們的文件和视频", "我們的文件和影片"),
            ("我們要了解這個视频", "我們要了解這個影片"),
            ("我們的文件，和这个视频", "我們的文件，和這個影片"),
            ("里長說这个", "里長說這個"),
            ("鄰里長辦公室的软件", "鄰里長辦公室的軟體"),
        ];
        for (input, expected) in cases {
            assert_eq!(to_zh_tw(input), expected, "input: {input}");
        }
    }

    #[test]
    fn clauses_are_converted_independently() {
        // The Traditional clause keeps 文件; the Simplified clause gets Taiwan phrasing.
        assert_eq!(
            to_zh_tw("我們的文件都在這裡。这个视频的内存不够"),
            "我們的文件都在這裡。這個影片的記憶體不夠"
        );
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
