import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

Page {
    id: detailsPanel

    // The `FileListModel` — `detail_row`, `detail_count`, `detail_group`
    // are qproperties populated by `select_detail`, and `detail_key(i)` /
    // `detail_value(i)` / `detail_error()` are qinvokables.
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
                text: detailsPanel.model.detail_group
                font.bold: true
                visible: detailsPanel.model.detail_group.length > 0
                wrapMode: Text.Wrap
                Layout.fillWidth: true
            }

            Repeater {
                // Reading `detail_row` creates a QML binding on the
                // property's notify signal; whenever `select_detail` is
                // called, this Repeater reinstantiates with the new count.
                model: detailsPanel.model.detail_row >= 0
                       ? detailsPanel.model.detail_count
                       : 0

                MetadataSection {
                    Layout.fillWidth: true
                    metadataKey: detailsPanel.model.detail_key(index)
                    metadataValue: detailsPanel.model.detail_value(index)
                }
            }

            Label {
                id: errorLabel
                // `detail_error` is a Q_PROPERTY, not a Q_INVOKABLE,
                // so this binding re-evaluates whenever `select_detail`
                // writes a new value. The method-call form
                // `detail_error()` was only evaluated once at Label
                // creation and froze the error text for the life of
                // the page.
                text: detailsPanel.model.detail_error
                visible: detailsPanel.model.detail_row >= 0
                         && detailsPanel.model.detail_count === 0
                         && errorLabel.text.length > 0
                color: "#c01c28"
                wrapMode: Text.Wrap
                Layout.fillWidth: true
                Layout.topMargin: 12
            }

            Label {
                text: "No metadata to display"
                visible: detailsPanel.model.detail_row >= 0
                         && detailsPanel.model.detail_count === 0
                         && errorLabel.text.length === 0
                opacity: 0.5
                Layout.alignment: Qt.AlignHCenter
                Layout.topMargin: 40
            }
        }
    }
}
