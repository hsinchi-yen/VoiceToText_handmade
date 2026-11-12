//! Desktop notifications on Linux (T3.4 `DbusNotifier`): controller status events on Wayland,
//! where the overlay is off, plus IBus and kept-text notices on any session.
//!
//! Like the 0.2.0 overlay, which shows no status text, a normal utterance (recording,
//! recognising, polishing, done) shows no bubble (T4.9). Only what needs the user's attention
//! does: errors, kept text, and the hands-free hint 「按右 Ctrl 結束」 (the overlay shows it
//! beside the waveform; Wayland has no overlay).
//!
//! [`StatusNotes`] maps [`StatusEvent`]s to notifications (pure, unit-tested). [`Notes`]
//! sends them to one background thread that owns the D-Bus connection, so a slow or missing
//! notification daemon never blocks the controller or the UI thread. Notifications carry
//! static UI text only (`notify::notification_for`), never the transcript.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use tracing::{debug, info};
use v2c_platform::linux::notify::{DbusNotifier, Notifier, NotifyErrorKind, NotifyStatus};

use crate::controller::StatusEvent;

/// One request for the notification thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Note {
    Show(NotifyStatus),
    Dismiss,
}

/// Status codes that are notices after a finished utterance or internal states, not
/// failures worth a bubble (the main window still shows them).
const QUIET_CODES: &[&str] = &[
    "cloud_fell_back",
    "polish_fell_back",
    "polish_timed_out",
    "hands_free_limit",
    "gpu_low_vram",
    "gpu_restored",
    "gpu_yield_heavy_app",
    "busy",
];

/// Maps a status error code to the notification kind, or `None` for no bubble.
/// Injection failures are `None`: the kept-text sink sends [`NotifyStatus::TextKept`]
/// once it knows the text is safe.
pub fn error_kind(code: &str) -> Option<NotifyErrorKind> {
    if QUIET_CODES.contains(&code) || code.starts_with("inject_") {
        return None;
    }
    Some(match code {
        c if c.starts_with("mic_") => NotifyErrorKind::MicrophoneUnavailable,
        "hotkey_failed" => NotifyErrorKind::HotkeyUnavailable,
        "asr_failed"
        | "audio_invalid"
        | "model_missing"
        | "model_load_failed"
        | "sensevoice_not_available"
        | "asr_loading" => NotifyErrorKind::RecognitionFailed,
        _ => NotifyErrorKind::Other,
    })
}

/// Which bubble [`StatusNotes`] last opened.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Open {
    #[default]
    Nothing,
    HandsFree,
    Error,
}

/// `Recording` arrives every 50 ms while recording; a longer gap means that recording ended
/// (the controller sends no event when it stops, and the previous utterance's `Transcribing`
/// / `Done` can arrive while the next one is recording).
pub const RECORDING_GAP: Duration = Duration::from_millis(500);

/// Turns the controller's status stream into notifications. Progress is silent; an error or
/// the hands-free hint opens a bubble.
/// - The hint shows once per recording and closes when that recording ends: after
///   [`RECORDING_GAP`] without `Recording` ([`StatusNotes::on_idle`]) or on `Cancelled`.
/// - An error stays until a new recording starts (not on the next 50 ms tick of the
///   recording it arrived in). The hint never replaces an error.
#[derive(Debug, Default)]
pub struct StatusNotes {
    last_recording: Option<Instant>,
    hint_shown: bool,
    open: Open,
}

impl StatusNotes {
    pub fn on_status(&mut self, ev: &StatusEvent, now: Instant) -> Option<Note> {
        match ev {
            StatusEvent::Recording { hands_free, .. } => {
                let starting = self
                    .last_recording
                    .is_none_or(|t| now.saturating_duration_since(t) > RECORDING_GAP);
                self.last_recording = Some(now);
                if starting {
                    self.hint_shown = false;
                }
                if *hands_free && !self.hint_shown && self.open != Open::Error {
                    // Replaces whatever bubble is open (one bubble id per notifier).
                    self.hint_shown = true;
                    self.open = Open::HandsFree;
                    Some(Note::Show(NotifyStatus::HandsFree))
                } else if starting && self.open != Open::Nothing {
                    self.open = Open::Nothing;
                    Some(Note::Dismiss)
                } else {
                    None
                }
            }
            StatusEvent::Cancelled => {
                self.last_recording = None;
                self.close_hint()
            }
            StatusEvent::Transcribing
            | StatusEvent::Polishing
            | StatusEvent::Done { .. }
            | StatusEvent::MicReady => None,
            StatusEvent::Error { code } => error_kind(code).map(|k| {
                self.open = Open::Error;
                Note::Show(NotifyStatus::Error(k))
            }),
        }
    }

    /// Called when no event arrived for a while: closes the hint once its recording ended.
    pub fn on_idle(&mut self, now: Instant) -> Option<Note> {
        let ended = self
            .last_recording
            .is_none_or(|t| now.saturating_duration_since(t) > RECORDING_GAP);
        if ended {
            self.close_hint()
        } else {
            None
        }
    }

