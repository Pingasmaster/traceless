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

/// Show the Preferences dialog. Currently hosts a single switch: the
/// process-wide "Disable all limits" master toggle.
///
/// The dialog lists every cap the toggle flips so the user knows
/// exactly what they're removing when they turn it on. The toggle
/// mutates `traceless_core`'s static atomic immediately; there is no
/// Apply button because nothing about the change is persisted to
/// disk - closing the dialog, or toggling back, reverts the effect on
/// the next handler call.
pub fn show_preferences_dialog(parent: &impl IsA<gtk::Window>) {
    let dialog = adw::PreferencesDialog::builder()
        .title("Preferences")
        .build();

    let page = adw::PreferencesPage::builder()
        .title("Limits")
        .icon_name("preferences-system-symbolic")
        .build();

    let group = adw::PreferencesGroup::builder()
        .title("Resource limits")
        .description(
            "Traceless enforces per-file caps so a single huge or \
             adversarial input can't hang the cleaner or exhaust the \
             host. Disable them only if you understand what your \
             inputs look like and accept the consequences.",
        )
        .build();

    // The master toggle.
    let row = adw::SwitchRow::builder()
        .title("Disable all limits")
        .subtitle("Removes every cap listed below. Takes effect immediately.")
        .active(traceless_core::limits_disabled())
        .build();
    row.connect_active_notify(|row| {
        traceless_core::set_limits_disabled(row.is_active());
    });
    group.add(&row);

    page.add(&group);

    // One row per cap, each as a read-only ActionRow so the user can
    // see exactly what "disable all limits" means. The units are
    // rendered via the same helper the dialog-test fixtures use, so a
    // future bump to any cap shows up here automatically on the next
    // open without a string to update.
    let detail = adw::PreferencesGroup::builder()
        .title("What gets disabled")
        .description(
            "Each row shows the cap as it ships in release builds. \
             Flipping the switch above makes every one of them a \
             no-op for the rest of this session.",
        )
        .build();

    let rows: [(&str, String); 5] = [
        (
            "Per-file input size",
            format!(
                "Rejects any single file larger than {}",
                format_bytes(traceless_core::MAX_INPUT_FILE_BYTES),
            ),
        ),
        (
            "Handler wall-clock cap",
            format!(
                "Aborts a handler that has been running longer than {} seconds",
                traceless_core::HANDLER_WALL_CLOCK_CAP.as_secs(),
            ),
        ),
        (
            "Per-archive-member decompression",
            format!(
                "Rejects any single ZIP/TAR/DOCX/ODT/EPUB member that decompresses to more than {}",
                format_bytes(traceless_core::MAX_ENTRY_DECOMPRESSED_BYTES),
            ),
        ),
        (
            "Tar outer-stream decompression",
            format!(
                "Rejects any .tar / .tar.gz / .tar.xz / .tar.zst whose decompressed body exceeds {}",
                format_bytes(traceless_core::MAX_TAR_DECOMPRESSED_BYTES),
            ),
        ),
        (
            "Cumulative archive decompression",
            format!(
                "Rejects an archive whose members sum to more than {} decompressed",
                format_bytes(traceless_core::MAX_ARCHIVE_TOTAL_DECOMPRESSED_BYTES),
            ),
        ),
    ];
    for (title, subtitle) in rows {
        let row = adw::ActionRow::builder().title(title).subtitle(subtitle).build();
        detail.add(&row);
    }

    page.add(&detail);
    dialog.add(&page);

    dialog.present(Some(&parent.clone().upcast::<gtk::Window>()));
}

/// Render a byte count the same way the GNOME file managers do - IEC
/// binary prefixes (KiB/MiB/GiB), one decimal place when useful.
/// Kept inside the dialogs module because only the preferences view
/// needs it.
fn format_bytes(n: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;
    if n >= GIB {
        let whole = n / GIB;
        let rem = (n % GIB) * 10 / GIB;
        if rem == 0 {
            format!("{whole} GiB")
        } else {
            format!("{whole}.{rem} GiB")
        }
    } else if n >= MIB {
        let whole = n / MIB;
        let rem = (n % MIB) * 10 / MIB;
        if rem == 0 {
            format!("{whole} MiB")
        } else {
            format!("{whole}.{rem} MiB")
        }
    } else if n >= KIB {
        format!("{} KiB", n / KIB)
    } else {
        format!("{n} B")
    }
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
