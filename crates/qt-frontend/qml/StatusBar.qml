import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

Item {
    id: statusBar
    height: 48

    property string statusMessage: ""
    property bool isWorking: false

    signal cleanClicked()
    signal settingsChanged(bool lightweight)

    RowLayout {
        anchors.fill: parent
        anchors.leftMargin: 12
        anchors.rightMargin: 12
        spacing: 8

        // Status area
        Label {
            text: statusBar.statusMessage
            visible: statusBar.statusMessage.length > 0
            Layout.fillWidth: true
            elide: Text.ElideRight
        }

        BusyIndicator {
            width: 24
            height: 24
            running: statusBar.isWorking
            visible: statusBar.isWorking
        }

        Item {
            Layout.fillWidth: true
            visible: !statusBar.isWorking && statusBar.statusMessage.length === 0
        }

        // Settings button
        ToolButton {
            icon.name: "configure"
            onClicked: settingsPopup.open()

            SettingsPopup {
                id: settingsPopup
                onLightweightChanged: function(enabled) {
                    statusBar.settingsChanged(enabled)
                }
            }
        }

        // Clean button
        Button {
            text: "Clean"
            highlighted: true
            palette.button: "#c01c28"
            palette.buttonText: "white"
            onClicked: statusBar.cleanClicked()
        }
    }
}