    /// The hint is open: the caller should call [`StatusNotes::on_idle`] after a quiet spell.
    pub fn wants_idle(&self) -> bool {
        self.open == Open::HandsFree
    }

    fn close_hint(&mut self) -> Option<Note> {
        (self.open == Open::HandsFree).then(|| {
            self.open = Open::Nothing;
            Note::Dismiss
        })
    }
}

/// Handle to the notification thread. Cheap to clone; sending never blocks.
#[derive(Clone)]
pub struct Notes {
    tx: mpsc::Sender<Note>,
}

/// Retry connecting to the session bus at most this often after a failure.
const RECONNECT_EVERY: Duration = Duration::from_secs(30);

impl Notes {
    /// Starts the thread. The D-Bus connection is made there, on the first note.
    pub fn start() -> Self {
        let (tx, rx) = mpsc::channel::<Note>();
        let spawned = std::thread::Builder::new()
            .name("v2c-notify".into())
            .spawn(move || run(rx));
        if spawned.is_err() {
            tracing::warn!("could not start the notification thread");
        }
        Self { tx }
    }

    /// A handle whose notes go to `tx` (tests).
    #[cfg(test)]
    pub fn with_sender(tx: mpsc::Sender<Note>) -> Self {
        Self { tx }
    }

    pub fn send(&self, note: Note) {
        let _ = self.tx.send(note);
    }

    pub fn show(&self, status: NotifyStatus) {
        self.send(Note::Show(status));
    }
}

