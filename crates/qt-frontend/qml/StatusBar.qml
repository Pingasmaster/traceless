import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

Item {
    id: statusBar
    height: 48

    property string statusMessage: ""
    property bool isWorking: false

    signal cleanClicked()

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

        // Clean button. Disabled while any file is still scanning so
        // the user cannot trigger a partial clean that silently skips
        // files whose state has not yet transitioned to cleanable.
        // Matches the GTK frontend's `cleanable_count() > 0 &&
        // !has_working()` gate.
        Button {
            text: "Clean"
            highlighted: true
            enabled: !statusBar.isWorking
            palette.button: "#c01c28"
            palette.buttonText: "white"
            onClicked: statusBar.cleanClicked()
        }
    }
}
