//! Ownership of independent MI objects created while dereferencing tree nodes.

use super::*;
use std::collections::BTreeMap;

#[derive(Default)]
struct OwnedObjects {
    by_owner: BTreeMap<String, String>,
    owner_of: HashMap<String, String>,
}

impl OwnedObjects {
    fn owned_root<'a, 'b>(
        &'b self,
        roots: &'a HashSet<String>,
        mut candidate: &'b str,
    ) -> Option<&'a String> {
        // A dereference root has an independent MI name. Follow its registered
        // owner as well as ordinary dotted child paths, without allocating.
        for _ in 0..=self.owner_of.len() {
            loop {
                if let Some(root) = roots.get(candidate) {
                    return Some(root);
                }

                if let Some(owner) = self.owner_of.get(candidate) {
                    candidate = owner;
                    break;
                }

                candidate = candidate.rsplit_once('.')?.0;
            }
        }

        None
    }

    fn register(&mut self, owner: &str, child: &str) -> Option<String> {
        if owner == child {
            return None;
        }

        if let Some(previous) = self.owner_of.insert(child.to_owned(), owner.to_owned())
            && previous != owner
        {
            self.by_owner.remove(&previous);
        }

        let retired = self.by_owner.insert(owner.to_owned(), child.to_owned());
        let retired = retired.filter(|previous| previous != child);

        if let Some(retired) = &retired {
            self.owner_of.remove(retired);
        }

        retired
    }

    fn take(&mut self, root: &str) -> Vec<String> {
        let mut objects = vec![root.to_owned()];
        let mut visited = HashSet::from([root.to_owned()]);
        let mut index = 0;

        while index < objects.len() {
            let object = &objects[index];

            if let Some(owner) = self.owner_of.remove(object) {
                self.by_owner.remove(&owner);
            }

            // Deleting an MI root also deletes its ordinary children. Their
            // independent dereference objects are ours to retire explicitly.
            let prefix = format!("{object}.");

            let mut owners = self
                .by_owner
                .range(prefix.clone()..)
                .take_while(|(owner, _)| owner.starts_with(&prefix))
                .map(|(owner, _)| owner.clone())
                .collect::<Vec<_>>();

            owners.push(object.clone());

            for owner in owners {
                if let Some(child) = self.by_owner.remove(&owner) {
                    self.owner_of.remove(&child);

                    if visited.insert(child.clone()) {
                        objects.push(child);
                    }
                }
            }

            index += 1;
        }

        objects
    }
}

thread_local! {
    static OWNED_OBJECTS: RefCell<OwnedObjects> = RefCell::default();
}

pub(super) fn variable_object_owned_root<'a>(
    roots: &'a HashSet<String>,
    candidate: &str,
) -> Option<&'a String> {
    OWNED_OBJECTS.with(|objects| objects.borrow().owned_root(roots, candidate))
}

pub(super) fn register_owned_variable_object(client: &MiClient, owner: &str, child: &str) {
    let retired = OWNED_OBJECTS.with(|objects| objects.borrow_mut().register(owner, child));

    // Never dispatch MI cleanup while holding a registry or refresh-state borrow.
    if let Some(retired) = retired {
        delete_variable_object(client, &retired);
    }
}

pub(in crate::app) fn delete_variable_object(client: &MiClient, varobj: &str) {
    let objects = OWNED_OBJECTS.with(|objects| objects.borrow_mut().take(varobj));

    for object in objects.into_iter().rev() {
        client.delete_variable_object(object);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deletes_independent_objects_below_native_child_paths() {
        let mut objects = OwnedObjects::default();
        objects.register("root.public.pointer", "deref");
        objects.register("deref.next", "nested");
        objects.register("root2.pointer", "unrelated");
        objects.register("root-other.pointer", "also_unrelated");
        let removed = objects.take("root");
        assert_eq!(removed, ["root", "deref", "nested"]);
        assert_eq!(objects.by_owner.len(), 2);
        assert_eq!(objects.owner_of.len(), 2);
    }

    #[test]
    fn repeated_dereferences_retire_the_previous_target_and_its_descendants() {
        let mut objects = OwnedObjects::default();
        assert_eq!(objects.register("root.pointer", "old"), None);
        objects.register("old.next", "nested");
        assert_eq!(
            objects.register("root.pointer", "new").as_deref(),
            Some("old")
        );

        assert_eq!(objects.take("old"), ["old", "nested"]);
        assert_eq!(objects.take("root.pointer"), ["root.pointer", "new"]);
        assert!(objects.by_owner.is_empty());
        assert!(objects.owner_of.is_empty());
    }

    #[test]
    fn cleanup_is_idempotent_and_cycle_safe() {
        let mut objects = OwnedObjects::default();
        objects.register("a", "b");
        objects.register("b", "a");
        assert_eq!(objects.take("a"), ["a", "b"]);
        assert_eq!(objects.take("a"), ["a"]);
        assert!(objects.by_owner.is_empty());
        assert!(objects.owner_of.is_empty());
    }

    #[test]
    fn routes_independent_dereference_updates_to_their_published_root() {
        let mut objects = OwnedObjects::default();
        objects.register("root.public.pointer", "deref");
        objects.register("deref.next", "nested");
        let roots = HashSet::from([String::from("root")]);

        for candidate in [
            "root",
            "root.public",
            "deref",
            "deref.value",
            "nested.value",
        ] {
            assert_eq!(
                objects.owned_root(&roots, candidate).map(String::as_str),
                Some("root"),
                "{candidate}",
            );
        }

        assert!(objects.owned_root(&roots, "root2.public").is_none());
        assert!(objects.owned_root(&roots, "unrelated").is_none());
        objects.register("a", "b");
        objects.register("b", "a");
        assert!(objects.owned_root(&roots, "a.value").is_none());
    }

    #[test]
    #[ignore = "requires GDB and the built C linked-list fixture"]
    fn live_repeated_dereferences_are_retired_with_their_native_owner() {
        use crate::app::test_support::{open_debugger, request, wait_until};

        let (_debugger, client) =
            open_debugger("c-linked-list-viewer-target", "linked_list_checkpoint");

        for (name, expression) in [
            ("owner", "*custom_head"),
            ("old_deref", "*custom_head->tailward"),
            ("new_deref", "*custom_head->tailward"),
            ("nested", "*custom_head->tailward->tailward"),
            ("unrelated", "*custom_head"),
        ] {
            let record = request(&client, &format!("-var-create {name} * {expression}"));
            assert!(record.is_done(), "{record:?}");
        }

        register_owned_variable_object(&client, "owner.tailward", "old_deref");
        register_owned_variable_object(&client, "old_deref.tailward", "nested");
        register_owned_variable_object(&client, "owner.tailward", "new_deref");

        for name in ["old_deref", "nested"] {
            // Cleanup is deliberately background work; foreground inspection
            // can overtake a queued deletion without making that object leak.
            wait_until(|| !request(&client, &format!("-var-info-type {name}")).is_done());
        }

        assert!(request(&client, "-var-info-type new_deref").is_done());
        delete_variable_object(&client, "owner");
        wait_until(|| !request(&client, "-var-info-type owner").is_done());
        wait_until(|| !request(&client, "-var-info-type new_deref").is_done());
        assert!(request(&client, "-var-info-type unrelated").is_done());
    }
}
