use serde::{Deserialize, Serialize};

use crate::parser::Action;
use crate::telegram;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct PendingPressure {
    pub sys: i32,
    pub dia: i32,
    pub pulse: Option<i32>,
}

impl From<PendingPressure> for Action {
    fn from(pending: PendingPressure) -> Self {
        Action::Pressure {
            sys: pending.sys,
            dia: pending.dia,
            pulse: pending.pulse,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", content = "data")]
pub enum UserState {
    None,
    AwaitingClassification { raw_text: String },
    AwaitingPressureConfirmation(PendingPressure),
    AwaitingMultipleChoice { options: Vec<PendingPressure> },
}

#[derive(Debug, Clone, PartialEq)]
pub enum TextTransition {
    Cancel,
    SavePressure(PendingPressure),
    ForcePressure { raw_text: String },
    ForceCost { raw_text: String },
    ProcessFresh { discard_existing: bool },
}

impl UserState {
    pub fn parse_or_none(raw: &str) -> Self {
        serde_json::from_str(raw).unwrap_or(Self::None)
    }

    pub fn text_transition(&self, text: &str) -> TextTransition {
        if text == telegram::BTN_CANCEL {
            return TextTransition::Cancel;
        }

        match self {
            Self::AwaitingPressureConfirmation(pending) if text == telegram::BTN_SAVE => {
                TextTransition::SavePressure(pending.clone())
            }
            Self::AwaitingPressureConfirmation(_) => TextTransition::ProcessFresh {
                discard_existing: true,
            },
            Self::AwaitingClassification { raw_text } if text == telegram::BTN_PRESSURE => {
                TextTransition::ForcePressure {
                    raw_text: raw_text.clone(),
                }
            }
            Self::AwaitingClassification { raw_text } if text == telegram::BTN_COST => {
                TextTransition::ForceCost {
                    raw_text: raw_text.clone(),
                }
            }
            Self::AwaitingClassification { .. } | Self::AwaitingMultipleChoice { .. } => {
                TextTransition::ProcessFresh {
                    discard_existing: true,
                }
            }
            Self::None => TextTransition::ProcessFresh {
                discard_existing: false,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_transition_should_cancel_from_any_state_when_cancel_button_received() {
        let state = UserState::AwaitingClassification {
            raw_text: "120 80".to_string(),
        };

        assert_eq!(
            state.text_transition(telegram::BTN_CANCEL),
            TextTransition::Cancel
        );
    }

    #[test]
    fn text_transition_should_save_pending_pressure_when_save_button_received() {
        let pending = PendingPressure {
            sys: 121,
            dia: 79,
            pulse: Some(70),
        };
        let state = UserState::AwaitingPressureConfirmation(pending.clone());

        assert_eq!(
            state.text_transition(telegram::BTN_SAVE),
            TextTransition::SavePressure(pending)
        );
    }

    #[test]
    fn text_transition_should_force_cost_with_original_raw_text() {
        let state = UserState::AwaitingClassification {
            raw_text: "250 taxi".to_string(),
        };

        assert_eq!(
            state.text_transition(telegram::BTN_COST),
            TextTransition::ForceCost {
                raw_text: "250 taxi".to_string()
            }
        );
    }

    #[test]
    fn parse_or_none_should_return_none_for_invalid_serialized_state() {
        assert_eq!(UserState::parse_or_none("{bad json"), UserState::None);
    }
}
