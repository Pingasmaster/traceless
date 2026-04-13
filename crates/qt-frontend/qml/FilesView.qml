import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

Item {
    id: filesView

    required property var model
    signal fileClicked(int index)
    signal removeClicked(int index)
    signal cleanClicked()

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
                model: filesView.model

                delegate: FileDelegate {
                    width: fileList.width
                    fileIndex: index
                    fileName: filename
                    fileDirectory: directory
                    simpleState: model.simpleState
                    metadataCount: model.metadataCount
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
        }
    }
}