fn run(rx: mpsc::Receiver<Note>) {
    let mut notifier: Option<DbusNotifier> = None;
    let mut last_attempt: Option<Instant> = None;
    for note in rx {
        if notifier.is_none() && last_attempt.is_none_or(|t| t.elapsed() >= RECONNECT_EVERY) {
            last_attempt = Some(Instant::now());
            match DbusNotifier::connect() {
                Ok(n) => {
                    info!("notifications: session bus connected");
                    notifier = Some(n);
                }
                Err(e) => info!(error = %e, "notifications unavailable"),
            }
        }
        let Some(n) = notifier.as_mut() else { continue };
        // Best effort: a failed bubble never affects dictation.
        let result = match note {
            Note::Show(s) => n.show(s),
            Note::Dismiss => n.dismiss(),
        };
        if let Err(e) = result {
            debug!(error = %e, "notification failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(rms: f32) -> StatusEvent {
        StatusEvent::Recording {
            rms,
            hands_free: false,
        }
    }

    fn hf(rms: f32) -> StatusEvent {
        StatusEvent::Recording {
            rms,
            hands_free: true,
        }
    }

    fn done(chars: usize) -> StatusEvent {
        StatusEvent::Done {
            chars,
            latency_ms: 900,
        }
    }

    fn err(code: &'static str) -> StatusEvent {
        StatusEvent::Error { code }
    }

    /// Feeds events 50 ms apart; `None` in the list means a 1 s pause plus an idle call.
    fn run(s: &mut StatusNotes, now: &mut Instant, evs: &[Option<StatusEvent>]) -> Vec<Note> {
        let mut notes = Vec::new();
        for e in evs {
            match e {
                Some(e) => {
                    *now += Duration::from_millis(50);
                    notes.extend(s.on_status(e, *now));
                }
                None => {
                    *now += Duration::from_secs(1);
                    notes.extend(s.on_idle(*now));
                }
            }
        }
        notes
    }

    #[test]
    fn normal_utterance_shows_no_bubble() {
        let mut s = StatusNotes::default();
        let mut now = Instant::now();
        let evs = [
            Some(rec(0.1)),
            Some(rec(0.2)),
            None,
            Some(StatusEvent::Transcribing),
            Some(StatusEvent::Polishing),
            Some(done(12)),
            None,
            Some(rec(0.1)),
            Some(StatusEvent::Cancelled),
            Some(rec(0.0)),
            None,
            Some(done(0)),
            Some(StatusEvent::MicReady),
        ];
        assert_eq!(run(&mut s, &mut now, &evs), vec![]);
        assert!(!s.wants_idle());
    }

    #[test]
    fn hands_free_hint_shows_once_and_closes_when_recording_ends() {
        let mut s = StatusNotes::default();
        let mut now = Instant::now();
        let evs = [Some(rec(0.1)), Some(hf(0.1)), Some(hf(0.2)), Some(hf(0.3))];
        assert_eq!(
            run(&mut s, &mut now, &evs),
            vec![Note::Show(NotifyStatus::HandsFree)]
        );
        assert!(s.wants_idle());
        // Ticks stopped: the idle call closes it; later events add nothing.
        let rest = [None, Some(StatusEvent::Transcribing), Some(done(5)), None];
        assert_eq!(run(&mut s, &mut now, &rest), vec![Note::Dismiss]);

        // Cancel closes it at once.
        let mut s = StatusNotes::default();
        let mut now = Instant::now();
        let evs = [Some(rec(0.1)), Some(hf(0.1)), Some(StatusEvent::Cancelled)];
        assert_eq!(
            run(&mut s, &mut now, &evs),
            vec![Note::Show(NotifyStatus::HandsFree), Note::Dismiss]
        );
    }

    #[test]
    fn previous_utterance_events_do_not_close_the_hint() {
        let mut s = StatusNotes::default();
        let mut now = Instant::now();
        // B goes hands-free while A is still being processed.
        let evs = [
            Some(rec(0.1)),
            Some(hf(0.1)),
            Some(StatusEvent::Transcribing),
            Some(hf(0.1)),
            Some(StatusEvent::Polishing),
            Some(done(4)),
            Some(hf(0.1)),
        ];
        assert_eq!(
            run(&mut s, &mut now, &evs),
            vec![Note::Show(NotifyStatus::HandsFree)]
        );
        // Idle while B is still recording (no gap yet) keeps it.
        let now = s.last_recording.unwrap() + Duration::from_millis(100);
        assert_eq!(s.on_idle(now), None);
    }

    #[test]
    fn error_codes() {
        assert_eq!(
            error_kind("mic_not_found"),
            Some(NotifyErrorKind::MicrophoneUnavailable)
        );
        assert_eq!(
            error_kind("mic_lost"),
            Some(NotifyErrorKind::MicrophoneUnavailable)
        );
        assert_eq!(
            error_kind("hotkey_failed"),
            Some(NotifyErrorKind::HotkeyUnavailable)
        );
        assert_eq!(
            error_kind("asr_failed"),
            Some(NotifyErrorKind::RecognitionFailed)
        );
        assert_eq!(error_kind("translate_failed"), Some(NotifyErrorKind::Other));
        for quiet in QUIET_CODES {
            assert_eq!(error_kind(quiet), None, "{quiet}");
        }
        for inject in ["inject_failed", "inject_no_input", "inject_partial"] {
            assert_eq!(error_kind(inject), None, "{inject}");
        }
    }

    #[test]
    fn quiet_notice_after_done_shows_nothing() {
        let mut s = StatusNotes::default();
        let mut now = Instant::now();
        assert_eq!(
            run(
                &mut s,
                &mut now,
                &[Some(done(3)), Some(err("polish_timed_out"))]
            ),
            vec![]
        );
    }

    #[test]
    fn error_stays_until_a_new_recording_starts() {
        let shown = Note::Show(NotifyStatus::Error(NotifyErrorKind::RecognitionFailed));
        let mut s = StatusNotes::default();
        let mut now = Instant::now();
        let evs = [
            Some(rec(0.1)),
            None,
            Some(StatusEvent::Transcribing),
            Some(err("asr_failed")),
            Some(done(0)),
            Some(StatusEvent::Cancelled),
            None,
        ];
        assert_eq!(run(&mut s, &mut now, &evs), vec![shown]);
        // The next recording closes it, once.
        assert_eq!(
            run(&mut s, &mut now, &[Some(rec(0.1)), Some(rec(0.1))]),
            vec![Note::Dismiss]
        );
    }

    #[test]
    fn error_during_recording_stays_while_recording_goes_on() {
        let mic = Note::Show(NotifyStatus::Error(NotifyErrorKind::MicrophoneUnavailable));
        let mut s = StatusNotes::default();
        let mut now = Instant::now();
        let evs = [
            Some(rec(0.1)),
            Some(err("mic_lost")),
            Some(rec(0.1)),
            Some(rec(0.1)),
        ];
        assert_eq!(run(&mut s, &mut now, &evs), vec![mic]);
        // An earlier utterance's failure arriving mid-recording also stays.
        let late = Note::Show(NotifyStatus::Error(NotifyErrorKind::RecognitionFailed));
        assert_eq!(
            run(
                &mut s,
                &mut now,
                &[Some(err("asr_failed")), Some(rec(0.1)), Some(rec(0.1))]
            ),
            vec![late]
        );
    }

    #[test]
    fn hint_never_replaces_an_error() {
        let mic = Note::Show(NotifyStatus::Error(NotifyErrorKind::MicrophoneUnavailable));
        let mut s = StatusNotes::default();
        let mut now = Instant::now();
        let evs = [
            Some(rec(0.1)),
            Some(hf(0.1)),
            Some(err("mic_lost")),
            Some(hf(0.1)),
            Some(hf(0.1)),
            None,
        ];
        assert_eq!(
            run(&mut s, &mut now, &evs),
            vec![Note::Show(NotifyStatus::HandsFree), mic]
        );
        // Next recording closes the error; going hands-free shows the hint again.
        assert_eq!(
            run(&mut s, &mut now, &[Some(rec(0.1)), Some(hf(0.1))]),
            vec![Note::Dismiss, Note::Show(NotifyStatus::HandsFree)]
        );
    }

    #[test]
    fn handle_forwards_notes() {
        let (tx, rx) = mpsc::channel();
        let notes = Notes::with_sender(tx);
        notes.show(NotifyStatus::TextKept);
        notes.send(Note::Dismiss);
        assert_eq!(rx.try_recv(), Ok(Note::Show(NotifyStatus::TextKept)));
        assert_eq!(rx.try_recv(), Ok(Note::Dismiss));
    }
}
