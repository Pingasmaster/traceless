import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

ItemDelegate {
    id: delegate

    required property int fileIndex
    required property string fileName
    required property string fileDirectory
    required property string simpleState
    required property int metadataCount

    signal removeRequested()

    contentItem: RowLayout {
        spacing: 0

        // Remove button
        ToolButton {
            icon.name: "edit-delete"
            onClicked: delegate.removeRequested()
            implicitWidth: 40
        }

        // Separator
        Rectangle {
            width: 1
            Layout.fillHeight: true
            color: palette.mid
            opacity: delegate.hovered ? 1.0 : 0.0
        }

        // File icon
        Item {
            width: 40
            height: 40
            Layout.leftMargin: 8

            Label {
                anchors.centerIn: parent
                text: {
                    if (delegate.fileName.match(/\.(jpg|jpeg|png|webp)$/i)) return "\u{1F5BC}"
                    if (delegate.fileName.match(/\.(mp3|flac|ogg|wav|m4a)$/i)) return "\u{1F3B5}"
                    if (delegate.fileName.match(/\.(mp4|mkv|webm|avi|mov)$/i)) return "\u{1F3AC}"
                    if (delegate.fileName.match(/\.pdf$/i)) return "\u{1F4C4}"
                    return "\u{1F4C1}"
                }
                font.pixelSize: 24
            }
        }

        // Name + directory
        ColumnLayout {
            Layout.fillWidth: true
            Layout.leftMargin: 8
            spacing: 2

            Label {
                text: delegate.fileName
                elide: Text.ElideMiddle
                Layout.fillWidth: true
            }
            Label {
                text: delegate.fileDirectory
                visible: delegate.fileDirectory.length > 0
                opacity: 0.6
                font.pixelSize: 11
                elide: Text.ElideMiddle
                Layout.fillWidth: true
            }
        }

        // Badge
        Badge {
            state: delegate.simpleState
            count: delegate.metadataCount
        }

        // Arrow
        Label {
            text: "\u{203A}" // single right angle quote
            font.pixelSize: 20
            opacity: 0.5
            Layout.rightMargin: 8
        }
    }

    // Bottom border
    Rectangle {
        anchors.bottom: parent.bottom
        width: parent.width
        height: 1
        color: palette.mid
        opacity: 0.3
    }
}
