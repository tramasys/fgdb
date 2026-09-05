mod app;
mod background;
mod bounded;
mod breakpoint_gutter;
mod compiler_probe;
mod config;
mod cpp_toolchain;
mod debug_info;
mod debugger;
mod kernel;
mod misc;
mod model;
mod performance;
mod rust_toolchain;
mod source;
mod theme;
mod ui;

use std::{io::IsTerminal, path::PathBuf};

use gtk::prelude::*;

use config::StartupAction;

pub(crate) const APPLICATION_ID: &str = "dev.fgdb.Fgdb";
pub(crate) const RESOURCE_PREFIX: &str = "/dev/fgdb/Fgdb";
const APPLICATION_ICON_SIZES: &[u16] = &[16, 24, 32, 48, 64, 128, 256];

pub(crate) fn install_window_icon(window: &impl IsA<gtk::Window>) {
    let textures = APPLICATION_ICON_SIZES
        .iter()
        .map(|size| {
            gtk::gdk::Texture::from_resource(&format!(
                "{RESOURCE_PREFIX}/icons/hicolor/{size}x{size}/apps/{APPLICATION_ID}.png"
            ))
        })
        .collect::<Vec<_>>();

    window.as_ref().connect_realize(move |window| {
        let Some(surface) = window.surface() else {
            return;
        };

        let Ok(toplevel) = surface.downcast::<gtk::gdk::Toplevel>() else {
            return;
        };

        toplevel.set_icon_list(&textures);
    });
}

fn main() -> gtk::glib::ExitCode {
    gtk::gio::resources_register_include!("fgdb.gresource")
        .expect("fgdb's bundled resources must be valid");

    match config::LaunchConfig::from_process() {
        Ok(StartupAction::Run(configuration)) => run_application(*configuration),
        Ok(StartupAction::Print(output)) => {
            print!("{output}");

            gtk::glib::ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");

            if error.should_show_graphically() && !std::io::stderr().is_terminal() {
                run_startup_error(
                    error.to_string(),
                    error.active_config_path().map(ToOwned::to_owned),
                )
            } else {
                gtk::glib::ExitCode::FAILURE
            }
        }
    }
}

fn run_application(launch_config: config::LaunchConfig) -> gtk::glib::ExitCode {
    let application = gtk::Application::builder()
        .application_id(APPLICATION_ID)
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();

    application.connect_activate(move |application| {
        app::build(application, launch_config.clone());
    });

    // The process arguments belong to GDB, not GApplication. Passing them to
    // `run` would make GApplication interpret the target as a file-open request.
    application.run_with_args(&["fgdb"])
}

fn run_startup_error(message: String, active_config_path: Option<PathBuf>) -> gtk::glib::ExitCode {
    let application = gtk::Application::builder()
        .application_id(APPLICATION_ID)
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();

    application.connect_activate(move |application| {
        let window = gtk::ApplicationWindow::builder()
            .application(application)
            .title("fgdb startup error")
            .icon_name(APPLICATION_ID)
            .default_width(620)
            .build();

        install_window_icon(&window);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.set_margin_top(18);
        content.set_margin_bottom(18);
        content.set_margin_start(18);
        content.set_margin_end(18);
        let heading = gtk::Label::new(Some("fgdb could not start"));
        heading.add_css_class("title-2");
        heading.set_halign(gtk::Align::Start);
        content.append(&heading);
        let detail = gtk::Label::new(Some(message.trim()));
        detail.add_css_class("monospace");
        detail.set_halign(gtk::Align::Start);
        detail.set_selectable(true);
        detail.set_wrap(true);
        detail.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        detail.set_max_width_chars(84);
        content.append(&detail);

        let hint_text = if active_config_path.is_some() {
            "Correct the configuration and start fgdb again."
        } else {
            "Run fgdb --help in a terminal to see valid options."
        };

        let hint = gtk::Label::new(Some(hint_text));
        hint.add_css_class("dim-label");
        hint.set_halign(gtk::Align::Start);
        content.append(&hint);
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        actions.set_halign(gtk::Align::End);

        if let Some(path) = active_config_path.as_ref() {
            let open = gtk::Button::with_label("Open active config");
            let file = gtk::gio::File::for_path(path);
            let launcher = gtk::FileLauncher::new(Some(&file));
            launcher.set_writable(true);
            let window_for_open = window.clone();

            open.connect_clicked(move |_| {
                let window_for_error = window_for_open.clone();

                launcher.launch(
                    Some(&window_for_open),
                    None::<&gtk::gio::Cancellable>,
                    move |result| {
                        if let Err(error) = result {
                            gtk::AlertDialog::builder()
                                .message("Could not open the configuration file")
                                .detail(error.to_string())
                                .modal(true)
                                .build()
                                .show(Some(&window_for_error));
                        }
                    },
                );
            });

            actions.append(&open);
        }

        let close = gtk::Button::with_label("Close");
        let window_for_close = window.clone();
        close.connect_clicked(move |_| window_for_close.close());
        actions.append(&close);
        content.append(&actions);
        window.set_child(Some(&content));
        window.present();
    });

    let _ = application.run_with_args(&["fgdb"]);

    gtk::glib::ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use gtk::gio;

    #[test]
    fn bundles_every_runtime_asset() {
        gio::resources_register_include!("fgdb.gresource").unwrap();

        for path in [
            "/dev/fgdb/Fgdb/icons/dev.fgdb.Fgdb.png",
            "/dev/fgdb/Fgdb/icons/hicolor/16x16/apps/dev.fgdb.Fgdb.png",
            "/dev/fgdb/Fgdb/icons/hicolor/256x256/apps/dev.fgdb.Fgdb.png",
            "/dev/fgdb/Fgdb/language-specs/assembly.lang",
            "/dev/fgdb/Fgdb/themes/carbon.xml",
        ] {
            assert!(
                gio::resources_lookup_data(path, gio::ResourceLookupFlags::NONE).is_ok(),
                "missing bundled resource {path}"
            );
        }
    }
}
