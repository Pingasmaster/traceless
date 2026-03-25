import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

Popup {
    id: settingsPopup
    width: 250
    padding: 12

    signal lightweightChanged(bool enabled)

    RowLayout {
        anchors.fill: parent
        spacing: 12

        Label {
            text: "Lightweight Cleaning"
            Layout.fillWidth: true
        }

        Switch {
            id: lightweightSwitch
            onToggled: settingsPopup.lightweightChanged(checked)
        }
    }
}
