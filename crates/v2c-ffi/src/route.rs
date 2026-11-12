//! Per-utterance choice of the speech recognition source (`tasks/plan.md` section 8.1).
//!
//! Pure: no I/O. LAN detection and availability checks happen in Kotlin (T2.4); this only
//! decides. Tested like `v2c-core`'s `trigger.rs`.

/// Result of the LAN probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum Lan {
    Tn,
    Rd,
    Unreachable,
}

/// The user's manual choice on the keyboard. `Auto` follows the routing table; anything else
/// forces that source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum AsrPick {
    Auto,
    Tea,
    Breeze,
    System,
    Local,
}

/// A recognition source. `LanTea`/`LanBreeze` run on the detected LAN (see [`lan_service_id`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum AsrSource {
    /// Nothing usable; see `AsrPlan::hint`.
    None,
    LanTea,
    LanBreeze,
    System,
    Local,
}

/// Why no source is offered; the keyboard maps these to messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum AsrHint {
    None,
    /// Auto mode and nothing works: show "install/update Google speech services" and
    /// "download the local model".
    NoSourceAvailable,
    /// A LAN source was forced but no LAN is reachable.
    LanUnreachable,
    /// System recognition was forced but is unavailable.
    SystemUnavailable,
    /// The local model was forced but is not downloaded.
    LocalModelNotReady,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct AsrPlan {
    pub primary: AsrSource,
    /// Used when `primary` fails. `None` when there is nothing to fall back to, and always
    /// `None` for a forced source (never silently switches).
    pub fallback: AsrSource,
    /// If `primary` fails, the user has to speak again: there is no fallback, or the primary
    /// (system recognition) records the audio itself so a fallback cannot reuse it.
    pub retry_speak: bool,
    pub hint: AsrHint,
}

impl AsrPlan {
    fn unavailable(hint: AsrHint) -> Self {
        AsrPlan {
            primary: AsrSource::None,
            fallback: AsrSource::None,
            retry_speak: false,
            hint,
        }
    }

    fn forced(primary: AsrSource) -> Self {
        AsrPlan {
            primary,
            fallback: AsrSource::None,
            retry_speak: true,
            hint: AsrHint::None,
        }
    }
}

/// Decide the source for one utterance.
#[uniffi::export]
pub fn choose_asr(
    lan: Lan,
    system_available: bool,
    local_model_ready: bool,
    pick: AsrPick,
) -> AsrPlan {
    let lan_up = lan != Lan::Unreachable;
    match pick {
        AsrPick::Tea | AsrPick::Breeze => {
            if lan_up {
                AsrPlan::forced(if pick == AsrPick::Tea {
                    AsrSource::LanTea
                } else {
                    AsrSource::LanBreeze
                })
            } else {
                AsrPlan::unavailable(AsrHint::LanUnreachable)
            }
        }
        AsrPick::System => {
            if system_available {
                AsrPlan::forced(AsrSource::System)
            } else {
                AsrPlan::unavailable(AsrHint::SystemUnavailable)
            }
        }
        AsrPick::Local => {
            if local_model_ready {
                AsrPlan::forced(AsrSource::Local)
            } else {
                AsrPlan::unavailable(AsrHint::LocalModelNotReady)
            }
        }
        AsrPick::Auto => {
            let local_fallback = if local_model_ready {
                AsrSource::Local
            } else {
                AsrSource::None
            };
            if lan_up {
                // The same PCM goes to the local model, so no re-speaking when it exists.
                AsrPlan {
                    primary: AsrSource::LanTea,
                    fallback: local_fallback,
                    retry_speak: local_fallback == AsrSource::None,
                    hint: AsrHint::None,
                }
            } else if system_available {
                // System recognition records by itself, so any fallback needs a new utterance.
                AsrPlan {
                    primary: AsrSource::System,
                    fallback: local_fallback,
                    retry_speak: true,
                    hint: AsrHint::None,
                }
            } else if local_model_ready {
                AsrPlan::forced(AsrSource::Local)
            } else {
                AsrPlan::unavailable(AsrHint::NoSourceAvailable)
            }
        }
    }
}

