import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

Item {
    ColumnLayout {
        anchors.centerIn: parent
        spacing: 16

        // Icon placeholder
        Rectangle {
            width: 96
            height: 96
            radius: 48
            color: "#813d9c"
            Layout.alignment: Qt.AlignHCenter

            Label {
                anchors.centerIn: parent
                text: "\u{1F9F9}" // broom emoji as placeholder
                font.pixelSize: 48
            }
        }

        Label {
            text: "Clean Your Traces"
            font.pixelSize: 24
            font.bold: true
            Layout.alignment: Qt.AlignHCenter
        }

        Label {
            text: "Add files or folders to view and remove their metadata."
            opacity: 0.7
            wrapMode: Text.WordWrap
            horizontalAlignment: Text.AlignHCenter
            Layout.maximumWidth: 300
            Layout.alignment: Qt.AlignHCenter
        }
    }
}
