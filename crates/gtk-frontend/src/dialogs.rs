use adw::prelude::*;

/// Show a file chooser dialog for selecting files.
pub fn show_file_chooser(
    parent: &impl IsA<gtk::Window>,
    callback: impl Fn(Vec<std::path::PathBuf>) + 'static,
) {
    let dialog = gtk::FileDialog::builder()
        .title("Add Files")
        .modal(true)
        .build();

    let parent_clone = parent.clone().upcast::<gtk::Window>();
    dialog.open_multiple(
        Some(&parent_clone),
        gtk::gio::Cancellable::NONE,
        move |result| {
            if let Ok(files) = result {
                let mut paths = Vec::new();
                for i in 0..files.n_items() {
                    if let Some(file) = files.item(i)
                        && let Some(gfile) = file.downcast_ref::<gtk::gio::File>()
                        && let Some(path) = gfile.path()
                    {
                        paths.push(path);
                    }
                }
                callback(paths);
            }
        },
    );
}

/// Show a folder chooser dialog.
pub fn show_folder_chooser(
    parent: &impl IsA<gtk::Window>,
    callback: impl Fn(std::path::PathBuf) + 'static,
) {
    let dialog = gtk::FileDialog::builder()
        .title("Add Folder")
        .modal(true)
        .build();

    let parent_clone = parent.clone().upcast::<gtk::Window>();
    dialog.select_folder(
        Some(&parent_clone),
        gtk::gio::Cancellable::NONE,
        move |result| {
            if let Ok(folder) = result
                && let Some(path) = folder.path()
            {
                callback(path);
            }
        },
    );
}

/// Show the cleaning warning dialog.
pub fn show_cleaning_warning(parent: &impl IsA<gtk::Window>, callback: impl Fn(bool) + 'static) {
    let dialog = adw::AlertDialog::builder()
        .heading("Make sure you backed up your files!")
        .body("Once the files are cleaned, there's no going back.")
        .build();

    dialog.add_response("cancel", "Cancel");
    dialog.add_response("clean", "Clean");
    dialog.set_response_appearance("clean", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");

    let parent_clone = parent.clone().upcast::<gtk::Window>();
    dialog.choose(
        Some(&parent_clone),
        gtk::gio::Cancellable::NONE,
        move |response| {
            callback(response == "clean");
        },
    );
}

/// Show the About dialog.
pub fn show_about_dialog(parent: &impl IsA<gtk::Window>) {
    let about = adw::AboutDialog::builder()
        .application_name("Traceless")
        .developer_name("Traceless Contributors")
        // `env!` resolves at compile time from the crate's Cargo.toml
        // version, so the About dialog stays in sync with the
        // workspace manifest automatically on any version bump.
        .version(env!("CARGO_PKG_VERSION"))
        .comments("View and remove metadata from your files.\n\nInspired by Metadata Cleaner by Romain Vigier.")
        .website("https://gitlab.com/rmnvgr/metadata-cleaner")
        .license_type(gtk::License::Gpl30)
        .build();

    about.present(Some(&parent.clone().upcast::<gtk::Window>()));
}
