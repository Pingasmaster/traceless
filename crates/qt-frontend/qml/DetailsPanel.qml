import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

Page {
    id: detailsPanel

    required property var model
    signal backClicked()

    header: ToolBar {
        RowLayout {
            anchors.fill: parent
            anchors.leftMargin: 4
            anchors.rightMargin: 8

            ToolButton {
                icon.name: "go-previous"
                onClicked: detailsPanel.backClicked()
            }

            Label {
                text: "Details"
                font.bold: true
                Layout.fillWidth: true
            }
        }
    }

    ScrollView {
        anchors.fill: parent
        anchors.margins: 12

        ColumnLayout {
            width: parent.width
            spacing: 8

            Label {
                text: detailsPanel.model.group_name
                font.bold: true
                visible: detailsPanel.model.group_name.length > 0
                wrapMode: Text.Wrap
                Layout.fillWidth: true
            }

            Repeater {
                model: detailsPanel.model.count

                MetadataSection {
                    Layout.fillWidth: true
                    metadataKey: detailsPanel.model.get_key(index)
                    metadataValue: detailsPanel.model.get_value(index)
                }
            }

            Label {
                text: "No metadata to display"
                visible: detailsPanel.model.count === 0
                opacity: 0.5
                Layout.alignment: Qt.AlignHCenter
                Layout.topMargin: 40
            }
        }
    }
}
