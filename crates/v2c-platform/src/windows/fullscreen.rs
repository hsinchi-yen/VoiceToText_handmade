//! Fullscreen detection for the VRAM guard (T1.10b).
//!
//! [`crate::AppInfo::fullscreen`] (T0.3) compares the foreground window with its monitor, which
//! catches borderless-fullscreen games and F11 windows. Exclusive D3D fullscreen and
//! presentation mode are reported by the shell through `SHQueryUserNotificationState`, which
//! [`foreground_fullscreen`] adds on top.

use windows_sys::Win32::UI::Shell::{
    SHQueryUserNotificationState, QUERY_USER_NOTIFICATION_STATE, QUNS_ACCEPTS_NOTIFICATIONS,
    QUNS_APP, QUNS_BUSY, QUNS_NOT_PRESENT, QUNS_PRESENTATION_MODE, QUNS_QUIET_TIME,
    QUNS_RUNNING_D3D_FULL_SCREEN,
};

use crate::AppInfo;

/// `QUERY_USER_NOTIFICATION_STATE`, as an enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationState {
    /// Screen saver, locked machine or fast user switching.
    NotPresent,
    /// A fullscreen application is running.
    Busy,
    /// An exclusive-mode Direct3D application (typically a game) is running.
    D3dFullscreen,
    /// Presentation mode is on.
    PresentationMode,
    /// Nothing special.
    AcceptsNotifications,
    /// First hour after a new user's first sign-in.
    QuietTime,
    /// A Windows Store app is running fullscreen.
    App,
    Other(i32),
}

impl NotificationState {
    fn from_raw(raw: QUERY_USER_NOTIFICATION_STATE) -> Self {
        match raw {
            QUNS_NOT_PRESENT => Self::NotPresent,
            QUNS_BUSY => Self::Busy,
            QUNS_RUNNING_D3D_FULL_SCREEN => Self::D3dFullscreen,
            QUNS_PRESENTATION_MODE => Self::PresentationMode,
            QUNS_ACCEPTS_NOTIFICATIONS => Self::AcceptsNotifications,
            QUNS_QUIET_TIME => Self::QuietTime,
            QUNS_APP => Self::App,
            other => Self::Other(other),
        }
    }

    /// A fullscreen game / app or a presentation is in front: v2c should leave the GPU.
    /// `App` (a Store / UWP app running fullscreen, e.g. Xbox app games) counts too: those
    /// render through the GPU like any other fullscreen game.
    pub fn is_fullscreen(self) -> bool {
        matches!(
            self,
            Self::Busy | Self::D3dFullscreen | Self::PresentationMode | Self::App
        )
    }
}

/// Current shell notification state, or `None` if the call fails.
pub fn notification_state() -> Option<NotificationState> {
    let mut raw: QUERY_USER_NOTIFICATION_STATE = 0;
    // SAFETY: `raw` is a valid out pointer for the duration of the call.
    let hr = unsafe { SHQueryUserNotificationState(&mut raw) };
    (hr >= 0).then(|| NotificationState::from_raw(raw))
}

/// Value for the guard's `ForegroundInfo::fullscreen`: the foreground window covers its
/// monitor, or the shell reports a fullscreen / D3D exclusive / presentation state.
pub fn foreground_fullscreen(app: Option<&AppInfo>) -> bool {
    app.is_some_and(|a| a.fullscreen)
        || notification_state().is_some_and(NotificationState::is_fullscreen)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_states_map() {
        assert_eq!(
            NotificationState::from_raw(QUNS_RUNNING_D3D_FULL_SCREEN),
            NotificationState::D3dFullscreen
        );
        assert_eq!(
            NotificationState::from_raw(99),
            NotificationState::Other(99)
        );
        assert!(NotificationState::D3dFullscreen.is_fullscreen());
        assert!(NotificationState::Busy.is_fullscreen());
        assert!(NotificationState::PresentationMode.is_fullscreen());
        assert!(!NotificationState::AcceptsNotifications.is_fullscreen());
        assert!(!NotificationState::NotPresent.is_fullscreen());
        assert!(NotificationState::App.is_fullscreen());
        assert!(!NotificationState::QuietTime.is_fullscreen());
    }

    #[test]
    fn window_rect_flag_wins() {
        let app = AppInfo {
            exe: "game.exe".into(),
            elevated: false,
            fullscreen: true,
        };
        assert!(foreground_fullscreen(Some(&app)));
    }

    #[test]
    fn query_does_not_panic() {
        let _ = notification_state();
        let _ = foreground_fullscreen(None);
    }
}
