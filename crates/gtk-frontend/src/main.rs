#![forbid(unsafe_code)]
// See CLAUDE.md: transitive dep version duplication we cannot fix.
#![allow(clippy::multiple_crate_versions)]

mod app;
mod badge;
mod bridge;
mod details_view;
mod dialogs;
mod empty_view;
mod file_row;
mod files_view;
mod metadata_row;
mod settings_popover;
mod status_indicator;
mod style;
mod window;

fn main() {
    env_logger::init();
    app::run();
}
