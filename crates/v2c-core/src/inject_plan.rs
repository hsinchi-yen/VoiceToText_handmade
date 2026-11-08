//! Turns output text into injection actions (pure logic). The platform `Injector`
//! executes them. Rules: `tasks/plan.md` section 4. Logic is implemented in T0.3.

use crate::config::{AppRule, NewlineKey};
use crate::redact::Redacted;

#[derive(Clone, PartialEq, Eq)]
pub enum Action {
    /// Type this text with simulated Unicode key events. Never contains line breaks.
    Text(String),
    /// Press the app's newline key combination.
    NewLine(NewlineKey),
    /// Put this text on the clipboard, paste, then restore the clipboard.
    Paste(String),
}

impl std::fmt::Debug for Action {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Action::Text(t) => f.debug_tuple("Text").field(&Redacted(t)).finish(),
            Action::NewLine(k) => f.debug_tuple("NewLine").field(k).finish(),
            Action::Paste(t) => f.debug_tuple("Paste").field(&Redacted(t)).finish(),
        }
    }
}

/// Injection behaviour for the foreground app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AppProfile {
    pub newline: NewlineKey,
    pub always_paste: bool,
}

/// Finds the profile for an executable name (case-insensitive); default profile if no rule matches.
pub fn profile_for(exe: &str, rules: &[AppRule]) -> AppProfile {
    rules
        .iter()
        .find(|r| r.exe.eq_ignore_ascii_case(exe))
        .map(|r| AppProfile {
            newline: r.newline,
            always_paste: r.always_paste,
        })
        .unwrap_or_default()
}

