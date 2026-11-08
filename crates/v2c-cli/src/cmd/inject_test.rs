use v2c_core::inject_plan::Action;

#[derive(clap::Args)]
pub struct Args {
    /// Text to inject; `\n` is a line break.
    pub text: String,
    /// Seconds to wait so you can focus the target window.
    #[arg(long, default_value_t = 3)]
    pub delay: u64,
    /// Force clipboard paste instead of typing (T1.2).
    #[arg(long)]
    pub paste: bool,
}

/// Turns the literal two-character sequence `\n` into a line break.
fn unescape_newlines(s: &str) -> String {
    s.replace("\\n", "\n")
}

/// Per-action summary without the text itself (lengths only).
fn describe(action: &Action) -> String {
    match action {
        Action::Text(t) => format!(
            "Text({} chars, {} utf16)",
            t.chars().count(),
            t.encode_utf16().count()
        ),
        Action::NewLine(k) => format!("NewLine({k:?})"),
        Action::Paste(t) => format!("Paste({} chars)", t.chars().count()),
    }
}

#[cfg(windows)]
pub fn run(args: Args) -> anyhow::Result<()> {
    use std::io::Write;
    use v2c_core::config::default_app_rules;
    use v2c_core::inject_plan::{plan, profile_for, AppProfile};
    use v2c_platform::windows::{foreground::WinForeground, inject::SendInputInjector};
    use v2c_platform::{Foreground, Injector};

    if args.paste {
        anyhow::bail!("--paste is not implemented yet (task T1.2)");
    }
    let text = unescape_newlines(&args.text);

    for remaining in (1..=args.delay).rev() {
        print!("\rinjecting in {remaining} s... focus the target window ");
        std::io::stdout().flush().ok();
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    println!();

    let app = WinForeground.current_app();
    let profile = match &app {
        Some(a) => profile_for(&a.exe, &default_app_rules()),
        None => AppProfile::default(),
    };
    match &app {
        Some(a) => println!(
            "foreground: {} (elevated: {}, fullscreen: {})",
            a.exe, a.elevated, a.fullscreen
        ),
        None => println!("foreground: <unknown>"),
    }
    println!("profile: {profile:?}");

    let actions = plan(&text, &profile);
    println!(
        "actions: {} (input {} chars, {} utf16 units)",
        actions.len(),
        text.chars().count(),
        text.encode_utf16().count()
    );
    for (i, a) in actions.iter().enumerate() {
        println!("  [{i}] {}", describe(a));
    }

    let started = std::time::Instant::now();
    SendInputInjector
        .run(&actions)
        .map_err(|e| anyhow::anyhow!("injection failed: {e}"))?;
    println!("done in {} ms", started.elapsed().as_millis());
    Ok(())
}

#[cfg(not(windows))]
pub fn run(args: Args) -> anyhow::Result<()> {
    let _ = (args, unescape_newlines, describe);
    anyhow::bail!("inject-test is only implemented on Windows so far")
}

#[cfg(test)]
mod tests {
    use super::*;
    use v2c_core::config::NewlineKey;

    #[test]
    fn literal_backslash_n_becomes_newline() {
        assert_eq!(unescape_newlines(r"a\nb"), "a\nb");
        assert_eq!(unescape_newlines("no escapes"), "no escapes");
    }

    #[test]
    fn describe_never_contains_text() {
        let d = describe(&Action::Text("祕密 secret👍".into()));
        assert_eq!(d, "Text(10 chars, 11 utf16)");
        assert!(!describe(&Action::Paste("祕密".into())).contains("祕密"));
        assert_eq!(
            describe(&Action::NewLine(NewlineKey::ShiftEnter)),
            "NewLine(ShiftEnter)"
        );
    }
}
