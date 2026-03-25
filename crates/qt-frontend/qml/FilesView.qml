import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

Item {
    id: filesView

    required property var model
    signal fileClicked(int index)
    signal removeClicked(int index)
    signal cleanClicked()
    signal settingsChanged(bool lightweight)

    ColumnLayout {
        anchors.fill: parent
        spacing: 0

        // File list
        ScrollView {
            Layout.fillWidth: true
            Layout.fillHeight: true

            ListView {
                id: fileList
                clip: true
                model: filesView.model.file_count

                delegate: FileDelegate {
                    width: fileList.width
                    fileIndex: index
                    fileName: filesView.model.get_filename(index)
                    fileDirectory: filesView.model.get_directory(index)
                    simpleState: filesView.model.get_simple_state(index)
                    metadataCount: filesView.model.get_metadata_count(index)
                    onClicked: filesView.fileClicked(index)
                    onRemoveRequested: filesView.removeClicked(index)
                }
            }
        }

        // Separator
        Rectangle {
            Layout.fillWidth: true
            height: 1
            color: palette.mid
        }

        // Bottom toolbar
        StatusBar {
            Layout.fillWidth: true
            statusMessage: filesView.model.status_message
            isWorking: filesView.model.is_working
            onCleanClicked: filesView.cleanClicked()
            onSettingsChanged: function(lightweight) {
                filesView.settingsChanged(lightweight)
            }
        }
    }
}
