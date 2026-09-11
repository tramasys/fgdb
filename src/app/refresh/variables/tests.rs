use std::collections::HashSet;

use super::{
    Variable, VariableRefreshTarget, has_persistent_variable_objects, reuse_variable_objects,
    variable_child_page_end, variable_object_has_owned_ancestor, variable_object_owned_root,
    variable_object_owns_update,
};

fn variable(name: &str, value: &str, type_name: Option<&str>, varobj: Option<&str>) -> Variable {
    Variable {
        local_index: None,
        return_value: None,
        name: name.to_owned(),
        value: value.to_owned(),
        type_name: type_name.map(str::to_owned),
        argument: false,
        varobj: varobj.map(str::to_owned),
        num_children: usize::from(varobj.is_some()),
        has_more: false,
        display_hint: None,
        dynamic: false,
    }
}

#[test]
fn reuses_live_roots_and_discards_only_stale_variable_objects() {
    let fallbacks = vec![
        variable("pointer", "0x20", Some("Node *"), None),
        variable("count", "7", Some("int"), None),
    ];

    let existing = vec![
        variable("pointer", "0x10", Some("Node *"), Some("var1")),
        variable("removed", "0x30", Some("Node *"), Some("var2")),
        variable("count", "0x40", Some("Node *"), Some("var3")),
    ];

    let (reused, needs_update, mut stale) = reuse_variable_objects(&fallbacks, existing);
    assert_eq!(reused[0].varobj.as_deref(), Some("var1"));
    assert_eq!(reused[0].value, "0x10");
    assert_eq!(reused[1], fallbacks[1]);
    assert_eq!(needs_update, [true, false]);
    stale.sort_unstable();
    assert_eq!(stale, [String::from("var2"), String::from("var3")]);
}

#[test]
fn duplicate_local_names_reuse_their_own_occurrence() {
    let mut first = variable("value", "{...}", Some("Value"), Some("var1"));
    first.local_index = Some(0);
    let mut second = first.clone();
    second.local_index = Some(1);
    second.varobj = Some("var2".into());
    let mut fallbacks = vec![first.clone(), second.clone()];

    for variable in &mut fallbacks {
        variable.varobj = None;
        variable.value = "<not available>".into();
    }

    let (reused, _, stale) = reuse_variable_objects(&fallbacks, vec![second, first]);
    assert!(stale.is_empty());
    assert_eq!(reused[0].varobj.as_deref(), Some("var1"));
    assert_eq!(reused[1].varobj.as_deref(), Some("var2"));

    let first = variable("pointer", "0x10", Some("Node *"), Some("first"));
    let mut second = first.clone();
    second.varobj = Some(String::from("second"));
    let mut argument = first.clone();
    argument.argument = true;
    argument.varobj = Some(String::from("argument"));
    let fallback = variable("pointer", "0x20", Some("Node *"), None);
    let (reused, needs_update, stale) =
        reuse_variable_objects(&[fallback.clone(), fallback], vec![first, second, argument]);

    assert_eq!(reused[0].varobj.as_deref(), Some("second"));
    assert_eq!(reused[1].varobj.as_deref(), Some("first"));
    assert_eq!(needs_update, [true, true]);
    assert_eq!(stale, [String::from("argument")]);
}

#[test]
fn creates_local_pointer_objects_only_after_they_are_requested() {
    let pointer = variable("pointer", "0x20", Some("Node *"), None);
    let aggregate = variable("fixture", "<not available>", Some("struct Fixture"), None);
    assert!(!VariableRefreshTarget::Locals.creates_missing_variable_object(&pointer));
    assert!(VariableRefreshTarget::Locals.creates_missing_variable_object(&aggregate));

    assert!(
        VariableRefreshTarget::ExpressionWatches(Vec::new())
            .creates_missing_variable_object(&pointer)
    );
}

