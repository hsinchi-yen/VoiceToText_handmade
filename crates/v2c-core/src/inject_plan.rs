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
pub fn plan(text: &str, profile: &AppProfile) -> Vec<Action> {
    let _ = (text, profile);
    todo!("T0.3: implement per tasks/plan.md section 4")
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

    #[test]
    fn action_debug_hides_text() {
        let s = format!("{:?}", Action::Paste("密碼123".into()));
        assert!(!s.contains("密碼"));
    }
}
