use adw::prelude::*;

use crate::style;
use crate::window::Window;

const APP_ID: &str = "io.github.traceless";

pub fn run() {
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .build();

    app.connect_startup(|_| {
        style::load_css();
    });

    app.connect_activate(move |app| {
        let window = Window::build(app);
        window.present();
    });

    // Run the application
    let empty: Vec<String> = vec![];
    app.run_with_args(&empty);
}
