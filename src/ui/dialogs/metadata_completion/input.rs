use super::*;
use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Field {
    Group,
    Tags,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Input {
    pub(super) text: String,
    pub(super) cursor: i32,
    pub(super) selection: Option<(i32, i32)>,
}

pub(super) struct Choice {
    pub(super) value: String,
    folded: String,
}

impl Choice {
    pub(super) fn new(value: String) -> Self {
        let folded = value.to_ascii_lowercase();
        Self { value, folded }
    }
}

pub(super) struct Query {
    range: Range<usize>,
    prefix: String,
    used: HashSet<String>,
    append_separator: bool,
}

fn byte_offset(text: &str, position: i32) -> Option<usize> {
    let position = usize::try_from(position).ok()?;

    Some(
        text.char_indices()
            .nth(position)
            .map_or(text.len(), |(index, _)| index),
    )
}

impl Query {
    pub(super) fn new(input: &Input, field: Field) -> Option<Self> {
        let cursor = byte_offset(&input.text, input.cursor)?;

        let token = match field {
            Field::Group => 0..input.text.len(),
            Field::Tags => {
                let start = input.text[..cursor].rfind(',').map_or(0, |index| index + 1);

                let end = input.text[cursor..]
                    .find(',')
                    .map_or(input.text.len(), |index| cursor + index);

                start..end
            }
        };

        if let Some((start, end)) = input.selection {
            let selection_start = byte_offset(&input.text, start.min(end))?;
            let selection_end = byte_offset(&input.text, start.max(end))?;

            if selection_start < token.start || selection_end > token.end {
                return None;
            }
        }

        let prefix = input.text[token.start..cursor].trim_start();

        if prefix.is_empty() {
            return None;
        }

        let text = &input.text[token.clone()];
        let start = token.start + text.len() - text.trim_start().len();
        let end = token.start + text.trim_end().len();

        let used = if field == Field::Tags {
            input.text[..token.start]
                .split(',')
                .chain(input.text[token.end..].split(','))
                .map(str::trim)
                .filter(|tag| !tag.is_empty())
                .map(str::to_ascii_lowercase)
                .collect()
        } else {
            HashSet::new()
        };

        Some(Self {
            range: if field == Field::Group {
                0..input.text.len()
            } else {
                start..end
            },
            prefix: prefix.to_ascii_lowercase(),
            used,
            append_separator: field == Field::Tags && token.end == input.text.len(),
        })
    }

    pub(super) fn matches(&self, input: &Input, choice: &Choice) -> bool {
        choice.folded.starts_with(&self.prefix)
            && !self.used.contains(&choice.folded)
            && input.text[self.range.clone()] != choice.value
    }

    pub(super) fn complete(&self, input: &Input, choice: &str) -> (String, i32) {
        let mut text = String::with_capacity(input.text.len() + choice.len() + 2);
        text.push_str(&input.text[..self.range.start]);
        text.push_str(choice);

        if self.append_separator {
            text.push_str(", ");
        }

        let cursor = i32::try_from(text.chars().count()).unwrap_or(i32::MAX);

        if !self.append_separator {
            text.push_str(&input.text[self.range.end..]);
        }

        (text, cursor)
    }
}
