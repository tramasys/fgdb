mod app;
mod breakpoint_gutter;
mod config;
mod debugger;
mod kernel;
mod source;
mod theme;
mod ui;

use gtk::prelude::*;

fn main() -> gtk::glib::ExitCode {
    let launch_config = match config::LaunchConfig::from_process() {
        Ok(configuration) => configuration,
        Err(error) => {
            eprintln!("invalid FGDB_GDB_ARGS: {error}");
            return gtk::glib::ExitCode::FAILURE;
        }
    };
    let application = gtk::Application::builder()
        .application_id("dev.fgdb.Fgdb")
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();

    application.connect_activate(move |application| {
        app::build(application, launch_config.clone());
    });

    // The process arguments belong to GDB, not GApplication. Passing them to
    // `run` would make GApplication interpret the target as a file-open request.
    application.run_with_args(&["fgdb"])
}
