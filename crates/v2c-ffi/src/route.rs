//! Per-utterance choice of the speech recognition source (`tasks/plan.md` section 8.1).
//!
//! Pure: no I/O. LAN detection, cloud readiness and availability checks happen in Kotlin
//! (T2.4, T2.8); this only decides. Tested like the `trigger.rs` module of `v2c-core`.

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
    Cloud,
    System,
    Local,
}

/// A recognition source. `LanTea`/`LanBreeze` run on the detected LAN (see [`lan_service_id`]);
/// `Cloud` runs on the user's cloud recognition service.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum AsrSource {
    /// Nothing usable; see `AsrPlan::hint`.
    None,
    LanTea,
    LanBreeze,
    Cloud,
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
    /// Cloud recognition was forced but is not set up, has no readable key, or there is no
    /// network.
    CloudNotReady,
    /// System recognition was forced but is unavailable.
    SystemUnavailable,
    /// The local model was forced but is not downloaded.
    LocalModelNotReady,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct AsrPlan {
    pub primary: AsrSource,
    /// Used when `primary` fails, with the same PCM. `None` when there is nothing to fall
    /// back to, and always `None` for a forced source (never silently switches).
    pub fallback: AsrSource,
    /// Used when `fallback` fails too (LAN -> cloud -> local). `None` when the chain ends.
    pub fallback2: AsrSource,
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
            fallback2: AsrSource::None,
            retry_speak: false,
            hint,
        }
    }

    fn forced(primary: AsrSource) -> Self {
        AsrPlan {
            primary,
            fallback: AsrSource::None,
            fallback2: AsrSource::None,
            retry_speak: true,
            hint: AsrHint::None,
        }
    }

    /// `primary` with fallbacks that reuse its PCM (cloud, then local; absent ones skipped).
    fn chain(primary: AsrSource, cloud: bool, local: bool) -> Self {
        let mut rest = [
            cloud.then_some(AsrSource::Cloud),
            local.then_some(AsrSource::Local),
        ]
        .into_iter()
        .flatten();
        let fallback = rest.next().unwrap_or(AsrSource::None);
        let fallback2 = rest.next().unwrap_or(AsrSource::None);
        AsrPlan {
            primary,
            fallback,
            fallback2,
            retry_speak: fallback == AsrSource::None,
            hint: AsrHint::None,
        }
    }
}

