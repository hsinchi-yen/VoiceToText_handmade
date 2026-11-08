//! `hotkey-test`: install the global hook, run the trigger state machine, and print every
//! command it emits. Prints key classes only (Trigger/Shift/Escape/Other), never which
//! character key was typed.

#[derive(clap::Args)]
pub struct Args {
    /// Stop after this many seconds.
    #[arg(long, default_value_t = 60)]
    pub seconds: u64,
}

#[cfg(windows)]
pub fn run(args: Args) -> anyhow::Result<()> {
    use std::sync::mpsc::{self, RecvTimeoutError};
    use std::time::Duration;

    use v2c_core::config::Config;
    use v2c_core::trigger::{Event, Trigger, TriggerConfig};
    use v2c_platform::windows::hotkey::{monotonic_ms, WindowsHotkey};
    use v2c_platform::Hotkey;

    const TICK_MS: u64 = 50;

    let cfg = TriggerConfig::from(&Config::default().hotkey);
    let mut trigger = Trigger::new(cfg);

    let (tx, rx) = mpsc::channel::<Event>();
    let mut hook = WindowsHotkey::new();
    hook.start(Box::new(move |event| {
        let _ = tx.send(event);
    }))?;

    let start = monotonic_ms();
    let deadline = start + args.seconds.saturating_mul(1000);
    println!(
        "hotkey-test: listening for {} s (hold {} ms, double-tap {} ms, hands-free max {} ms)",
        args.seconds, cfg.hold_threshold_ms, cfg.double_tap_window_ms, cfg.hands_free_max_ms
    );
    println!("Right Ctrl = dictate, Shift+Right Ctrl = translate, Esc = cancel.");

    let mut next_tick = start + TICK_MS;
    loop {
        let now = monotonic_ms();
        if now >= deadline {
            break;
        }
        let wait = next_tick.min(deadline).saturating_sub(now);
        let event = match rx.recv_timeout(Duration::from_millis(wait)) {
            Ok(event) => {
                print_key(start, &event);
                event
            }
            Err(RecvTimeoutError::Timeout) => {
                let at_ms = monotonic_ms();
                next_tick = at_ms + TICK_MS;
                Event::Tick { at_ms }
            }
            Err(RecvTimeoutError::Disconnected) => break,
        };
        let at_ms = match event {
            Event::KeyDown { at_ms, .. } | Event::KeyUp { at_ms, .. } | Event::Tick { at_ms } => {
                at_ms
            }
        };
        for command in trigger.on_event(event) {
            println!("[{:>8} ms] => {command:?}", at_ms.saturating_sub(start));
        }
    }

    hook.stop();
    println!("hotkey-test: done");
    Ok(())
}

#[cfg(windows)]
fn print_key(start: u64, event: &v2c_core::trigger::Event) {
    use v2c_core::trigger::Event;
    // `Key` is only a class (Trigger/Shift/Escape/Other), so this never reveals typed text.
    match event {
        Event::KeyDown { key, at_ms } => {
            println!("[{:>8} ms]    down {key:?}", at_ms.saturating_sub(start))
        }
        Event::KeyUp { key, at_ms } => {
            println!("[{:>8} ms]    up   {key:?}", at_ms.saturating_sub(start))
        }
        Event::Tick { .. } => {}
    }
}

#[cfg(not(windows))]
pub fn run(args: Args) -> anyhow::Result<()> {
    let _ = args;
    anyhow::bail!("hotkey-test is only implemented on Windows so far")
}
