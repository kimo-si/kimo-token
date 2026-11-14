use std::cell::Cell;

use tauri::{AppHandle, Manager, WebviewWindow};
use tauri_nspanel::block::ConcreteBlock;
use tauri_nspanel::cocoa::base::{id, nil};
use tauri_nspanel::cocoa::foundation::{NSPoint, NSRect};
use tauri_nspanel::objc::{class, msg_send, sel, sel_impl};

// NSEventMaskLeftMouseDown | NSEventMaskRightMouseDown | NSEventMaskOtherMouseDown
const MOUSE_DOWN_MASK: u64 = (1 << 1) | (1 << 3) | (1 << 25);

thread_local! {
    static MONITOR: Cell<id> = const { Cell::new(nil) };
}

// 面板显示时在 AppKit 主线程监听其他应用收到的鼠标按下。
pub(super) fn start(app: &AppHandle) {
    MONITOR.with(|slot| {
        if !slot.get().is_null() {
            return;
        }
        let app = app.clone();
        let handler = ConcreteBlock::new(move |_event: id| dismiss_for_click(&app)).copy();
        let monitor: id = unsafe {
            msg_send![class!(NSEvent), addGlobalMonitorForEventsMatchingMask: MOUSE_DOWN_MASK
                handler: &*handler]
        };
        slot.set(monitor);
    });
}

// 隐藏后移除监听，下一次打开面板时重新注册。
pub(super) fn stop() {
    let monitor = MONITOR.with(|slot| slot.replace(nil));
    if !monitor.is_null() {
        unsafe {
            let _: () = msg_send![class!(NSEvent), removeMonitor: monitor];
        }
    }
}

// 真实的外部点击直接收起面板，失焦保护只用于系统焦点通知。
fn dismiss_for_click(app: &AppHandle) {
    if super::is_panel_hidden() || super::cursor_is_over_tray(app) {
        return;
    }
    let Some(panel) = app.get_webview_window(super::LABEL) else {
        return;
    };
    if click_is_outside(&panel) {
        crate::logging::info("panel: external mouse press — dismissing");
        super::hide_window_on_ui(&panel);
    }
}

// 用同一套 AppKit 屏幕坐标比较鼠标和面板，避免混用多屏缩放比例。
fn click_is_outside(panel: &WebviewWindow) -> bool {
    let Ok(native) = panel.ns_window() else {
        return false;
    };
    unsafe {
        let frame: NSRect = msg_send![native as id, frame];
        let cursor: NSPoint = msg_send![class!(NSEvent), mouseLocation];
        !frame_contains_point(frame, cursor)
    }
}

// 窗口边缘包含在左、下边界，右、上边界之外视为面板外。
fn frame_contains_point(frame: NSRect, point: NSPoint) -> bool {
    (frame.origin.x..frame.origin.x + frame.size.width).contains(&point.x)
        && (frame.origin.y..frame.origin.y + frame.size.height).contains(&point.y)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tauri_nspanel::cocoa::foundation::NSSize;

    // 辅助屏可在主屏左侧，面板外点击仍要按原始屏幕坐标判断。
    #[test]
    fn frame_boundary_matches_clicks_on_a_secondary_screen() {
        let frame = NSRect::new(NSPoint::new(-720.0, 120.0), NSSize::new(360.0, 760.0));
        assert!(frame_contains_point(frame, NSPoint::new(-720.0, 120.0)));
        assert!(frame_contains_point(frame, NSPoint::new(-400.0, 600.0)));
        assert!(!frame_contains_point(frame, NSPoint::new(-360.0, 600.0)));
        assert!(!frame_contains_point(frame, NSPoint::new(-400.0, 880.0)));
        assert!(!frame_contains_point(frame, NSPoint::new(100.0, 600.0)));
    }
}
