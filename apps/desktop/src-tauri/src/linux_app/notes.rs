//! Desktop notifications on Linux (T3.4 `DbusNotifier`): controller status events on Wayland,
//! where the overlay is off, plus IBus and kept-text notices on any session.
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

/// Turns the controller's status stream into notifications: one bubble per utterance,
/// updated in place (`Recording` arrives every 50 ms; only the first one shows).
#[derive(Debug, Default)]
pub struct StatusNotes {
    recording: bool,
}

impl StatusNotes {
    pub fn on_status(&mut self, ev: &StatusEvent) -> Option<Note> {
        let was_recording = std::mem::replace(
            &mut self.recording,
            matches!(ev, StatusEvent::Recording { .. }),
        );
        match ev {
            StatusEvent::Recording { .. } if was_recording => None,
            StatusEvent::Recording { .. } => Some(Note::Show(NotifyStatus::Recording)),
            StatusEvent::Transcribing => Some(Note::Show(NotifyStatus::Recognizing)),
            StatusEvent::Polishing => Some(Note::Show(NotifyStatus::Polishing)),
            StatusEvent::Done { chars: 0, .. } | StatusEvent::Cancelled => Some(Note::Dismiss),
            StatusEvent::Done { .. } => Some(Note::Show(NotifyStatus::Done)),
            StatusEvent::MicReady => None,
            StatusEvent::Error { code } => {
                error_kind(code).map(|k| Note::Show(NotifyStatus::Error(k)))
            }
        }
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

    #[test]
    fn one_bubble_per_utterance() {
        let mut s = StatusNotes::default();
        let evs = [
            rec(0.1),
            rec(0.2),
            rec(0.3),
            StatusEvent::Transcribing,
            StatusEvent::Polishing,
            StatusEvent::Done {
                chars: 12,
                latency_ms: 900,
            },
            rec(0.1),
            rec(0.1),
            StatusEvent::Cancelled,
        ];
        let notes: Vec<_> = evs.iter().filter_map(|e| s.on_status(e)).collect();
        assert_eq!(
            notes,
            vec![
                Note::Show(NotifyStatus::Recording),
                Note::Show(NotifyStatus::Recognizing),
                Note::Show(NotifyStatus::Polishing),
                Note::Show(NotifyStatus::Done),
                Note::Show(NotifyStatus::Recording),
                Note::Dismiss,
            ]
        );
    }

    #[test]
    fn nothing_recognised_closes_the_bubble() {
        let mut s = StatusNotes::default();
        s.on_status(&rec(0.0));
        s.on_status(&StatusEvent::Transcribing);
        assert_eq!(
            s.on_status(&StatusEvent::Done {
                chars: 0,
                latency_ms: 10
            }),
            Some(Note::Dismiss)
        );
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
    fn notice_after_done_does_not_replace_done() {
        let mut s = StatusNotes::default();
        s.on_status(&StatusEvent::Done {
            chars: 3,
            latency_ms: 1,
        });
        assert_eq!(
            s.on_status(&StatusEvent::Error {
                code: "polish_timed_out"
            }),
            None
        );
    }

    #[test]
    fn error_during_recording_resets_the_bubble() {
        let mut s = StatusNotes::default();
        s.on_status(&rec(0.1));
        assert_eq!(
            s.on_status(&StatusEvent::Error { code: "mic_lost" }),
            Some(Note::Show(NotifyStatus::Error(
                NotifyErrorKind::MicrophoneUnavailable
            )))
        );
        // The next recording shows again.
        assert_eq!(
            s.on_status(&rec(0.1)),
            Some(Note::Show(NotifyStatus::Recording))
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
