use async_channel::Receiver;
use gtk::glib;
use traceless_core::FileStoreEvent;

/// Spawn an event-driven task on the GTK main loop that awaits events from
/// worker threads and dispatches each one to the callback. The future ends
/// when the channel is closed.
pub fn install_event_pump<F>(rx: Receiver<FileStoreEvent>, callback: F)
where
    F: Fn(FileStoreEvent) + 'static,
{
    glib::spawn_future_local(async move {
        while let Ok(event) = rx.recv().await {
            callback(event);
        }
    });
}
