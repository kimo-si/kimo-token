use std::ffi::CStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tauri::{PhysicalPosition, Rect, WebviewWindow};
use tauri_nspanel::block::ConcreteBlock;
use tauri_nspanel::cocoa::base::{id, BOOL, NO, YES};
use tauri_nspanel::cocoa::foundation::{NSPoint, NSRect, NSSize};
use tauri_nspanel::objc::{class, msg_send, sel, sel_impl};

const OPEN_DURATION: Duration = Duration::from_millis(360);
const CLOSE_DURATION: Duration = Duration::from_millis(220);
const FADE_IN_DURATION: Duration = Duration::from_millis(150);
const COMPACT_SCALE: f64 = 0.18;
const OPEN_STIFFNESS: f64 = 600.0;
const OPEN_DAMPING: f64 = 41.0;
const ANIMATION_KEY: &CStr = c"kimo-token-tray-motion";
static REVISION: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy)]
enum Motion {
    Expand,
    Collapse(u64),
}

// 在主线程立即设置面板位置，避免原生窗口先在旧位置绘制一帧。
pub(super) fn place(window: &WebviewWindow, position: PhysicalPosition<f64>) -> bool {
    let (Ok(native), Ok(scale)) = (window.ns_window(), window.scale_factor()) else {
        return false;
    };
    let logical = position.to_logical::<f64>(scale);
    unsafe {
        let screens: id = msg_send![class!(NSScreen), screens];
        let primary: id = msg_send![screens, firstObject];
        if primary.is_null() {
            return false;
        }
        let frame: NSRect = msg_send![primary, frame];
        let top_left = NSPoint::new(logical.x, frame.origin.y + frame.size.height - logical.y);
        let native = native as id;
        let _: () = msg_send![native, setFrameTopLeftPoint: top_left];
        // 小屏上收短窗口，让固定的底部操作栏保持可见。
        fit_panel_height(native, primary);
    }
    true
}

// 将窗口高度限制在当前屏幕可用区域内，并保留贴近图标的上沿。
unsafe fn fit_panel_height(native: id, primary: id) {
    let screen: id = msg_send![native, screen];
    let screen = if screen.is_null() { primary } else { screen };
    let available: NSRect = msg_send![screen, visibleFrame];
    let frame: NSRect = msg_send![native, frame];
    let top = (frame.origin.y + frame.size.height).min(available.origin.y + available.size.height);
    let height = super::HEIGHT.min((top - available.origin.y - super::GAP).max(1.0));
    if (frame.size.height - height).abs() > 0.5 {
        let _: () = msg_send![native, setContentSize: NSSize::new(frame.size.width, height)];
        let _: () = msg_send![native, setFrameTopLeftPoint: NSPoint::new(frame.origin.x, top)];
    }
}

// 在窗口显示前挂载从图标下方展开的动画。
pub(super) fn prepare_show(window: &WebviewWindow, tray: Option<&Rect>) {
    animate(window, tray, Motion::Expand);
}

// 启动收拢动画，结束时仅隐藏仍属于这次关闭请求的面板。
pub(super) fn hide(window: &WebviewWindow, tray: Option<&Rect>) {
    let revision = REVISION.fetch_add(1, Ordering::AcqRel) + 1;
    if !animate(window, tray, Motion::Collapse(revision)) {
        super::hide_window_on_ui(window);
    }
}

// 使旧回调失效，并恢复面板合成层的原始显示。
pub(super) fn cancel(window: &WebviewWindow) {
    REVISION.fetch_add(1, Ordering::AcqRel);
    if let Some(layer) = content_layer(window) {
        unsafe {
            let _: () = msg_send![layer, removeAnimationForKey: text(ANIMATION_KEY)];
        }
    }
}

