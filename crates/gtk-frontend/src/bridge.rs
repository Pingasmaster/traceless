use std::sync::mpsc;
use std::time::Duration;

use gtk::glib;
use traceless_core::FileStoreEvent;

/// Install a recurring timer that drains the event channel and calls the
/// callback for each event. This bridges worker threads to the GTK main loop.
pub fn install_event_pump<F>(rx: mpsc::Receiver<FileStoreEvent>, callback: F)
where
    F: Fn(FileStoreEvent) + 'static,
{
    glib::timeout_add_local(Duration::from_millis(16), move || {
        while let Ok(event) = rx.try_recv() {
            callback(event);
        }
        glib::ControlFlow::Continue
    });
}
