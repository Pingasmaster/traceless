import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

RowLayout {
    property string metadataKey: ""
    property string metadataValue: ""

    spacing: 12

    Label {
        text: metadataKey
        opacity: 0.6
        Layout.preferredWidth: 120
        horizontalAlignment: Text.AlignRight
        wrapMode: Text.Wrap
        font.pixelSize: 12
    }

    Label {
        text: metadataValue
        Layout.fillWidth: true
        wrapMode: Text.Wrap
    }
}