/// Service id (as in the bundled preset files) for a LAN source, or `None` when the source is
/// not a LAN source or no LAN is reachable.
#[uniffi::export]
pub fn lan_service_id(lan: Lan, source: AsrSource) -> Option<String> {
    let prefix = match lan {
        Lan::Tn => "tn",
        Lan::Rd => "rd",
        Lan::Unreachable => return None,
    };
    match source {
        AsrSource::LanTea => Some(format!("{prefix}-tea-asr")),
        AsrSource::LanBreeze => Some(format!("{prefix}-breeze-asr")),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use AsrPick::*;

    fn plan(lan: Lan, sys: bool, local: bool, pick: AsrPick) -> AsrPlan {
        choose_asr(lan, sys, local, pick)
    }

    fn p(primary: AsrSource, fallback: AsrSource, retry: bool, hint: AsrHint) -> AsrPlan {
        AsrPlan {
            primary,
            fallback,
            retry_speak: retry,
            hint,
        }
    }

    // Table row 1: LAN reachable -> TEA, fallback local (if downloaded) else retry.
    #[test]
    fn lan_reachable_uses_tea_with_local_fallback() {
        for lan in [Lan::Tn, Lan::Rd] {
            for sys in [false, true] {
                assert_eq!(
                    plan(lan, sys, true, Auto),
                    p(AsrSource::LanTea, AsrSource::Local, false, AsrHint::None)
                );
                assert_eq!(
                    plan(lan, sys, false, Auto),
                    p(AsrSource::LanTea, AsrSource::None, true, AsrHint::None)
                );
            }
        }
    }

    // Table row 2: LAN unreachable -> system, fallback local (user must re-speak).
    #[test]
    fn lan_down_uses_system_with_local_fallback() {
        assert_eq!(
            plan(Lan::Unreachable, true, true, Auto),
            p(AsrSource::System, AsrSource::Local, true, AsrHint::None)
        );
        assert_eq!(
            plan(Lan::Unreachable, true, false, Auto),
            p(AsrSource::System, AsrSource::None, true, AsrHint::None)
        );
    }

    // Table row 3: nothing works -> hint.
    #[test]
    fn nothing_available_gives_hint() {
        assert_eq!(
            plan(Lan::Unreachable, false, false, Auto),
            AsrPlan::unavailable(AsrHint::NoSourceAvailable)
        );
    }

    // Implied by row 3's conditions: system unavailable but local ready -> local, no hint.
    #[test]
    fn lan_down_system_unavailable_uses_local() {
        assert_eq!(
            plan(Lan::Unreachable, false, true, Auto),
            p(AsrSource::Local, AsrSource::None, true, AsrHint::None)
        );
    }

    #[test]
    fn forced_sources_never_fall_back() {
        for lan in [Lan::Tn, Lan::Rd, Lan::Unreachable] {
            for sys in [false, true] {
                for local in [false, true] {
                    for pick in [Tea, Breeze, System, Local] {
                        let r = plan(lan, sys, local, pick);
                        assert_eq!(
                            r.fallback,
                            AsrSource::None,
                            "{lan:?} {sys} {local} {pick:?}"
                        );
                        let usable = match pick {
                            Tea | Breeze => lan != Lan::Unreachable,
                            System => sys,
                            Local => local,
                            Auto => unreachable!(),
                        };
                        if usable {
                            assert_eq!(r.hint, AsrHint::None);
                            assert_ne!(r.primary, AsrSource::None);
                            assert!(r.retry_speak);
                        } else {
                            assert_eq!(r.primary, AsrSource::None);
                            assert_ne!(r.hint, AsrHint::None);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn forced_unavailable_returns_matching_hint() {
        assert_eq!(
            plan(Lan::Unreachable, true, true, Tea).hint,
            AsrHint::LanUnreachable
        );
        assert_eq!(
            plan(Lan::Unreachable, true, true, Breeze).hint,
            AsrHint::LanUnreachable
        );
        assert_eq!(
            plan(Lan::Tn, false, true, System).hint,
            AsrHint::SystemUnavailable
        );
        assert_eq!(
            plan(Lan::Tn, true, false, Local).hint,
            AsrHint::LocalModelNotReady
        );
    }

    #[test]
    fn forced_choice_maps_to_its_source() {
        assert_eq!(plan(Lan::Rd, false, false, Tea).primary, AsrSource::LanTea);
        assert_eq!(
            plan(Lan::Rd, false, false, Breeze).primary,
            AsrSource::LanBreeze
        );
        assert_eq!(
            plan(Lan::Unreachable, true, false, System).primary,
            AsrSource::System
        );
        assert_eq!(
            plan(Lan::Unreachable, false, true, Local).primary,
            AsrSource::Local
        );
    }

    #[test]
    fn lan_service_ids() {
        assert_eq!(
            lan_service_id(Lan::Tn, AsrSource::LanTea).unwrap(),
            "tn-tea-asr"
        );
        assert_eq!(
            lan_service_id(Lan::Rd, AsrSource::LanBreeze).unwrap(),
            "rd-breeze-asr"
        );
        assert_eq!(lan_service_id(Lan::Unreachable, AsrSource::LanTea), None);
        assert_eq!(lan_service_id(Lan::Tn, AsrSource::System), None);
    }
}
