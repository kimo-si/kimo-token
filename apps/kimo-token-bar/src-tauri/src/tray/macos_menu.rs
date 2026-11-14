#![allow(deprecated, unexpected_cfgs)]

use std::cell::RefCell;

use tauri::{tray::TrayIcon, Runtime};
use tauri_nspanel::cocoa::appkit::{NSViewHeightSizable, NSViewWidthSizable};
use tauri_nspanel::cocoa::base::{id, nil};
use tauri_nspanel::cocoa::foundation::{NSPoint, NSRect};
use tauri_nspanel::objc::rc::StrongPtr;
use tauri_nspanel::objc::runtime::Class;
use tauri_nspanel::objc::{msg_send, sel, sel_impl};

thread_local! {
    static MENU: RefCell<Option<StrongPtr>> = const { RefCell::new(None) };
}

// 保留已构建的菜单，并解除 NSStatusItem 的自动弹出行为。
pub(super) fn detach<R: Runtime>(tray: &TrayIcon<R>) -> tauri::Result<()> {
    tray.with_inner_tray_icon(|inner| {
        let Some(item) = inner.ns_status_item() else {
            return;
        };
        inner.set_show_menu_on_right_click(false);
        let item = &*item as *const _ as id;
        unsafe {
            let menu: id = msg_send![item, menu];
            let retained = (!menu.is_null()).then(|| StrongPtr::retain(menu));
            let _: () = msg_send![item, setMenu: nil];
            let _: () = msg_send![menu, setDelegate: nil];
            MENU.with(|slot| slot.replace(retained));
            let button: id = msg_send![item, button];
            resize_hit_view(button);
            let attached: id = msg_send![item, menu];
            crate::logging::info(&format!(
                "tray: explicit right-click menu active, native menu attached={}",
                !attached.is_null()
            ));
        }
    })
}

// 在真实右键事件中，把缓存的菜单显示在菜单栏图标下方。
pub(super) fn popup<R: Runtime>(tray: &TrayIcon<R>) {
    let _ = tray.with_inner_tray_icon(|inner| {
        let Some(item) = inner.ns_status_item() else {
            return;
        };
        let Some(menu) = MENU.with(|slot| slot.borrow().clone()) else {
            return;
        };
        let item = &*item as *const _ as id;
        unsafe {
            let button: id = msg_send![item, button];
            let bounds: NSRect = msg_send![button, bounds];
            let flipped: bool = msg_send![button, isFlipped];
            let y = bounds.origin.y + if flipped { bounds.size.height } else { 0.0 };
            let point = NSPoint::new(bounds.origin.x, y);
            crate::logging::info("tray: right click opening options");
            let _: bool =
                msg_send![*menu, popUpMenuPositioningItem: nil atLocation: point inView: button];
        }
    });
}

// 状态栏文字改变后，同步 Tauri 子视图的点击范围。
pub(super) fn sync_hit_area<R: Runtime>(tray: &TrayIcon<R>) {
    let _ = tray.with_inner_tray_icon(|inner| {
        let Some(item) = inner.ns_status_item() else {
            return;
        };
        let item = &*item as *const _ as id;
        unsafe {
            let button: id = msg_send![item, button];
            resize_hit_view(button);
        }
    });
}

// 子视图使用按钮的局部边界，避免把按钮在父视图中的偏移带入点击区。
unsafe fn resize_hit_view(button: id) {
    let Some(target_class) = Class::get("TaoTrayTarget") else {
        return;
    };
    let bounds: NSRect = msg_send![button, bounds];
    let views: id = msg_send![button, subviews];
    let count: usize = msg_send![views, count];
    for index in 0..count {
        let view: id = msg_send![views, objectAtIndex: index];
        let is_target: bool = msg_send![view, isKindOfClass: target_class];
        if is_target {
            let mask = NSViewWidthSizable | NSViewHeightSizable;
            let _: () = msg_send![view, setAutoresizingMask: mask];
            let _: () = msg_send![view, setFrame: bounds];
            return;
        }
    }
}
