//! Windows taskbar integration: transfer progress and attention hints.
//!
//! The state is kept in a thread local because the COM interface and the
//! `HWND` are both tied to the UI thread. All calls are best effort: when COM
//! or the shell is unavailable the application keeps working without the
//! overlay.

#[cfg(not(target_os = "windows"))]
mod platform {
    use gpui_kit::Window;

    pub fn init(_window: &Window) {}

    pub fn set_progress(_transferred: u64, _total: u64) {}

    pub fn clear() {}

    /// Attention hints (sound + taskbar badge) are Windows-only.
    pub fn notify() {}

    pub fn clear_notification() {}
}

#[cfg(target_os = "windows")]
mod platform {
    use gpui_kit::Window;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use std::cell::{Cell, RefCell};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    };
    use windows::Win32::System::Diagnostics::Debug::MessageBeep;
    use windows::Win32::UI::Shell::{
        ITaskbarList3, TBPF_INDETERMINATE, TBPF_NOPROGRESS, TBPF_NORMAL, TaskbarList,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        HICON, IDI_ERROR, KillTimer, LoadIconW, MB_ICONEXCLAMATION, SetTimer,
    };
    use windows::core::{HSTRING, PCWSTR};

    struct TaskbarState {
        list: ITaskbarList3,
    }

    // The state is only ever touched from the UI thread.
    thread_local! {
        static STATE: RefCell<Option<TaskbarState>> = const { RefCell::new(None) };
        static WINDOW: Cell<Option<HWND>> = const { Cell::new(None) };
        static BLINK_TIMER: Cell<usize> = const { Cell::new(0) };
        static BLINK_VISIBLE: Cell<bool> = const { Cell::new(false) };
    }

    /// Interval of the blinking warning badge on the taskbar button.
    const BLINK_INTERVAL_MS: u32 = 600;

    fn hwnd() -> Option<HWND> {
        WINDOW.with(Cell::get)
    }

    /// Binds the taskbar list to the application window. Safe to call again;
    /// the previous binding is replaced.
    pub fn init(window: &Window) {
        let Ok(handle) = HasWindowHandle::window_handle(window) else {
            return;
        };
        let RawWindowHandle::Win32(win32) = handle.as_raw() else {
            return;
        };
        let hwnd = HWND(win32.hwnd.get() as *mut _);
        WINDOW.with(|cell| cell.set(Some(hwnd)));

        unsafe {
            // Ignore RPC_E_CHANGED_MODE: if COM already lives in another
            // apartment the shell object is still creatable in-process.
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let Ok(list): windows::core::Result<ITaskbarList3> = CoCreateInstance(
                &TaskbarList,
                None::<&windows::core::IUnknown>,
                CLSCTX_INPROC_SERVER,
            ) else {
                return;
            };
            if list.HrInit().is_err() {
                return;
            }
            STATE.with(|state| {
                *state.borrow_mut() = Some(TaskbarState { list });
            });
        }
    }

    /// Shows a determinate bar when `total` is known, otherwise a marquee.
    pub fn set_progress(transferred: u64, total: u64) {
        let Some(hwnd) = hwnd() else {
            return;
        };
        STATE.with(|state| {
            let state = state.borrow();
            let Some(state) = state.as_ref() else {
                return;
            };
            unsafe {
                if total == 0 {
                    let _ = state.list.SetProgressState(hwnd, TBPF_INDETERMINATE);
                } else {
                    let _ = state.list.SetProgressState(hwnd, TBPF_NORMAL);
                    let _ = state
                        .list
                        .SetProgressValue(hwnd, transferred.min(total), total);
                }
            }
        });
    }

    /// Removes the overlay from the taskbar button.
    pub fn clear() {
        let Some(hwnd) = hwnd() else {
            return;
        };
        STATE.with(|state| {
            let state = state.borrow();
            let Some(state) = state.as_ref() else {
                return;
            };
            unsafe {
                let _ = state.list.SetProgressState(hwnd, TBPF_NOPROGRESS);
            }
        });
    }

    /// Toggles the red warning badge between visible and hidden.
    fn set_badge_visible(visible: bool) {
        BLINK_VISIBLE.with(|flag| flag.set(visible));
        let Some(hwnd) = hwnd() else {
            return;
        };
        STATE.with(|state| {
            let state = state.borrow();
            let Some(state) = state.as_ref() else {
                return;
            };
            unsafe {
                if visible {
                    // `IDI_ERROR` is the red circle system icon, which reads
                    // as an alert even at taskbar size.
                    if let Ok(icon) = LoadIconW(None, IDI_ERROR) {
                        let description = HSTRING::from("文件已存在，需要确认");
                        let _ = state.list.SetOverlayIcon(hwnd, icon, &description);
                    }
                } else {
                    let _ = state
                        .list
                        .SetOverlayIcon(hwnd, HICON::default(), PCWSTR::null());
                }
            }
        });
    }

    unsafe extern "system" fn blink_timer(_hwnd: HWND, _msg: u32, _id: usize, _time: u32) {
        let visible = !BLINK_VISIBLE.with(Cell::get);
        set_badge_visible(visible);
    }

    fn stop_blink() {
        let timer = BLINK_TIMER.with(|slot| slot.replace(0));
        if timer != 0 {
            unsafe {
                let _ = KillTimer(None, timer);
            }
        }
    }

    /// Asks for attention when a modal prompt needs an answer: plays the
    /// system notification sound and blinks a red warning badge on the
    /// taskbar button until the prompt is answered.
    pub fn notify() {
        // The sound does not depend on the taskbar integration, so play it
        // even when COM or the shell is unavailable.
        unsafe {
            let _ = MessageBeep(MB_ICONEXCLAMATION);
        }

        stop_blink();
        set_badge_visible(true);

        let timer = unsafe { SetTimer(None, 0, BLINK_INTERVAL_MS, Some(blink_timer)) };
        BLINK_TIMER.with(|slot| slot.set(timer));
    }

    /// Stops blinking and removes the warning badge from the taskbar button.
    pub fn clear_notification() {
        stop_blink();
        set_badge_visible(false);
    }
}

pub use platform::{clear, clear_notification, init, notify, set_progress};
