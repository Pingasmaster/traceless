use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use async_channel::Sender;

use crate::bridge;
use crate::details_view::DetailsView;
use crate::dialogs;
use crate::empty_view;
use crate::files_view::FilesView;
use traceless_core::{FileStore, FileStoreEvent};

pub struct Window;

impl Window {
    // GTK application window builder: linear widget construction + signal wiring.
    // Splitting this into helpers would scatter tightly-coupled Rc<RefCell<_>> state
    // across functions without creating any real abstraction, so the length is intrinsic.
    #[allow(clippy::too_many_lines)]
    pub fn build(app: &adw::Application) -> adw::ApplicationWindow {
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("Traceless")
            .default_width(500)
            .default_height(700)
            .build();
        window.add_css_class("main");

        // Shared state
        let store = Rc::new(RefCell::new(FileStore::new()));
        let tx: Rc<RefCell<Option<Sender<FileStoreEvent>>>> =
            Rc::new(RefCell::new(None));
        let show_warning = Rc::new(RefCell::new(true));

        // --- Layout ---

        // Header bar
        let header = adw::HeaderBar::new();

        let title_widget = adw::WindowTitle::new("Traceless", "");
        header.set_title_widget(Some(&title_widget));

        // Add Files button
        let add_files_btn = gtk::Button::with_label("Add Files");
        add_files_btn.set_tooltip_text(Some("Add files (Ctrl+O)"));

        // Add Folders button
        let add_folders_btn = gtk::Button::with_label("Add Folders");
        add_folders_btn.set_tooltip_text(Some("Add folder"));

        header.pack_start(&add_files_btn);
        header.pack_start(&add_folders_btn);

        // Menu button
        let menu = gtk::gio::Menu::new();
        menu.append(Some("New Window"), Some("app.new-window"));
        menu.append(Some("Clear Window"), Some("win.clear-files"));
        let section2 = gtk::gio::Menu::new();
        section2.append(Some("About Traceless"), Some("win.about"));
        menu.append_section(None, &section2);

        let menu_btn = gtk::MenuButton::new();
        menu_btn.set_icon_name("open-menu-symbolic");
        menu_btn.set_menu_model(Some(&menu));
        header.pack_end(&menu_btn);

        // View stack: empty vs files
        let view_stack = gtk::Stack::new();
        view_stack.set_vexpand(true);
        view_stack.set_transition_type(gtk::StackTransitionType::Crossfade);

        let empty_view = empty_view::create_empty_view();
        view_stack.add_named(&empty_view, Some("empty"));

        let files_view = Rc::new(FilesView::new());
        view_stack.add_named(&files_view.widget, Some("files"));

        view_stack.set_visible_child_name("empty");

        // Header + content wrapped in a ToolbarView (libadwaita 1.4+).
        let toolbar_view = adw::ToolbarView::new();
        toolbar_view.add_top_bar(&header);
        toolbar_view.set_content(Some(&view_stack));
        toolbar_view.set_top_bar_style(adw::ToolbarStyle::Raised);

        // Details panel as the trailing side of an OverlaySplitView
        // (the libadwaita 1.4+ replacement for the deprecated AdwFlap).
        let split = adw::OverlaySplitView::new();
        split.set_sidebar_position(gtk::PackType::End);
        split.set_content(Some(&toolbar_view));
        split.set_show_sidebar(false);
        split.set_max_sidebar_width(360.0);
        split.set_min_sidebar_width(320.0);

        let split_for_back = split.clone();
        let details_view = Rc::new(DetailsView::new(move || {
            split_for_back.set_show_sidebar(false);
        }));
        split.set_sidebar(Some(&details_view.widget));

        window.set_content(Some(&split));

        // Collapse the split on narrow windows (responsive).
        let breakpoint =
            adw::Breakpoint::new(adw::BreakpointCondition::new_length(
                adw::BreakpointConditionLengthType::MaxWidth,
                600.0,
                adw::LengthUnit::Sp,
            ));
        breakpoint.add_setter(&split, "collapsed", Some(&true.to_value()));
        window.add_breakpoint(breakpoint);

        // --- Event handling setup ---
        let (event_tx, event_rx) = async_channel::unbounded::<FileStoreEvent>();
        *tx.borrow_mut() = Some(event_tx);

        // Wire up the row click callbacks once; the callbacks get the *current*
        // row index at click-time from the enclosing ListBoxRow.
        {
            let on_remove = {
                let store = store.clone();
                let files_view = files_view.clone();
                let view_stack = view_stack.clone();
                let split = split.clone();
                move |idx: usize| {
                    store.borrow_mut().remove_file(idx);
                    let s = store.borrow();
                    files_view.remove_row(&s, idx);
                    if s.is_empty() {
                        view_stack.set_visible_child_name("empty");
                        split.set_show_sidebar(false);
                    }
                }
            };
            let on_details = {
                let store = store.clone();
                let dv = details_view;
                let split = split.clone();
                move |idx: usize| {
                    let s = store.borrow();
                    if let Some(entry) = s.get(idx) {
                        dv.show_file(entry);
                        split.set_show_sidebar(true);
                    }
                }
            };
            files_view.bind_callbacks(on_remove, on_details);
        }

        // Install event pump from worker threads -> GTK main loop
        {
            let store = store.clone();
            let files_view = files_view.clone();

            bridge::install_event_pump(event_rx, move |event| {
                let affected: Option<usize> = match &event {
                    FileStoreEvent::FileStateChanged { index, .. }
                    | FileStoreEvent::MetadataReady { index, .. }
                    | FileStoreEvent::FileError { index, .. } => Some(*index),
                    FileStoreEvent::AllDone => None,
                };
                store.borrow_mut().apply_event(&event);
                let s = store.borrow();
                if let Some(idx) = affected {
                    files_view.update_row(&s, idx);
                }

                // Status bar update
                let cleaned = s.cleaned_count();
                let total = s.len();
                if !s.has_working() && cleaned > 0 {
                    files_view.status.set_done(&format!(
                        "{cleaned} file{} cleaned.",
                        if cleaned == 1 { "" } else { "s" }
                    ));
                } else if s.has_working() {
                    let done = s.files().iter().filter(|f| !f.state.is_working()).count();
                    let frac = if total > 0 {
                        let done_f = f64::from(u32::try_from(done).unwrap_or(u32::MAX));
                        let total_f = f64::from(u32::try_from(total).unwrap_or(u32::MAX));
                        done_f / total_f
                    } else {
                        0.0
                    };
                    files_view.status.set_working(frac);
                } else {
                    files_view.status.set_idle();
                }
            });
        }

        // --- Connect Actions ---

        // Lightweight cleaning toggle
        {
            let store = store.clone();
            files_view.settings_switch.connect_state_set(move |_, active| {
                store.borrow_mut().lightweight_mode = active;
                gtk::glib::Propagation::Proceed
            });
        }

        // Add Files button
        {
            let window_clone = window.clone();
            let store = store.clone();
            let tx = tx.clone();
            let view_stack = view_stack.clone();
            let files_view = files_view.clone();
            add_files_btn.connect_clicked(move |_| {
                let store = store.clone();
                let tx = tx.clone();
                let vs = view_stack.clone();
                let fv = files_view.clone();
                dialogs::show_file_chooser(&window_clone, move |paths| {
                    if !paths.is_empty()
                        && let Some(sender) = tx.borrow().as_ref()
                    {
                        let start = store.borrow().len();
                        store.borrow_mut().add_files(paths, sender);
                        fv.append_new(&store.borrow(), start);
                        vs.set_visible_child_name("files");
                    }
                });
            });
        }

        // Add Folders button
        {
            let window_clone = window.clone();
            let store = store.clone();
            let tx = tx.clone();
            let view_stack = view_stack.clone();
            let files_view = files_view.clone();
            add_folders_btn.connect_clicked(move |_| {
                let store = store.clone();
                let tx = tx.clone();
                let vs = view_stack.clone();
                let fv = files_view.clone();
                dialogs::show_folder_chooser(&window_clone, move |path| {
                    if let Some(sender) = tx.borrow().as_ref() {
                        let start = store.borrow().len();
                        store.borrow_mut().add_directory(&path, true, sender);
                        fv.append_new(&store.borrow(), start);
                        vs.set_visible_child_name("files");
                    }
                });
            });
        }

        // Clean button
        {
            let window_clone = window.clone();
            let store = store.clone();
            let tx = tx.clone();
            files_view.clean_button.connect_clicked(move |_| {
                let store = store.clone();
                let tx = tx.clone();
                let sw = show_warning.clone();

                if *sw.borrow() {
                    let store2 = store;
                    let tx2 = tx;
                    dialogs::show_cleaning_warning(&window_clone, move |confirmed| {
                        if confirmed
                            && let Some(sender) = tx2.borrow().as_ref()
                        {
                            store2.borrow_mut().clean_files(sender);
                        }
                    });
                } else if let Some(sender) = tx.borrow().as_ref() {
                    store.borrow_mut().clean_files(sender);
                }
            });
        }

        // Drag-and-drop: accept files and folders dropped onto the window.
        {
            let drop_target = gtk::DropTarget::new(
                gtk::gdk::FileList::static_type(),
                gtk::gdk::DragAction::COPY,
            );

            let store_drop = store.clone();
            let vs_drop = view_stack.clone();
            let fv_drop = files_view.clone();
            let window_drop = window.clone();
            drop_target.connect_drop(move |_, value, _, _| {
                let Ok(file_list) = value.get::<gtk::gdk::FileList>() else {
                    return false;
                };
                let mut files: Vec<PathBuf> = Vec::new();
                let mut dirs: Vec<PathBuf> = Vec::new();
                for file in file_list.files() {
                    if let Some(path) = file.path() {
                        if path.is_dir() {
                            dirs.push(path);
                        } else {
                            files.push(path);
                        }
                    }
                }
                if files.is_empty() && dirs.is_empty() {
                    window_drop.remove_css_class("drop-target");
                    return false;
                }
                let Some(sender) = tx.borrow().clone() else {
                    window_drop.remove_css_class("drop-target");
                    return false;
                };
                let start = store_drop.borrow().len();
                if !files.is_empty() {
                    store_drop.borrow_mut().add_files(files, &sender);
                }
                for dir in dirs {
                    store_drop.borrow_mut().add_directory(&dir, true, &sender);
                }
                fv_drop.append_new(&store_drop.borrow(), start);
                vs_drop.set_visible_child_name("files");
                window_drop.remove_css_class("drop-target");
                true
            });

            let window_enter = window.clone();
            drop_target.connect_enter(move |_, _, _| {
                window_enter.add_css_class("drop-target");
                gtk::gdk::DragAction::COPY
            });

            let window_leave = window.clone();
            drop_target.connect_leave(move |_| {
                window_leave.remove_css_class("drop-target");
            });

            window.add_controller(drop_target);
        }

        // Window actions
        let clear_action = gtk::gio::SimpleAction::new("clear-files", None);
        {
            clear_action.connect_activate(move |_, _| {
                store.borrow_mut().clear();
                files_view.clear_rows();
                view_stack.set_visible_child_name("empty");
                split.set_show_sidebar(false);
            });
        }
        window.add_action(&clear_action);

        let about_action = gtk::gio::SimpleAction::new("about", None);
        {
            let window_clone = window.clone();
            about_action.connect_activate(move |_, _| {
                dialogs::show_about_dialog(&window_clone);
            });
        }
        window.add_action(&about_action);

        window
    }
}