#[test]
fn changed_types_retire_old_objects_instead_of_relabeling_their_layout() {
    let fallback = variable("value", "{...}", Some("New"), None);
    let previous = variable("value", "{...}", Some("Old"), Some("previous"));
    let (reused, updates, stale) =
        reuse_variable_objects(std::slice::from_ref(&fallback), vec![previous]);

    assert_eq!(reused, [fallback]);
    assert_eq!(updates, [false]);
    assert_eq!(stale, ["previous"]);
}

#[test]
fn explicit_watches_are_not_silently_limited_by_the_locals_creation_budget() {
    let expressions = (0..40).map(|index| format!("items[{index}]")).collect();
    assert_eq!(VariableRefreshTarget::Locals.creation_budget(), 32);
    assert_eq!(
        VariableRefreshTarget::ExpressionWatches(expressions).creation_budget(),
        40
    );
}

#[test]
fn bounds_dynamic_variable_pages_while_allowing_later_pages() {
    assert_eq!(variable_child_page_end(0), Some(128));
    assert_eq!(variable_child_page_end(128), Some(256));
    assert_eq!(variable_child_page_end(4_000), Some(4_096));
    assert_eq!(variable_child_page_end(4_096), None);
    assert_eq!(variable_child_page_end(usize::MAX), None);
}

#[test]
fn refreshes_existing_lazy_local_objects_without_creating_new_ones() {
    let pointer = variable("pointer", "0x20", Some("Node *"), None);
    let target = VariableRefreshTarget::Locals;
    assert!(!target.requires_refresh(std::slice::from_ref(&pointer), &[false]));
    assert!(target.requires_refresh(std::slice::from_ref(&pointer), &[true]));
}

#[test]
fn bulk_updates_route_only_to_owned_roots_and_descendants() {
    assert!(variable_object_owns_update("fgdb_var_1", "fgdb_var_1"));

    assert!(variable_object_owns_update(
        "fgdb_var_1",
        "fgdb_var_1.public.next"
    ));

    assert!(!variable_object_owns_update(
        "fgdb_var_1",
        "fgdb_var_10.public"
    ));

    assert!(!variable_object_owns_update("fgdb_var_1", "temporary"));
    let roots = HashSet::from([String::from("fgdb_var_1"), String::from("fgdb_var_20")]);

    assert_eq!(
        variable_object_owned_root(&roots, "fgdb_var_1.choice.value").map(String::as_str),
        Some("fgdb_var_1"),
    );

    assert_eq!(
        variable_object_owned_root(&roots, "fgdb_var_20").map(String::as_str),
        Some("fgdb_var_20"),
    );

    assert!(variable_object_has_owned_ancestor(
        &roots,
        "fgdb_var_1.public.next.value"
    ));

    assert!(!variable_object_has_owned_ancestor(
        &roots,
        "fgdb_var_10.public"
    ));
}

#[test]
fn newly_created_persistent_roots_participate_in_the_bulk_update() {
    let created = variable("items", "{...}", Some("Vec<int>"), Some("fgdb_var_1"));
    let scalar = variable("count", "4", Some("int"), None);
    assert!(has_persistent_variable_objects(&[created]));
    assert!(!has_persistent_variable_objects(&[scalar]));
}

#[test]
fn bulk_root_updates_preserve_the_existing_update_semantics() {
    let mut root = variable("value", "old", Some("Old"), Some("fgdb_var_1"));

    let update = crate::debugger::VariableUpdate {
        varobj: String::from("fgdb_var_1"),
        value: Some(String::from("new")),
        in_scope: Some(true),
        type_changed: false,
        new_type: Some(String::from("New")),
        new_num_children: Some(4),
        has_more: Some(true),
        display_hint: Some(String::from("array")),
        dynamic: Some(true),
    };

    root.apply_update(&update);
    assert_eq!(root.value, "new");
    assert_eq!(root.type_name.as_deref(), Some("New"));
    assert_eq!(root.num_children, 4);
    assert!(root.has_more);
    assert_eq!(root.display_hint.as_deref(), Some("array"));
    assert!(root.dynamic);
}