/// Splits `text` into actions for the given app profile.
///
/// - Empty text gives no actions.
/// - `always_paste` gives a single [`Action::Paste`] with the text unchanged (the paste
///   implementation decides the clipboard line-ending format).
/// - Otherwise `\r\n` and lone `\r` are treated as line breaks; each line break becomes one
///   [`Action::NewLine`] with the profile's key, and the text between them becomes
///   [`Action::Text`] runs (never empty, never containing a line break).
pub fn plan(text: &str, profile: &AppProfile) -> Vec<Action> {
    if text.is_empty() {
        return Vec::new();
    }
    if profile.always_paste {
        return vec![Action::Paste(text.to_owned())];
    }

    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut actions = Vec::new();
    for (i, line) in normalized.split('\n').enumerate() {
        if i > 0 {
            actions.push(Action::NewLine(profile.newline));
        }
        if !line.is_empty() {
            actions.push(Action::Text(line.to_owned()));
        }
    }
    actions
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::default_app_rules;

    #[test]
    fn profile_lookup_is_case_insensitive() {
        let p = profile_for("line.EXE", &default_app_rules());
        assert_eq!(p.newline, NewlineKey::ShiftEnter);
        assert_eq!(
            profile_for("notepad.exe", &default_app_rules()),
            AppProfile::default()
        );
    }

    fn text(s: &str) -> Action {
        Action::Text(s.into())
    }

    const ENTER: AppProfile = AppProfile {
        newline: NewlineKey::Enter,
        always_paste: false,
    };
    const CHAT: AppProfile = AppProfile {
        newline: NewlineKey::ShiftEnter,
        always_paste: false,
    };

    #[test]
    fn empty_text_gives_no_actions() {
        assert!(plan("", &ENTER).is_empty());
        let paste = AppProfile {
            always_paste: true,
            ..ENTER
        };
        assert!(plan("", &paste).is_empty());
    }

    #[test]
    fn single_line_is_one_text_run() {
        assert_eq!(plan("hello", &ENTER), vec![text("hello")]);
    }

    #[test]
    fn normal_app_uses_enter() {
        assert_eq!(
            plan("第一行\n第二行", &ENTER),
            vec![
                text("第一行"),
                Action::NewLine(NewlineKey::Enter),
                text("第二行")
            ]
        );
    }

    #[test]
    fn chat_apps_use_shift_enter() {
        let line = profile_for("LINE.exe", &default_app_rules());
        assert_eq!(line, CHAT);
        assert_eq!(
            plan("a\nb", &line),
            vec![
                text("a"),
                Action::NewLine(NewlineKey::ShiftEnter),
                text("b")
            ]
        );
        for exe in ["chrome.exe", "slack.exe", "Discord.exe", "ms-teams.exe"] {
            let p = profile_for(exe, &default_app_rules());
            assert!(
                !plan("x\ny", &p).contains(&Action::NewLine(NewlineKey::Enter)),
                "{exe} must not get a plain Enter"
            );
        }
    }

    #[test]
    fn crlf_and_cr_are_normalised() {
        let expected = vec![
            text("a"),
            Action::NewLine(NewlineKey::Enter),
            text("b"),
            Action::NewLine(NewlineKey::Enter),
            text("c"),
        ];
        assert_eq!(plan("a\r\nb\rc", &ENTER), expected);
        assert_eq!(plan("a\nb\nc", &ENTER), expected);
    }

    #[test]
    fn blank_lines_and_edges_keep_every_newline() {
        assert_eq!(
            plan("a\n\nb", &ENTER),
            vec![
                text("a"),
                Action::NewLine(NewlineKey::Enter),
                Action::NewLine(NewlineKey::Enter),
                text("b")
            ]
        );
        assert_eq!(
            plan("end\n", &CHAT),
            vec![text("end"), Action::NewLine(NewlineKey::ShiftEnter)]
        );
        assert_eq!(
            plan("\nstart", &ENTER),
            vec![Action::NewLine(NewlineKey::Enter), text("start")]
        );
        assert_eq!(
            plan("\r\n", &ENTER),
            vec![Action::NewLine(NewlineKey::Enter)]
        );
    }

    #[test]
    fn always_paste_gives_single_paste() {
        let p = AppProfile {
            newline: NewlineKey::ShiftEnter,
            always_paste: true,
        };
        let rules = vec![AppRule {
            exe: "Legacy.exe".into(),
            newline: NewlineKey::ShiftEnter,
            always_paste: true,
        }];
        assert_eq!(profile_for("legacy.exe", &rules), p);
        assert_eq!(
            plan("line1\r\nline2\n", &p),
            vec![Action::Paste("line1\r\nline2\n".into())]
        );
    }

    #[test]
    fn surrogate_pairs_and_emoji_stay_intact() {
        // U+1F44D and U+20BB7 are outside the BMP (surrogate pairs in UTF-16).
        let s = "讚👍\n𠮷野家👨‍👩‍👧";
        let actions = plan(s, &ENTER);
        assert_eq!(
            actions,
            vec![
                text("讚👍"),
                Action::NewLine(NewlineKey::Enter),
                text("𠮷野家👨‍👩‍👧")
            ]
        );
        if let Action::Text(t) = &actions[0] {
            assert_eq!(t.encode_utf16().count(), 3);
        }
    }

    #[test]
    fn mixed_zh_en_keeps_spacing_and_punctuation() {
        assert_eq!(
            plan("這個 meeting 我們 delay 一下，OK？\n第二行", &CHAT),
            vec![
                text("這個 meeting 我們 delay 一下，OK？"),
                Action::NewLine(NewlineKey::ShiftEnter),
                text("第二行")
            ]
        );
    }

    #[test]
    fn text_runs_never_contain_line_breaks_or_are_empty() {
        for s in ["\r\r\n\n", "a\r\n\rb\n", "x", "\n\n"] {
            for a in plan(s, &ENTER) {
                if let Action::Text(t) = a {
                    assert!(!t.is_empty());
                    assert!(!t.contains(['\r', '\n']));
                }
            }
        }
        // CR, CRLF, LF -> three line breaks.
        assert_eq!(plan("\r\r\n\n", &ENTER).len(), 3);
    }

    #[test]
    fn action_debug_hides_text() {
        let s = format!("{:?}", Action::Paste("密碼123".into()));
        assert!(!s.contains("密碼"));
    }
}
