use super::*;

pub(super) fn normalize_member_name(name: &str) -> String {
    name.trim()
        .trim_matches(['[', ']'])
        .rsplit("::")
        .next()
        .unwrap_or(name)
        .to_ascii_lowercase()
}

pub(super) fn compact_variable_type_name(type_name: Option<&str>) -> String {
    type_name
        .map(super::compact_variable_type)
        .filter(|type_name| !type_name.is_empty())
        .unwrap_or_else(|| String::from("<unknown>"))
}

pub(super) fn compact_viewer_text(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let compact = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{compact}...")
    } else {
        compact
    }
}

pub(super) fn viewer_value_is_null(value: &str) -> bool {
    if pointer_address(value) == Some(0) {
        return true;
    }
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "0" | "null" | "nullptr" | "none" | "nil" | "<null>"
    )
}

pub(super) fn indexed_child_ordinal(name: &str) -> Option<usize> {
    name.trim()
        .strip_prefix('[')?
        .strip_suffix(']')?
        .parse()
        .ok()
}

pub(super) fn transparent_index_wrapper(children: &[Variable]) -> Option<Variable> {
    children
        .iter()
        .filter_map(|child| {
            let name = normalize_member_name(&child.name);
            if !child.can_expand() {
                return None;
            }
            let priority = match name.as_str() {
                "_m_elems" | "__elems" | "__elems_" | "_elems" | "elems" | "elements" => 0,
                "public" | "private" | "protected" => 1,
                _ => return None,
            };
            Some((priority, child))
        })
        .min_by_key(|(priority, _)| *priority)
        .map(|(_, child)| child.clone())
}

pub(super) fn transparent_link_wrapper(
    current: &Variable,
    children: &[Variable],
) -> Option<Variable> {
    let type_name = current
        .type_name
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let preferred: &[&str] = if type_name.contains("option<") {
        &["some", "__0", "0"]
    } else if type_name.contains("rc<")
        || type_name.contains("arc<")
        || type_name.contains("weak<")
        || type_name.contains("box<")
    {
        &["ptr", "pointer", "__0", "0"]
    } else if type_name.contains("nonnull<") {
        &["pointer", "ptr", "__0", "0"]
    } else if ["rcinner<", "arcinner<", "refcell<", "unsafecell<"]
        .iter()
        .any(|wrapper| type_name.contains(wrapper))
    {
        &["value"]
    } else {
        return None;
    };
    preferred.iter().find_map(|preferred| {
        children
            .iter()
            .find(|child| normalize_member_name(&child.name) == *preferred && child.can_expand())
            .cloned()
    })
}

pub(super) fn is_cpp_access_group(name: &str) -> bool {
    matches!(
        normalize_member_name(name).as_str(),
        "public" | "private" | "protected"
    )
}
