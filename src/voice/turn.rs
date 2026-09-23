/**
* this file code contains conversational turn assembly and settling
*/
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct DraftTurn {
    pub id: String,
    pub revision: u64,
    pub text: String,
    partial: String,
}

impl Default for DraftTurn {
    fn default() -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            revision: 0,
            text: String::new(),
            partial: String::new(),
        }
    }
}

impl DraftTurn {
    pub fn partial(&mut self, text: &str) -> bool {
        let text = text.trim();
        if text.is_empty() || text == self.partial {
            return false;
        }
        self.partial = text.to_string();
        self.revision += 1;
        true
    }

    pub fn finish(&mut self, text: &str) {
        let text = text.trim();
        self.partial.clear();
        if text.is_empty() {
            return;
        }
        let lower = text.to_ascii_lowercase();
        if [
            "actually",
            "instead",
            "no,",
            "no ",
            "sorry,",
            "i meant",
            "never mind",
            "cancel that",
        ]
        .iter()
        .any(|prefix| lower.starts_with(prefix))
        {
            self.text.clear();
        }
        if !self.text.is_empty() {
            self.text.push(' ');
        }
        self.text.push_str(text);
        self.revision += 1;
    }

    pub fn has_partial(&self) -> bool {
        !self.partial.is_empty()
    }

    pub fn snapshot(&self) -> String {
        if self.partial.is_empty() {
            self.text.clone()
        } else if self.text.is_empty() {
            self.partial.clone()
        } else {
            format!("{} {}", self.text, self.partial)
        }
    }

    pub fn settle_delay(&self) -> Duration {
        let lower = self.text.to_ascii_lowercase();
        let numeric = lower.split_whitespace().all(|word| {
            let word = word.trim_matches(|c: char| !c.is_alphanumeric());
            word.chars().all(|c| c.is_ascii_digit())
                || matches!(
                    word,
                    "zero"
                        | "oh"
                        | "one"
                        | "two"
                        | "three"
                        | "four"
                        | "five"
                        | "six"
                        | "seven"
                        | "eight"
                        | "nine"
                        | "ten"
                        | "eleven"
                        | "twelve"
                        | "double"
                        | "triple"
                        | "plus"
                )
        });
        Duration::from_millis(if numeric { 1200 } else { 350 })
    }

    pub fn settle_delay_for_completeness(&self, completeness: Option<f64>) -> Duration {
        if let Some(score) = completeness {
            if score >= 0.85 {
                return Duration::from_millis(160);
            }
            if score < 0.35 {
                return Duration::from_millis(1100);
            }
        }
        self.settle_delay()
    }
}

pub fn is_backchannel(text: &str) -> bool {
    let lower = text.trim().to_ascii_lowercase();
    let cleaned = lower.trim_matches(|c: char| !c.is_alphanumeric());
    matches!(
        cleaned,
        "uh huh"
            | "uh-huh"
            | "uhhuh"
            | "yeah"
            | "yep"
            | "yes"
            | "ok"
            | "okay"
            | "right"
            | "got it"
            | "mhm"
            | "mm hmm"
            | "mm-hmm"
            | "sure"
            | "cool"
            | "fine"
    )
}