// 对整个内容合成层做缩放、位移和透明度变化，保持 WebView 的布局尺寸。
fn animate(window: &WebviewWindow, tray: Option<&Rect>, motion: Motion) -> bool {
    unsafe {
        let workspace: id = msg_send![class!(NSWorkspace), sharedWorkspace];
        let reduce_motion: BOOL = msg_send![workspace, accessibilityDisplayShouldReduceMotion];
        let Some(layer) = content_layer(window).filter(|_| reduce_motion == NO) else {
            return false;
        };
        let position: NSPoint = msg_send![layer, position];
        let target = tray_anchor(window, tray, layer);
        let (animations, duration) = match motion {
            Motion::Expand => (expand_animations(position, target), OPEN_DURATION),
            Motion::Collapse(_) => (collapse_animations(position, target), CLOSE_DURATION),
        };
        // 统一提交三个属性，收拢时由 Core Animation 回调隐藏窗口。
        apply_animation(window, layer, &animations, duration, motion);
        true
    }
}

// 读取原生面板的内容层，普通窗口也可沿用此路径。
fn content_layer(window: &WebviewWindow) -> Option<id> {
    let native = window.ns_window().ok()? as id;
    unsafe {
        let content: id = msg_send![native, contentView];
        let layer: id = msg_send![content, layer];
        (!layer.is_null()).then_some(layer)
    }
}

// 将菜单栏图标中心换算为合成层坐标，并以面板上沿作为收拢目标。
unsafe fn tray_anchor(window: &WebviewWindow, tray: Option<&Rect>, layer: id) -> NSPoint {
    let frame: NSRect = msg_send![layer, frame];
    let parent: id = msg_send![layer, superlayer];
    let flipped: BOOL = msg_send![parent, isGeometryFlipped];
    let offset = tray.and_then(|rect| {
        let (width, height) = super::rect_size(rect);
        if !super::usable_tray_size(width, height) {
            return None;
        }
        let (x, _) = super::rect_origin(rect);
        let origin = window.outer_position().ok()?;
        let scale = window.scale_factor().ok()?;
        Some((x + width / 2.0 - f64::from(origin.x)) / scale)
    });
    NSPoint::new(
        frame.origin.x
            + offset
                .unwrap_or(frame.size.width / 2.0)
                .clamp(0.0, frame.size.width),
        frame.origin.y
            + if flipped == YES {
                0.0
            } else {
                frame.size.height
            },
    )
}

// 计算缩放后的中心，让视觉上的收拢目标在动画期间保持固定。
fn scaled_position(position: NSPoint, target: NSPoint, scale: f64) -> NSPoint {
    let travel = 1.0 - scale;
    NSPoint::new(
        position.x + (target.x - position.x) * travel,
        position.y + (target.y - position.y) * travel,
    )
}

// 位置和缩放使用同一条弹簧曲线，在图标处起步并轻微回弹。
unsafe fn expand_animations(position: NSPoint, target: NSPoint) -> [id; 3] {
    let origin = scaled_position(position, target, COMPACT_SCALE);
    let fade = basic_animation(c"opacity", number(0.0), number(1.0), FADE_IN_DURATION);
    let _: () = msg_send![fade, setTimingFunction: timing_function(c"easeOut")];
    [
        spring_animation(c"position", point(origin), point(position)),
        spring_animation(c"transform.scale", number(COMPACT_SCALE), number(1.0)),
        fade,
    ]
}

// 关闭时沿相同锚点缩小并淡出。
unsafe fn collapse_animations(position: NSPoint, target: NSPoint) -> [id; 3] {
    let destination = scaled_position(position, target, COMPACT_SCALE);
    [
        basic_animation(
            c"position",
            point(position),
            point(destination),
            CLOSE_DURATION,
        ),
        basic_animation(
            c"transform.scale",
            number(1.0),
            number(COMPACT_SCALE),
            CLOSE_DURATION,
        ),
        basic_animation(c"opacity", number(1.0), number(0.0), CLOSE_DURATION),
    ]
}

// 绑定持续缓动的弹簧参数，避免分段关键帧之间停顿。
unsafe fn spring_animation(key: &CStr, from: id, to: id) -> id {
    let animation: id = msg_send![class!(CASpringAnimation), animationWithKeyPath: text(key)];
    let _: () = msg_send![animation, setFromValue: from];
    let _: () = msg_send![animation, setToValue: to];
    let _: () = msg_send![animation, setStiffness: OPEN_STIFFNESS];
    let _: () = msg_send![animation, setDamping: OPEN_DAMPING];
    let _: () = msg_send![animation, setDuration: OPEN_DURATION.as_secs_f64()];
    animation
}

