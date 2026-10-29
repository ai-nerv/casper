use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadArgs {
    pub path: String,
    pub offset: Option<usize>,
    pub limit: Option<usize>,
}

impl ReadArgs {
    pub fn parse(value: &serde_json::Value) -> Result<Self, String> {
        let args: Self = serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
        if args.path.is_empty() || args.path.len() > 4096 || args.path.as_bytes().contains(&0) {
            return Err("native read path exceeds its bounds".into());
        }
        if args.offset == Some(0) || args.limit.is_some_and(|value| value == 0 || value > 5000) {
            return Err("native read offset/limit is outside the declared bounds".into());
        }
        Ok(args)
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ReadData {
    Text(ReadText),
    Image(crate::images::Image),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadText {
    pub source_text: Option<String>,
    pub text: String,
    pub source_bytes: usize,
    pub total_lines: usize,
    pub first_line: usize,
    pub last_line: usize,
    pub omitted_bytes: usize,
}

impl ReadText {
    pub fn validate(&self, args: &ReadArgs) -> Result<(), String> {
        if self.first_line != args.offset.unwrap_or(1)
            || self.last_line
                != self
                    .first_line
                    .saturating_sub(1)
                    .saturating_add(args.limit.unwrap_or(2000))
                    .min(self.total_lines)
            || self.total_lines > self.source_bytes
            || self.omitted_bytes > self.source_bytes
            || self.text.len() > crate::running::MOST * 3
            || self.source_text.as_ref().is_some_and(|text| {
                text.len() != self.source_bytes
                    || text.len() >= crate::source::EDIT_BYTES
                    || text.contains('\0')
            })
        {
            return Err("broker read data has invalid bounds or range".into());
        }
        Ok(())
    }
}