/// Decide the source for one utterance. `cloud_ready`: a cloud recognition service is set
/// up, its key is readable, and the phone has validated network.
///
/// Auto order: LAN -> cloud -> phone (system recognition, then the local model).
#[uniffi::export]
pub fn choose_asr(
    lan: Lan,
    cloud_ready: bool,
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
        AsrPick::Cloud => {
            if cloud_ready {
                AsrPlan::forced(AsrSource::Cloud)
            } else {
                AsrPlan::unavailable(AsrHint::CloudNotReady)
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
            if lan_up {
                // The same PCM goes on to the cloud, then the local model.
                AsrPlan::chain(AsrSource::LanTea, cloud_ready, local_model_ready)
            } else if cloud_ready {
                AsrPlan::chain(AsrSource::Cloud, false, local_model_ready)
            } else if system_available {
                // System recognition records by itself, so any fallback needs a new utterance.
                AsrPlan {
                    primary: AsrSource::System,
                    fallback: if local_model_ready {
                        AsrSource::Local
                    } else {
                        AsrSource::None
                    },
                    fallback2: AsrSource::None,
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
    use AsrSource as S;

    fn plan(lan: Lan, cloud: bool, sys: bool, local: bool, pick: AsrPick) -> AsrPlan {
        choose_asr(lan, cloud, sys, local, pick)
    }

    fn p(primary: S, fallback: S, fallback2: S, retry: bool, hint: AsrHint) -> AsrPlan {
        AsrPlan {
            primary,
            fallback,
            fallback2,
            retry_speak: retry,
            hint,
        }
    }

    const BOOLS: [bool; 2] = [false, true];

    // Table row 1: LAN reachable -> TEA; on failure cloud (if ready) then local (if
    // downloaded), all with the same PCM; no fallback at all -> speak again.
    #[test]
    fn lan_reachable_chains_cloud_then_local() {
        let n = AsrHint::None;
        for lan in [Lan::Tn, Lan::Rd] {
            for sys in BOOLS {
                assert_eq!(
                    plan(lan, true, sys, true, Auto),
                    p(S::LanTea, S::Cloud, S::Local, false, n)
                );
                assert_eq!(
                    plan(lan, true, sys, false, Auto),
                    p(S::LanTea, S::Cloud, S::None, false, n)
                );
                assert_eq!(
                    plan(lan, false, sys, true, Auto),
                    p(S::LanTea, S::Local, S::None, false, n)
                );
                assert_eq!(
                    plan(lan, false, sys, false, Auto),
                    p(S::LanTea, S::None, S::None, true, n)
                );
            }
        }
    }

    // Table row 2: LAN down + cloud ready -> cloud; failure -> local if downloaded, else
    // speak again.
    #[test]
    fn lan_down_with_cloud_uses_cloud_then_local() {
        let n = AsrHint::None;
        for sys in BOOLS {
            assert_eq!(
                plan(Lan::Unreachable, true, sys, true, Auto),
                p(S::Cloud, S::Local, S::None, false, n)
            );
            assert_eq!(
                plan(Lan::Unreachable, true, sys, false, Auto),
                p(S::Cloud, S::None, S::None, true, n)
            );
        }
    }

    // Table row 3: LAN down, no cloud -> system, fallback local (user must re-speak).
    #[test]
    fn lan_down_no_cloud_uses_system_with_local_fallback() {
        let n = AsrHint::None;
        assert_eq!(
            plan(Lan::Unreachable, false, true, true, Auto),
            p(S::System, S::Local, S::None, true, n)
        );
        assert_eq!(
            plan(Lan::Unreachable, false, true, false, Auto),
            p(S::System, S::None, S::None, true, n)
        );
    }

    // Table row 4: nothing works -> hint.
    #[test]
    fn nothing_available_gives_hint() {
        assert_eq!(
            plan(Lan::Unreachable, false, false, false, Auto),
            AsrPlan::unavailable(AsrHint::NoSourceAvailable)
        );
    }

    // Implied by row 4's conditions: system unavailable but local ready -> local, no hint.
    #[test]
    fn lan_down_system_unavailable_uses_local() {
        assert_eq!(
            plan(Lan::Unreachable, false, false, true, Auto),
            p(S::Local, S::None, S::None, true, AsrHint::None)
        );
    }

    // Cloud goes ahead of system recognition even when the system one works.
    #[test]
    fn cloud_beats_system_in_auto() {
        assert_eq!(
            plan(Lan::Unreachable, true, true, false, Auto).primary,
            S::Cloud
        );
    }

    #[test]
    fn forced_sources_never_fall_back() {
        for lan in [Lan::Tn, Lan::Rd, Lan::Unreachable] {
            for cloud in BOOLS {
                for sys in BOOLS {
                    for local in BOOLS {
                        for pick in [Tea, Breeze, Cloud, System, Local] {
                            let r = plan(lan, cloud, sys, local, pick);
                            let ctx = format!("{lan:?} {cloud} {sys} {local} {pick:?}");
                            assert_eq!(r.fallback, S::None, "{ctx}");
                            assert_eq!(r.fallback2, S::None, "{ctx}");
                            let usable = match pick {
                                Tea | Breeze => lan != Lan::Unreachable,
                                Cloud => cloud,
                                System => sys,
                                Local => local,
                                Auto => unreachable!(),
                            };
                            if usable {
                                assert_eq!(r.hint, AsrHint::None, "{ctx}");
                                assert_ne!(r.primary, S::None, "{ctx}");
                                assert!(r.retry_speak, "{ctx}");
                            } else {
                                assert_eq!(r.primary, S::None, "{ctx}");
                                assert_ne!(r.hint, AsrHint::None, "{ctx}");
                            }
                        }
                    }
                }
            }
        }
    }

    // Forced cloud without a ready cloud service: a hint, never another source.
    #[test]
    fn forced_cloud_not_ready_hints_instead_of_switching() {
        for lan in [Lan::Tn, Lan::Unreachable] {
            assert_eq!(
                plan(lan, false, true, true, Cloud),
                AsrPlan::unavailable(AsrHint::CloudNotReady)
            );
        }
    }

    #[test]
    fn forced_unavailable_returns_matching_hint() {
        let down = Lan::Unreachable;
        assert_eq!(
            plan(down, true, true, true, Tea).hint,
            AsrHint::LanUnreachable
        );
        assert_eq!(
            plan(down, true, true, true, Breeze).hint,
            AsrHint::LanUnreachable
        );
        assert_eq!(
            plan(Lan::Tn, true, false, true, System).hint,
            AsrHint::SystemUnavailable
        );
        assert_eq!(
            plan(Lan::Tn, true, true, false, Local).hint,
            AsrHint::LocalModelNotReady
        );
    }

    #[test]
    fn forced_choice_maps_to_its_source() {
        let f = false;
        assert_eq!(plan(Lan::Rd, f, f, f, Tea).primary, S::LanTea);
        assert_eq!(plan(Lan::Rd, f, f, f, Breeze).primary, S::LanBreeze);
        assert_eq!(plan(Lan::Unreachable, true, f, f, Cloud).primary, S::Cloud);
        assert_eq!(
            plan(Lan::Unreachable, f, true, f, System).primary,
            S::System
        );
        assert_eq!(plan(Lan::Unreachable, f, f, true, Local).primary, S::Local);
    }

    #[test]
    fn lan_service_ids() {
        assert_eq!(lan_service_id(Lan::Tn, S::LanTea).unwrap(), "tn-tea-asr");
        assert_eq!(
            lan_service_id(Lan::Rd, S::LanBreeze).unwrap(),
            "rd-breeze-asr"
        );
        assert_eq!(lan_service_id(Lan::Unreachable, S::LanTea), None);
        assert_eq!(lan_service_id(Lan::Tn, S::System), None);
        assert_eq!(lan_service_id(Lan::Tn, S::Cloud), None);
    }
}
