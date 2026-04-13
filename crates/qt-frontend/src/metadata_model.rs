use traceless_core::MetadataItem;

#[cxx_qt::bridge]
mod ffi {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qproperty(i32, count)]
        #[qproperty(QString, group_name)]
        type MetadataModel = super::MetadataModelRust;
    }

    unsafe extern "RustQt" {
        #[qinvokable]
        fn get_key(self: &MetadataModel, index: i32) -> QString;

        #[qinvokable]
        fn get_value(self: &MetadataModel, index: i32) -> QString;

        #[qinvokable]
        fn clear(self: Pin<&mut MetadataModel>);
    }
}

use cxx_qt::CxxQtType;
use cxx_qt_lib::QString;
use std::pin::Pin;

#[derive(Default)]
pub struct MetadataModelRust {
    items: Vec<MetadataItem>,
    count: i32,
    group_name: QString,
}

impl ffi::MetadataModel {
    fn get_key(&self, index: i32) -> QString {
        usize::try_from(index)
            .ok()
            .and_then(|i| self.rust().items.get(i))
            .map(|item| QString::from(&item.key as &str))
            .unwrap_or_default()
    }

    fn get_value(&self, index: i32) -> QString {
        usize::try_from(index)
            .ok()
            .and_then(|i| self.rust().items.get(i))
            .map(|item| QString::from(&item.value as &str))
            .unwrap_or_default()
    }

    fn clear(mut self: Pin<&mut Self>) {
        self.as_mut().rust_mut().items.clear();
        self.as_mut().rust_mut().count = 0;
        self.as_mut().rust_mut().group_name = QString::default();
    }
}
