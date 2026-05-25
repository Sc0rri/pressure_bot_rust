#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Pressure {
        sys: i32,
        dia: i32,
        pulse: Option<i32>,
    },
    Cost {
        amount: i32,
        comment: String,
    },
}

impl Action {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Pressure { .. } => "pressure",
            Self::Cost { .. } => "cost",
        }
    }
}

pub struct ParserService;

impl ParserService {
    /// Parser for pressure in strict auto-detection mode
    pub fn parse_as_pressure(text: &str) -> Option<Action> {
        let clean = text.trim();
        let parts: Vec<&str> = clean
            .split(|c: char| c.is_whitespace() || c == '\\' || c == '/' || c == '|')
            .filter(|s| !s.is_empty())
            .collect();

        let mut nums = Vec::new();
        let mut words = Vec::new();
        for p in parts {
            if let Ok(num) = p.parse::<i32>() {
                nums.push(num);
            } else {
                words.push(p);
            }
        }

        if words.is_empty() && (nums.len() == 2 || nums.len() == 3) {
            let sys = nums[0];
            let dia = nums[1];
            if (80..=250).contains(&sys) && (40..=150).contains(&dia) {
                let mut pulse = None;
                if nums.len() == 3 {
                    let p = nums[2];
                    if (40..=200).contains(&p) {
                        pulse = Some(p);
                    } else {
                        return None;
                    }
                }
                return Some(Action::Pressure { sys, dia, pulse });
            }
        }
        None
    }

    /// Parser for manual pressure option (from KV store payload)
    pub fn parse_manual_pressure(text: &str) -> Option<Action> {
        let clean = text.trim();
        let parts: Vec<&str> = clean
            .split(|c: char| c.is_whitespace() || c == '\\' || c == '/' || c == '|')
            .filter(|s| !s.is_empty())
            .collect();

        let mut nums = Vec::new();
        for p in parts {
            if let Ok(num) = p.parse::<i32>() {
                nums.push(num);
            }
        }

        if nums.len() >= 2 {
            let sys = nums[0];
            let dia = nums[1];
            let pulse = nums.get(2).copied();
            Some(Action::Pressure { sys, dia, pulse })
        } else {
            None
        }
    }

    /// Parser for manual cost option (from KV store payload)
    pub fn parse_manual_cost(text: &str) -> Option<Action> {
        let clean = text.trim();
        let parts: Vec<&str> = clean
            .split(|c: char| c.is_whitespace() || c == '\\' || c == '/' || c == '|')
            .filter(|s| !s.is_empty())
            .collect();

        let mut amount = None;
        let mut comment_parts = Vec::new();

        for p in parts {
            if let Ok(num) = p.parse::<i32>()
                && amount.is_none()
            {
                amount = Some(num);
                continue;
            }
            comment_parts.push(p);
        }

        amount.map(|amt| Action::Cost {
            amount: amt,
            comment: comment_parts.join(" "),
        })
    }

    /// Default classification flow
    pub fn detect_action(text: &str) -> Option<Action> {
        if let Some(pressure) = Self::parse_as_pressure(text) {
            return Some(pressure);
        }

        let clean = text.trim();
        let parts: Vec<&str> = clean
            .split(|c: char| c.is_whitespace() || c == '\\' || c == '/' || c == '|')
            .filter(|s| !s.is_empty())
            .collect();

        let mut nums = Vec::new();
        let mut words = Vec::new();
        for p in parts {
            if let Ok(num) = p.parse::<i32>() {
                nums.push(num);
            } else {
                words.push(p);
            }
        }

        if nums.len() == 1 {
            return Some(Action::Cost {
                amount: nums[0],
                comment: words.join(" "),
            });
        }

        None
    }

    fn pressure_from_json_value(value: &serde_json::Value) -> Option<(i32, i32, Option<i32>)> {
        let obj = value.as_object()?;

        let sys = obj.get("sys")?.as_i64()? as i32;
        let dia = obj.get("dia")?.as_i64()? as i32;
        let pulse = obj.get("pulse").and_then(|v| v.as_i64()).map(|v| v as i32);

        // Validate ranges
        if (80..=250).contains(&sys) && (40..=150).contains(&dia) {
            let valid_pulse = pulse.filter(|&p| (40..=200).contains(&p));
            Some((sys, dia, valid_pulse))
        } else {
            None
        }
    }

