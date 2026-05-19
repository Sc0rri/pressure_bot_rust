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
}
