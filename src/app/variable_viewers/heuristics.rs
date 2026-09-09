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

pub(super) fn linked_value_is_end(variable: &Variable) -> bool {
    variable.is_null_pointer()
        || (link_wrapper(variable) == Some(LinkWrapper::Optional)
            && variable.value.trim().rsplit("::").next() == Some("None"))
}

pub(super) fn indexed_child_ordinal(name: &str) -> Option<i64> {
    let name = name.trim();
    let index = name
        .strip_prefix('[')
        .and_then(|name| name.strip_suffix(']'))
        .unwrap_or(name);

    index.parse().ok()
}

pub(super) fn linked_children_are_end(current: &Variable, children: &[Variable]) -> bool {
    // Raw Rust MI represents only the active variant as a named child.
    // Absence of a readable Some is not, by itself, proof of an empty Option.
    link_wrapper(current) == Some(LinkWrapper::Optional)
        && children.iter().any(|child| child.name == "None")
        && !children.iter().any(|child| child.name == "Some")
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
    link_wrapper_members(current)?.iter().find_map(|preferred| {
        children
            .iter()
            .find(|child| normalize_member_name(&child.name) == *preferred && child.can_expand())
            .cloned()
    })
}

pub(super) fn link_wrapper_members(current: &Variable) -> Option<&'static [&'static str]> {
    Some(match link_wrapper(current)? {
        LinkWrapper::Optional => &["some", "__0", "0"],
        LinkWrapper::Owner => &["ptr", "pointer", "__0", "0", "value"],
        LinkWrapper::Pointer => &["pointer", "ptr", "__0", "0"],
        LinkWrapper::Cell => &["value"],
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LinkWrapper {
    Optional,
    Owner,
    Pointer,
    Cell,
}

fn link_wrapper(current: &Variable) -> Option<LinkWrapper> {
    let type_name = current
        .type_name
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();

    let outer = type_name.split('<').next()?.rsplit("::").next()?;
    let outer = outer
        .trim()
        .trim_start_matches("&mut ")
        .trim_start_matches('&')
        .trim_start_matches("*mut ")
        .trim_start_matches("*const ");

    match outer {
        "option" => Some(LinkWrapper::Optional),
        "rc" | "arc" | "weak" | "box" => Some(LinkWrapper::Owner),
        "nonnull" => Some(LinkWrapper::Pointer),
        "rcinner" | "arcinner" | "refcell" | "unsafecell" => Some(LinkWrapper::Cell),
        _ => None,
    }
}

pub(super) fn is_cpp_access_group(name: &str) -> bool {
    matches!(
        normalize_member_name(name).as_str(),
        "public" | "private" | "protected"
    )
}
