//! Application entry points for backend-owned variable object retirement.

use crate::debugger::MiClient;

pub(super) fn register_owned_variable_object(client: &MiClient, owner: &str, child: &str) {
    client.register_owned_variable_object(owner, child);
}

pub(in crate::app) fn delete_variable_object(client: &MiClient, varobj: &str) {
    client.delete_variable_object(varobj.to_owned());
}

#[cfg(test)]
mod tests {
    use super::*;

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