// 绑定单个属性的起止值与持续时间。
unsafe fn basic_animation(key: &CStr, from: id, to: id, duration: Duration) -> id {
    let animation: id = msg_send![class!(CABasicAnimation), animationWithKeyPath: text(key)];
    let _: () = msg_send![animation, setFromValue: from];
    let _: () = msg_send![animation, setToValue: to];
    let _: () = msg_send![animation, setDuration: duration.as_secs_f64()];
    animation
}

// 三个属性共用时间曲线，收拢的末帧保留到窗口隐藏。
unsafe fn apply_animation(
    window: &WebviewWindow,
    layer: id,
    animations: &[id],
    duration: Duration,
    motion: Motion,
) {
    let group: id = msg_send![class!(CAAnimationGroup), animation];
    let curve = match motion {
        Motion::Expand => c"linear",
        Motion::Collapse(_) => c"easeInEaseOut",
    };
    let _: () = msg_send![group, setTimingFunction: timing_function(curve)];
    let _: () = msg_send![group, setDuration: duration.as_secs_f64()];
    let transaction = class!(CATransaction);
    let _: () = msg_send![transaction, begin];
    if let Motion::Collapse(revision) = motion {
        // 关闭完成前保持最后一帧，避免还原尺寸后才隐藏。
        for animation in animations {
            let _: () = msg_send![*animation, setFillMode: text(c"forwards")];
            let _: () = msg_send![*animation, setRemovedOnCompletion: NO];
        }
        let _: () = msg_send![group, setFillMode: text(c"forwards")];
        let _: () = msg_send![group, setRemovedOnCompletion: NO];
        let surface = window.clone();
        let completion = ConcreteBlock::new(move || finish(&surface, revision)).copy();
        let _: () = msg_send![transaction, setCompletionBlock: &*completion as *const _ as id];
    }
    let _: () = msg_send![group, setAnimations: array(animations)];
    let _: () = msg_send![layer, addAnimation: group forKey: text(ANIMATION_KEY)];
    let _: () = msg_send![transaction, commit];
}

// 将 Rust 切片传给 Core Animation 的对象数组。
unsafe fn array(values: &[id]) -> id {
    msg_send![class!(NSArray), arrayWithObjects: values.as_ptr() count: values.len()]
}

// 将几何点包装成可用于动画的值。
unsafe fn point(value: NSPoint) -> id {
    msg_send![class!(NSValue), valueWithPoint: value]
}

// 将比例和时间点包装成 Core Animation 数值。
unsafe fn number(value: f64) -> id {
    msg_send![class!(NSNumber), numberWithDouble: value]
}

// 取出 Core Animation 内置的缓动曲线。
unsafe fn timing_function(name: &CStr) -> id {
    msg_send![class!(CAMediaTimingFunction), functionWithName: text(name)]
}

// 只结束仍有效的关闭请求，避免旧动画关掉已经重新打开的面板。
fn finish(window: &WebviewWindow, revision: u64) {
    let surface = window.clone();
    let _ = window.run_on_main_thread(move || {
        if REVISION.load(Ordering::Acquire) == revision {
            super::hide_window_on_ui(&surface);
        }
    });
}

// 创建由 AppKit 当前自动释放池管理的字符串。
unsafe fn text(value: &CStr) -> id {
    msg_send![class!(NSString), stringWithUTF8String: value.as_ptr()]
}

#[cfg(test)]
mod tests {
    use super::*;

    // 验证面板在屏幕右缘时也能向图标位置收拢。
    #[test]
    fn scaling_keeps_the_off_center_tray_anchor_fixed() {
        let center = NSPoint::new(200.0, 330.0);
        let anchor = NSPoint::new(380.0, 660.0);
        let destination = scaled_position(center, anchor, COMPACT_SCALE);
        let scaled_x = destination.x + (anchor.x - center.x) * COMPACT_SCALE;
        let scaled_y = destination.y + (anchor.y - center.y) * COMPACT_SCALE;
        assert!((scaled_x - anchor.x).abs() < 0.001);
        assert!((scaled_y - anchor.y).abs() < 0.001);
    }
}
