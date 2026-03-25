use adw::prelude::*;

/// Create the welcome/empty state view.
pub fn create_empty_view() -> adw::StatusPage {
    let page = adw::StatusPage::new();
    page.set_icon_name(Some("edit-clear-all-symbolic"));
    page.set_title("Clean Your Traces");
    page.set_description(Some(
        "Add files or folders to view and remove their metadata.",
    ));
    page.set_vexpand(true);
    page
}
