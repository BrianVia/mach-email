use crate::LabelId;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Split {
    #[default]
    Important,
    Other,
    Newsletters,
}

pub fn split_of(label_ids: &[LabelId]) -> Split {
    let has = |label| label_ids.iter().any(|id| id.as_str() == label);
    if ["CATEGORY_PROMOTIONS", "CATEGORY_UPDATES", "CATEGORY_FORUMS"]
        .into_iter()
        .any(has)
    {
        Split::Newsletters
    } else if has("IMPORTANT")
        || has("CATEGORY_PERSONAL")
        || !label_ids
            .iter()
            .any(|id| id.as_str().starts_with("CATEGORY_"))
    {
        Split::Important
    } else {
        Split::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(values: &[&str]) -> Vec<LabelId> {
        values.iter().copied().map(LabelId::new).collect()
    }

    #[test]
    fn classifies_inbox_splits() {
        assert_eq!(split_of(&labels(&["INBOX", "IMPORTANT"])), Split::Important);
        assert_eq!(
            split_of(&labels(&["IMPORTANT", "CATEGORY_PROMOTIONS"])),
            Split::Newsletters
        );
        assert_eq!(split_of(&labels(&["CATEGORY_SOCIAL"])), Split::Other);
    }
}
