//! Independent variable objects belong to one MI backend incarnation.

use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Default)]
pub(super) struct OwnedObjects {
    by_owner: BTreeMap<String, String>,
    owner_of: HashMap<String, String>,
}

impl OwnedObjects {
    pub(super) fn owned_root<'a, 'b>(
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

    pub(super) fn register(&mut self, owner: &str, child: &str) -> Option<String> {
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

    pub(super) fn take(&mut self, root: &str) -> Vec<String> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ownership_and_deferred_cleanup_retire_with_the_backend() {
        use crate::debugger::mi::{MiClient, tests::MI_CLIENT_TEST_LOCK};
        use std::{cell::Cell, rc::Rc};

        let _guard = MI_CLIENT_TEST_LOCK.lock().unwrap();
        let context = gtk::glib::MainContext::default();
        let _context = context.acquire().unwrap();
        let (client, _peer) = MiClient::open_with_injected_transport(|_, _| {}).unwrap();
        let (other, _other_peer) = MiClient::open_with_injected_transport(|_, _| {}).unwrap();
        let allowed = Rc::new(Cell::new(false));
        let guard = Rc::clone(&allowed);
        client.set_variable_cleanup_guard(move || guard.get());
        client.register_owned_variable_object("root.pointer", "independent");
        let roots = HashSet::from([String::from("root")]);

        assert_eq!(
            client.variable_object_owned_root(&roots, "independent.field"),
            roots.get("root"),
        );
        assert!(
            other
                .variable_object_owned_root(&roots, "independent.field")
                .is_none()
        );
        client.delete_variable_object(String::from("root"));
        assert!(client.pending.borrow().is_empty());
        assert_eq!(client.deferred_variable_deletions.borrow().len(), 2);
        assert!(
            client
                .variable_object_owned_root(&roots, "independent.field")
                .is_none()
        );
        client.register_owned_variable_object("root.pointer", "new_independent");
        client.reconnect().unwrap();
        assert!(client.deferred_variable_deletions.borrow().is_empty());
        assert!(
            client
                .variable_object_owned_root(&roots, "new_independent.field")
                .is_none()
        );
        client.delete_variable_object(String::from("fresh"));
        assert_eq!(client.deferred_variable_deletions.borrow().len(), 1);
        allowed.set(true);
        client.flush_variable_deletions();
        assert!(client.deferred_variable_deletions.borrow().is_empty());
        assert_eq!(client.pending.borrow().len(), 1);
    }

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
}
