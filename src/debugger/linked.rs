//! Bounded, read-only linked-list navigation contracts.

pub(crate) const PAGE_LIMIT: usize = 512;
pub(crate) const NODE_LIMIT: usize = 4096;
pub(crate) const FIELD_BUDGET: usize = 32_768;
pub(crate) const REQUEST_BUDGET: usize = 8192;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LinkedListQuery {
    pub member: String,
    pub page_size: usize,
}

impl LinkedListQuery {
    pub(crate) fn validate(&self, page_limit: usize) -> Result<(), &'static str> {
        if self.page_size == 0 || self.page_size > page_limit.min(PAGE_LIMIT) {
            return Err("Page size is outside the supported range");
        }

        let mut characters = self.member.chars();

        if self.member.len() > 128
            || characters
                .next()
                .is_some_and(|first| first != '_' && !first.is_alphabetic())
            || characters.any(|character| character != '_' && !character.is_alphanumeric())
        {
            return Err(
                "Use a direct field name such as next or prev, or leave it empty for automatic detection",
            );
        }

        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LinkedListAction {
    Restart(LinkedListQuery),
    First,
    Previous,
    Next,
    Cancel,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct LinkedListProgress {
    pub offset: usize,
    pub shown: usize,
    pub cached: usize,
    pub busy: bool,
    pub can_continue: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_queries_accept_only_bounded_member_names_and_pages() {
        for member in ["", "next", "prev", "_M_next", "tailward"] {
            assert!(
                LinkedListQuery {
                    member: member.into(),
                    page_size: 64
                }
                .validate(128)
                .is_ok()
            );
        }

        for member in ["a.b", "next()", "*next", "a\nrun", "1next"] {
            assert!(
                LinkedListQuery {
                    member: member.into(),
                    page_size: 64
                }
                .validate(128)
                .is_err()
            );
        }

        for page_size in [0, 129, usize::MAX] {
            assert!(
                LinkedListQuery {
                    member: String::new(),
                    page_size
                }
                .validate(128)
                .is_err()
            );
        }
    }
}
