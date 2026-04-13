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
    MetadataModel { id: metadataModel }
    AppController { id: appController }

    FileDialog {
        id: fileDialog
        title: "Add Files"
        fileMode: FileDialog.OpenFiles
        onAccepted: {
            var paths = []
            for (var i = 0; i < selectedFiles.length; i++) {
                var path = selectedFiles[i].toString().replace("file://", "")
                paths.push(path)
            }
            fileModel.add_files(paths.join("\n"))
        }
    }

    FolderDialog {
        id: folderDialog
        title: "Add Folder"
        onAccepted: {
            var path = selectedFolder.toString().replace("file://", "")
            fileModel.add_folder(path)
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

        DetailsPanel {
            anchors.fill: parent
            model: metadataModel
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
                paths.push(drop.urls[i].toString().replace(/^file:\/\//, ""))
            }
            if (paths.length > 0) {
                fileModel.add_files(paths.join("\n"))
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
                    detailsDrawer.open()
                }
                onRemoveClicked: function(index) {
                    fileModel.remove_file(index)
                }
                onCleanClicked: cleaningWarning.open()
                onSettingsChanged: function(lightweight) {
                    fileModel.set_lightweight_mode(lightweight)
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
