import QtQuick
import QtQuick.Controls

Item {
    id: badge
    property string state: ""
    property int count: 0

    width: 28
    height: 28

    Rectangle {
        anchors.centerIn: parent
        width: Math.max(22, badgeLabel.implicitWidth + 12)
        height: 22
        radius: 11
        visible: badge.state === "has-metadata" || badge.state === "error"
                 || badge.state === "warning" || badge.state === "clean"
        color: {
            if (badge.state === "has-metadata") return "#813d9c"
            if (badge.state === "error") return "#c01c28"
            if (badge.state === "warning") return "#e5a50a"
            if (badge.state === "clean") return "#26a269"
            return "transparent"
        }

        Label {
            id: badgeLabel
            anchors.centerIn: parent
            text: {
                if (badge.state === "has-metadata") return badge.count.toString()
                if (badge.state === "clean") return "\u2713"
                if (badge.state === "error") return "\u2717"
                if (badge.state === "warning") return "!"
                return ""
            }
            color: "white"
            font.pixelSize: 12
            font.bold: true
        }
    }

    BusyIndicator {
        anchors.centerIn: parent
        width: 22
        height: 22
        running: badge.state === "working"
        visible: badge.state === "working"
    }
}
