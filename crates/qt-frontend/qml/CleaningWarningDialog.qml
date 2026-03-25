import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

Dialog {
    id: warningDialog
    title: "Make sure you backed up your files!"
    standardButtons: Dialog.Cancel | Dialog.Ok
    modal: true
    anchors.centerIn: parent
    width: 400

    ColumnLayout {
        spacing: 12
        width: parent.width

        Label {
            text: "Once the files are cleaned, there's no going back."
            wrapMode: Text.WordWrap
            Layout.fillWidth: true
        }
    }
}
