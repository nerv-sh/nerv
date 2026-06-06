pub use crate::proto::fig::*;

mod internal {
    use std::fmt::Display;

    use crate::proto::fig::result::Result as FigResultEnum;
    use crate::proto::fig::{NotificationType, Result as FigResult};

    impl serde::Serialize for NotificationType {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {
            serializer.serialize_str(match self {
                NotificationType::All => "all",
                NotificationType::NotifyOnEditbuffferChange => "editbuffer_change",
                NotificationType::NotifyOnSettingsChange => "settings_change",
                NotificationType::NotifyOnPrompt => "prompt",
                NotificationType::NotifyOnLocationChange => "location_change",
                NotificationType::NotifyOnProcessChanged => "process_change",
                NotificationType::NotifyOnKeybindingPressed => "keybinding_pressed",
                NotificationType::NotifyOnFocusChanged => "focus_change",
                NotificationType::NotifyOnHistoryUpdated => "history_update",
                NotificationType::NotifyOnApplicationUpdateAvailable => {
                    "application_update_available"
                }
                NotificationType::NotifyOnLocalStateChanged => "local_state_change",
                NotificationType::NotifyOnEvent => "event",
                NotificationType::NotifyOnAccessibilityChange => "accessibility_change",
            })
        }
    }

    impl<E> From<Result<(), E>> for FigResult
    where
        E: Display,
    {
        fn from(value: Result<(), E>) -> Self {
            match value {
                Ok(()) => FigResult {
                    result: FigResultEnum::Ok.into(),
                    error: None,
                },
                Err(e) => FigResult {
                    result: FigResultEnum::Error.into(),
                    error: Some(e.to_string()),
                },
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// NotificationType serializes to a stable string key. The
        /// daemon's notification stream is consumed by external
        /// listeners (figterm shim, future UI plugins) that match on
        /// these exact strings; locking the mapping catches any
        /// accidental rename at refactor time.
        #[test]
        fn notification_type_serialize_keys() {
            let cases = [
                (NotificationType::All, "\"all\""),
                (
                    NotificationType::NotifyOnEditbuffferChange,
                    "\"editbuffer_change\"",
                ),
                (
                    NotificationType::NotifyOnSettingsChange,
                    "\"settings_change\"",
                ),
                (NotificationType::NotifyOnPrompt, "\"prompt\""),
                (NotificationType::NotifyOnFocusChanged, "\"focus_change\""),
                (NotificationType::NotifyOnEvent, "\"event\""),
            ];
            for (input, want) in cases {
                let got = serde_json::to_string(&input).expect("serialize");
                assert_eq!(got, want, "wrong key for {input:?}");
            }
        }

        /// `Result<(), E>` → `FigResult` is the standard daemon
        /// command response shape. Lock both arms.
        #[test]
        fn fig_result_from_ok_arm() {
            let r: FigResult = Ok::<(), &str>(()).into();
            assert_eq!(r.result, FigResultEnum::Ok as i32);
            assert!(r.error.is_none());
        }

        #[test]
        fn fig_result_from_err_arm() {
            let r: FigResult = Err::<(), &str>("boom").into();
            assert_eq!(r.result, FigResultEnum::Error as i32);
            assert_eq!(r.error.as_deref(), Some("boom"));
        }
    }
}
