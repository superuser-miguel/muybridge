//! muybridge — a GTK4/libadwaita front end for pulling still frames out of
//! video with the bundled ffmpeg.
//!
//! Entry point only: register the compiled GResource, start an
//! `adw::Application`, and present the composite-template window. The command
//! contract lives in the `ffmpeg-frames` crate, the UI in `window.rs`.

mod filmstrip;
mod runner;
mod window;

mod config {
    include!(concat!(env!("OUT_DIR"), "/config.rs"));
}

use adw::prelude::*;
use gtk::gio;
use gtk::glib;
use std::path::PathBuf;
use window::MuybridgeWindow;

fn main() -> glib::ExitCode {
    register_resources();

    let app = adw::Application::builder()
        .application_id(config::APP_ID)
        // So `muybridge some-clip.mp4` — and a file manager's Open With — start
        // on a video instead of on the picker.
        .flags(gio::ApplicationFlags::HANDLES_OPEN)
        .build();

    app.connect_startup(|app| {
        setup_actions(app);
        load_css();
    });
    app.connect_activate(|app| {
        present_window(app);
    });
    app.connect_open(|app, files, _| {
        let window = present_window(app);
        // One window, one job: anything past the first file is not something
        // this app has a way to show.
        if let Some(path) = files.first().and_then(|file| file.path()) {
            window.open_video(path);
        }
    });

    app.run()
}

fn present_window(app: &adw::Application) -> MuybridgeWindow {
    let window = MuybridgeWindow::new(app);
    if config::PROFILE == "development" {
        // libadwaita renders the striped "devel" header for unreleased builds.
        window.add_css_class("devel");
    }
    window.present();
    window
}

/// Load `muybridge.gresource`. In an installed build it lives in `PKGDATADIR`;
/// for host dev runs, point `MUYBRIDGE_GRESOURCE` at the file Meson built.
fn register_resources() {
    let path = std::env::var_os("MUYBRIDGE_GRESOURCE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(config::PKGDATADIR).join("muybridge.gresource"));

    let resource = gio::Resource::load(&path)
        .unwrap_or_else(|e| panic!("failed to load GResource at {}: {e}", path.display()));
    gio::resources_register(&resource);
}

/// Three rules: a chunkier progress bar with a bigger readout, because during a
/// long extraction it is the only thing on screen worth looking at; the outline
/// that says a dragged file will be taken, using `outline` rather than `border`
/// so the highlight cannot shift the form while it appears; and a black bed for
/// the scrub preview, which is what an image of unknown aspect ratio should be
/// letterboxed against in either light or dark.
fn load_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(
        "progressbar.muybridge-progress > trough,
         progressbar.muybridge-progress > trough > progress { min-height: 24px; }
         progressbar.muybridge-progress > trough > progress { border-radius: 8px; }
         progressbar.muybridge-progress > text {
             font-size: 1.1em;
             font-weight: bold;
             margin-bottom: 4px;
         }
         .muybridge-drop {
             outline: 2px dashed @accent_color;
             outline-offset: 4px;
             border-radius: 12px;
         }
         .muybridge-frame {
             background-color: black;
             border-radius: 12px;
         }",
    );
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

fn setup_actions(app: &adw::Application) {
    let about = gio::SimpleAction::new("about", None);
    about.connect_activate(glib::clone!(
        #[weak]
        app,
        move |_, _| {
            let window = app.active_window();
            adw::AboutDialog::builder()
                .application_name("Muybridge")
                .application_icon(config::APP_ID)
                .version(config::VERSION)
                .developer_name("The Muybridge contributors")
                .license_type(gtk::License::Gpl30)
                .comments("Take still frames out of video, at whatever rate you ask for.")
                .website("https://github.com/superuser-miguel/muybridge")
                .build()
                .present(window.as_ref());
        }
    ));
    app.add_action(&about);

    app.set_accels_for_action("win.new-job", &["<Primary>n"]);
    app.set_accels_for_action("window.close", &["<Primary>w"]);
    app.set_accels_for_action("app.quit", &["<Primary>q"]);

    let quit = gio::SimpleAction::new("quit", None);
    quit.connect_activate(glib::clone!(
        #[weak]
        app,
        move |_, _| app.quit()
    ));
    app.add_action(&quit);
}