    /// Attempts to parse any valid JSON object with sys, dia, pulse keys from text.
    /// AI vision models often wrap the object in prose or include an example before
    /// the final answer, so each balanced `{...}` block is tried independently.
    fn parse_json_response(text: &str) -> Option<(i32, i32, Option<i32>)> {
        let mut start = None;
        let mut depth = 0usize;
        let mut last_valid = None;

        for (idx, ch) in text.char_indices() {
            match ch {
                '{' => {
                    if depth == 0 {
                        start = Some(idx);
                    }
                    depth += 1;
                }
                '}' if depth > 0 => {
                    depth -= 1;
                    if depth == 0 {
                        if let Some(start_idx) = start {
                            let json_str = &text[start_idx..=idx];
                            if let Ok(value) = serde_json::from_str::<serde_json::Value>(json_str)
                                && let Some(result) = Self::pressure_from_json_value(&value)
                            {
                                last_valid = Some(result);
                            }
                        }
                        start = None;
                    }
                }
                _ => {}
            }
        }

        last_valid
    }

    /// Parses AI response text that might contain blood pressure values
    /// Accepts formats like:
    /// - JSON: {"sys": 120, "dia": 80, "pulse": 72}
    /// - "120/80", "120 80", "158 113 79.", "sys:120 dia:80 pulse:72", etc.
    pub fn parse_ai_pressure_response(text: &str) -> Option<(i32, i32, Option<i32>)> {
        let clean = text.trim().to_lowercase();

        // Try JSON first (most reliable)
        if let Some(result) = Self::parse_json_response(&clean) {
            return Some(result);
        }

        // Fallback: try to find numbers in the response
        let parts: Vec<&str> = clean
            .split(|c: char| {
                c.is_whitespace() || c == '/' || c == '\\' || c == '|' || c == ':' || c == ','
            })
            .filter(|s| !s.is_empty())
            .collect();

        let mut nums: Vec<i32> = Vec::new();
        for p in &parts {
            // Clean each token: strip trailing/leading non-digit chars like '.', ',', ')', etc.
            let cleaned: String = p
                .chars()
                .filter(|c| c.is_ascii_digit() || *c == '-')
                .collect();
            if cleaned.is_empty() {
                continue;
            }
            if let Ok(n) = cleaned.parse::<i32>() {
                nums.push(n);
            }
        }

        if nums.len() >= 2 {
            let sys = nums[0];
            let dia = nums[1];
            // Validate ranges
            if (80..=250).contains(&sys) && (40..=150).contains(&dia) {
                let pulse = nums.get(2).copied().filter(|&p| (40..=200).contains(&p));
                return Some((sys, dia, pulse));
            }
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_action_should_parse_two_numbers_as_pressure() {
        assert_eq!(
            ParserService::detect_action("120/80"),
            Some(Action::Pressure {
                sys: 120,
                dia: 80,
                pulse: None
            })
        );
    }

    #[test]
    fn detect_action_should_parse_single_number_with_words_as_cost() {
        assert_eq!(
            ParserService::detect_action("250 taxi"),
            Some(Action::Cost {
                amount: 250,
                comment: "taxi".to_string()
            })
        );
    }

    #[test]
    fn parse_as_pressure_should_reject_out_of_range_pulse() {
        assert_eq!(ParserService::parse_as_pressure("120 80 250"), None);
    }

    #[test]
    fn parse_manual_cost_should_keep_second_number_in_comment() {
        assert_eq!(
            ParserService::parse_manual_cost("250 taxi 2"),
            Some(Action::Cost {
                amount: 250,
                comment: "taxi 2".to_string()
            })
        );
    }

    #[test]
    fn parse_ai_pressure_response_should_parse_json_inside_prose() {
        assert_eq!(
            ParserService::parse_ai_pressure_response(
                "reading looks like {\"sys\": 121, \"dia\": 79, \"pulse\": 68}"
            ),
            Some((121, 79, Some(68)))
        );
    }

    #[test]
    fn parse_ai_pressure_response_should_ignore_invalid_json_range() {
        assert_eq!(
            ParserService::parse_ai_pressure_response("{\"sys\": 260, \"dia\": 79, \"pulse\": 68}"),
            None
        );
    }
}
