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
            if let Ok(num) = p.parse::<i32>() {
                if amount.is_none() {
                    amount = Some(num);
                    continue;
                }
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

    /// Attempts to parse a JSON object with sys, dia, pulse keys from text.
    /// Looks for a `{...}` substring and parses it as JSON.
    fn parse_json_response(text: &str) -> Option<(i32, i32, Option<i32>)> {
        // Find the first '{' and last '}'
        let start = text.find('{')?;
        let end = text.rfind('}')?;
        if start >= end {
            return None;
        }
        let json_str = &text[start..=end];

        // Try to parse as JSON
        let value: serde_json::Value = serde_json::from_str(json_str).ok()?;
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
            .split(|c: char| c.is_whitespace() || c == '/' || c == '\\' || c == '|' || c == ':' || c == ',')
            .filter(|s| !s.is_empty())
            .collect();

        let mut nums: Vec<i32> = Vec::new();
        for p in &parts {
            // Clean each token: strip trailing/leading non-digit chars like '.', ',', ')', etc.
            let cleaned: String = p.chars().filter(|c| c.is_ascii_digit() || *c == '-').collect();
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