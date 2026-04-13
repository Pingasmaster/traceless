use std::sync::mpsc;
use traceless_core::FileStoreEvent;

/// Bridge between core worker threads and the Qt event loop.
/// The Qt frontend polls this in a timer callback.
pub struct EventBridge {
    rx: mpsc::Receiver<FileStoreEvent>,
}

impl EventBridge {
    pub const fn new(rx: mpsc::Receiver<FileStoreEvent>) -> Self {
        Self { rx }
    }

    /// Drain all pending events from the channel.
    pub fn drain_events(&self) -> Vec<FileStoreEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.rx.try_recv() {
            events.push(event);
        }
        events
    }
}
