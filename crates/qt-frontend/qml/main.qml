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
                text: "Version 1.0.0"
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
