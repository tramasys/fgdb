use gtk::glib;
use vte4::{PtyFlags, Terminal, prelude::*};

use crate::config::LaunchConfig;

use super::MiClient;

#[derive(Clone, Debug)]
pub enum SessionEvent {
    Spawned,
    Failed(String),
    Exited(i32),
}

pub fn launch_gdb(
    terminal: &Terminal,
    configuration: &LaunchConfig,
    mi_client: &MiClient,
    on_event: impl Fn(SessionEvent) + Clone + 'static,
) {
    let arguments = configuration.gdb_arguments();
    let argument_refs: Vec<&str> = arguments.iter().map(String::as_str).collect();
    let working_directory = configuration.working_directory.to_string_lossy();
    let mi_path = mi_client.slave_path().to_string_lossy().into_owned();
    let terminal_for_callback = terminal.clone();
    let spawn_event = on_event.clone();

    terminal.connect_child_exited(move |_, status| {
        on_event(SessionEvent::Exited(status));
    });

    terminal.spawn_async(
        PtyFlags::DEFAULT,
        Some(working_directory.as_ref()),
        &argument_refs,
        &[],
        glib::SpawnFlags::SEARCH_PATH,
        || {},
        -1,
        None::<&gtk::gio::Cancellable>,
        move |result| match result {
            Ok(_) => {
                terminal_for_callback.feed_child(format!("new-ui mi2 {mi_path}\n").as_bytes());
                spawn_event(SessionEvent::Spawned);
            }
            Err(error) => spawn_event(SessionEvent::Failed(error.to_string())),
        },
    );
}
