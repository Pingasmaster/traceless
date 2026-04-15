import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import QtQuick.Dialogs
import Traceless

ApplicationWindow {
    id: root
    title: "Traceless"
    width: 500
    height: 700
    visible: true
    color: palette.window

    FileListModel { id: fileModel }
    AppController { id: appController }

    // Convert a QML url (file://…) to a decoded local filesystem path.
    // Returns an empty string for non-file URIs so the caller can filter.
    //
    // Plain `.toString().replace("file://", "")` is wrong in two ways:
    //   1. It leaves percent-encoded sequences (%20 for space, etc.),
    //      producing paths that don't exist on disk.
    //   2. It silently passes through non-`file://` URIs (e.g. http links
    //      dragged from a browser) as if they were local paths.
    function urlToLocalFile(url) {
        var s = url.toString()
        if (!s.startsWith("file://")) return ""
        return decodeURIComponent(s.substring(7))
    }

    FileDialog {
        id: fileDialog
        title: "Add Files"
        fileMode: FileDialog.OpenFiles
        onAccepted: {
            var paths = []
            for (var i = 0; i < selectedFiles.length; i++) {
                var p = urlToLocalFile(selectedFiles[i])
                if (p.length > 0) paths.push(p)
            }
            // NUL-delimited: NUL is the only byte that cannot appear in a
            // filename on POSIX, so it is the only safe delimiter for a
            // list that may contain paths with newlines, tabs, etc.
            if (paths.length > 0) fileModel.add_files(paths.join("\0"))
        }
    }

    FolderDialog {
        id: folderDialog
        title: "Add Folder"
        onAccepted: {
            var p = urlToLocalFile(selectedFolder)
            if (p.length > 0) fileModel.add_folder(p)
        }
    }

    CleaningWarningDialog {
        id: cleaningWarning
        onAccepted: fileModel.clean_all()
    }

    Drawer {
        id: detailsDrawer
        edge: Qt.RightEdge
        width: Math.min(360, root.width * 0.8)
        height: root.height
        interactive: true
        onClosed: fileModel.select_detail(-1)

        DetailsPanel {
            anchors.fill: parent
            model: fileModel
            onBackClicked: detailsDrawer.close()
        }
    }

    DropArea {
        id: dropZone
        anchors.fill: parent
        keys: ["text/uri-list"]

        onDropped: (drop) => {
            if (!drop.hasUrls) return
            var paths = []
            for (var i = 0; i < drop.urls.length; i++) {
                var p = urlToLocalFile(drop.urls[i])
                if (p.length > 0) paths.push(p)
            }
            if (paths.length > 0) {
                fileModel.add_files(paths.join("\0"))
            }
            drop.accept(Qt.CopyAction)
        }

        Rectangle {
            anchors.fill: parent
            visible: dropZone.containsDrag
            color: "transparent"
            border.color: "#813d9c"
            border.width: 3
            z: 1000
        }

    ColumnLayout {
        anchors.fill: parent
        spacing: 0

        // Header bar
        ToolBar {
            Layout.fillWidth: true
            palette.window: "#813d9c"
            palette.windowText: "white"
            palette.buttonText: "white"

            RowLayout {
                anchors.fill: parent
                anchors.leftMargin: 8
                anchors.rightMargin: 8

                Button {
                    text: "Add Files"
                    flat: true
                    onClicked: fileDialog.open()
                    palette.buttonText: "white"
                }

                Button {
                    text: "Add Folders"
                    flat: true
                    onClicked: folderDialog.open()
                    palette.buttonText: "white"
                }

                Item { Layout.fillWidth: true }

                Label {
                    text: "Traceless"
                    font.bold: true
                    color: "white"
                }

                Item { Layout.fillWidth: true }

                ToolButton {
                    icon.name: "application-menu"
                    palette.buttonText: "white"
                    onClicked: appMenu.open()

                    Menu {
                        id: appMenu
                        MenuItem {
                            text: "Clear Window"
                            onTriggered: fileModel.clear_files()
                        }
                        MenuSeparator {}
                        MenuItem {
                            text: "Preferences"
                            onTriggered: preferencesDialog.open()
                        }
                        MenuSeparator {}
                        MenuItem {
                            text: "About Traceless"
                            onTriggered: aboutDialog.open()
                        }
                    }
                }
            }
        }

        // Main content
        StackLayout {
            id: mainStack
            Layout.fillWidth: true
            Layout.fillHeight: true
            currentIndex: fileModel.file_count > 0 ? 1 : 0

            EmptyView {}

            FilesView {
                model: fileModel
                onFileClicked: function(index) {
                    fileModel.select_detail(index)
                    detailsDrawer.open()
                }
                onRemoveClicked: function(index) {
                    fileModel.remove_file(index)
                }
                onCleanClicked: cleaningWarning.open()
            }
        }
    }
    }

    Dialog {
        id: preferencesDialog
        title: "Preferences"
        standardButtons: Dialog.Close
        anchors.centerIn: parent
        modal: true
        width: Math.min(480, root.width * 0.9)

        ColumnLayout {
            spacing: 14
            width: parent.width

            Label {
                text: "Resource limits"
                font.bold: true
                font.pixelSize: 16
            }
            Label {
                // Matches the GTK preferences dialog body. Explains the
                // threat model the defaults guard against so a user
                // knows when it's safe to turn them off.
                text: "Traceless enforces per-file caps so a single huge or adversarial input can't hang the cleaner or exhaust the host. Disable them only if you understand what your inputs look like and accept the consequences."
                wrapMode: Text.WordWrap
                Layout.fillWidth: true
                opacity: 0.8
            }

            RowLayout {
                Layout.fillWidth: true
                Switch {
                    id: limitsSwitch
                    checked: appController.limits_disabled
                    onToggled: appController.set_limits_disabled_flag(checked)
                }
                ColumnLayout {
                    Label {
                        text: "Disable all limits"
                        font.bold: true
                    }
                    Label {
                        text: "Removes every cap listed below. Takes effect immediately."
                        opacity: 0.7
                        font.pixelSize: 11
                    }
                }
                Item { Layout.fillWidth: true }
            }

            Rectangle {
                Layout.fillWidth: true
                height: 1
                color: palette.mid
                opacity: 0.3
            }

            Label {
                text: "What gets disabled"
                font.bold: true
                font.pixelSize: 13
            }
            Label {
                text: "Each row shows the cap as it ships in release builds. Flipping the switch above makes every one of them a no-op for the rest of this session."
                wrapMode: Text.WordWrap
                Layout.fillWidth: true
                opacity: 0.7
                font.pixelSize: 11
            }

            // One row per cap. The subtitles come from AppController
            // invokables that read traceless-core's constants at call
            // time, so bumping any cap in the Rust source flows through
            // to this dialog automatically.
            Repeater {
                model: [
                    {
                        "title": "Per-file input size",
                        "body": "Rejects any single file larger than " + appController.limit_input_size()
                    },
                    {
                        "title": "Handler wall-clock cap",
                        "body": "Aborts a handler that has been running longer than " + appController.limit_handler_timeout()
                    },
                    {
                        "title": "Per-archive-member decompression",
                        "body": "Rejects any single ZIP/TAR/DOCX/ODT/EPUB member that decompresses to more than " + appController.limit_entry_decompressed()
                    },
                    {
                        "title": "Tar outer-stream decompression",
                        "body": "Rejects any .tar / .tar.gz / .tar.xz / .tar.zst whose decompressed body exceeds " + appController.limit_tar_decompressed()
                    },
                    {
                        "title": "Cumulative archive decompression",
                        "body": "Rejects an archive whose members sum to more than " + appController.limit_archive_total_decompressed() + " decompressed"
                    }
                ]
                delegate: ColumnLayout {
                    Layout.fillWidth: true
                    spacing: 2
                    Label {
                        text: modelData.title
                        font.bold: true
                        font.pixelSize: 12
                    }
                    Label {
                        text: modelData.body
                        wrapMode: Text.WordWrap
                        Layout.fillWidth: true
                        opacity: 0.75
                        font.pixelSize: 11
                    }
                }
            }
        }
    }

    Dialog {
        id: aboutDialog
        title: "About Traceless"
        standardButtons: Dialog.Ok
        anchors.centerIn: parent
        width: 350

        ColumnLayout {
            spacing: 12
            width: parent.width

            Label {
                text: "Traceless"
                font.pixelSize: 24
                font.bold: true
                Layout.alignment: Qt.AlignHCenter
            }
            Label {
                // `app_version` is a Q_PROPERTY populated at
                // construction from `env!("CARGO_PKG_VERSION")`, so
                // this label tracks the workspace Cargo.toml version
                // automatically on any bump.
                text: "Version " + appController.app_version
                Layout.alignment: Qt.AlignHCenter
                opacity: 0.7
            }
            Label {
                text: "View and remove metadata from your files."
                wrapMode: Text.WordWrap
                Layout.fillWidth: true
                horizontalAlignment: Text.AlignHCenter
            }
            Label {
                text: "Inspired by Metadata Cleaner by Romain Vigier.\nRewritten in Rust."
                wrapMode: Text.WordWrap
                Layout.fillWidth: true
                horizontalAlignment: Text.AlignHCenter
                opacity: 0.7
                font.pixelSize: 11
            }
        }
    }
}
