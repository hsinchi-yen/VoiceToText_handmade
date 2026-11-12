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

/// Turns the controller's status stream into notifications. Progress is silent; an error or
/// the hands-free hint opens a bubble. The hint closes when the recording ends; an error from
/// an earlier utterance closes when the next recording starts (`Recording` arrives every
/// 50 ms; only state changes produce a note).
#[derive(Debug, Default)]
pub struct StatusNotes {
    recording: bool,
    open: Open,
}

impl StatusNotes {
    pub fn on_status(&mut self, ev: &StatusEvent) -> Option<Note> {
        let was_recording = std::mem::replace(
            &mut self.recording,
            matches!(ev, StatusEvent::Recording { .. }),
        );
        match ev {
            StatusEvent::Recording { hands_free, .. } => {
                if *hands_free && self.open != Open::HandsFree {
                    // Replaces whatever bubble is open.
                    self.open = Open::HandsFree;
                    Some(Note::Show(NotifyStatus::HandsFree))
                } else if !was_recording && self.open == Open::Error {
                    self.open = Open::Nothing;
                    Some(Note::Dismiss)
                } else {
                    None
                }
            }
            StatusEvent::Transcribing
            | StatusEvent::Polishing
            | StatusEvent::Done { .. }
            | StatusEvent::Cancelled => self.close_hint(),
            StatusEvent::MicReady => None,
            StatusEvent::Error { code } => error_kind(code).map(|k| {
                self.open = Open::Error;
                Note::Show(NotifyStatus::Error(k))
            }),
        }
    }

    /// Closes the hands-free hint once the recording is over; leaves an error bubble alone.
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

    #[test]
    fn normal_utterance_shows_no_bubble() {
        let mut s = StatusNotes::default();
        let evs = [
            rec(0.1),
            rec(0.2),
            StatusEvent::Transcribing,
            StatusEvent::Polishing,
            done(12),
            rec(0.1),
            StatusEvent::Cancelled,
            rec(0.0),
            StatusEvent::Transcribing,
            done(0),
            StatusEvent::MicReady,
        ];
        let notes: Vec<_> = evs.iter().filter_map(|e| s.on_status(e)).collect();
        assert_eq!(notes, vec![]);
    }

    #[test]
    fn hands_free_hint_shows_once_and_closes_when_recording_ends() {
        for end in [StatusEvent::Transcribing, StatusEvent::Cancelled, done(5)] {
            let mut s = StatusNotes::default();
            let evs = [
                rec(0.1),
                hf(0.1),
                hf(0.2),
                hf(0.3),
                end.clone(),
                StatusEvent::Polishing,
                done(5),
            ];
            let notes: Vec<_> = evs.iter().filter_map(|e| s.on_status(e)).collect();
            assert_eq!(
                notes,
                vec![Note::Show(NotifyStatus::HandsFree), Note::Dismiss],
                "{end:?}"
            );
        }
    }

    #[test]
    fn nothing_recognised_closes_the_hint() {
        let mut s = StatusNotes::default();
        s.on_status(&hf(0.0));
        assert_eq!(s.on_status(&done(0)), Some(Note::Dismiss));
        assert_eq!(s.on_status(&rec(0.1)), None);
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
        s.on_status(&done(3));
        assert_eq!(
            s.on_status(&StatusEvent::Error {
                code: "polish_timed_out"
            }),
            None
        );
    }

    #[test]
    fn error_after_done_stays_until_the_next_recording() {
        let mut s = StatusNotes::default();
        s.on_status(&rec(0.1));
        s.on_status(&StatusEvent::Transcribing);
        assert_eq!(
            s.on_status(&StatusEvent::Error { code: "asr_failed" }),
            Some(Note::Show(NotifyStatus::Error(
                NotifyErrorKind::RecognitionFailed
            )))
        );
        // A later Done or Cancelled of the same utterance does not close it.
        assert_eq!(s.on_status(&done(0)), None);
        assert_eq!(s.on_status(&StatusEvent::Cancelled), None);
        // The next recording does, once.
        assert_eq!(s.on_status(&rec(0.1)), Some(Note::Dismiss));
        assert_eq!(s.on_status(&rec(0.1)), None);
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
        // The next recording closes the error bubble; going hands-free replaces it.
        assert_eq!(s.on_status(&rec(0.1)), Some(Note::Dismiss));
        s.on_status(&StatusEvent::Error { code: "mic_lost" });
        s.on_status(&StatusEvent::Cancelled);
        assert_eq!(
            s.on_status(&hf(0.1)),
            Some(Note::Show(NotifyStatus::HandsFree))
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
