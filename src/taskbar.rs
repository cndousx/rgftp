//! Windows taskbar progress overlay built on `ITaskbarList3`.
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
}

#[cfg(target_os = "windows")]
mod platform {
    use gpui_kit::Window;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use std::cell::RefCell;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    };
    use windows::Win32::UI::Shell::{
        ITaskbarList3, TBPF_INDETERMINATE, TBPF_NOPROGRESS, TBPF_NORMAL, TaskbarList,
    };

    struct TaskbarState {
        list: ITaskbarList3,
        hwnd: HWND,
    }

    // The state is only ever touched from the UI thread.
    thread_local! {
        static STATE: RefCell<Option<TaskbarState>> = const { RefCell::new(None) };
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
                *state.borrow_mut() = Some(TaskbarState { list, hwnd });
            });
        }
    }

    /// Shows a determinate bar when `total` is known, otherwise a marquee.
    pub fn set_progress(transferred: u64, total: u64) {
        STATE.with(|state| {
            let mut state = state.borrow_mut();
            let Some(state) = state.as_mut() else {
                return;
            };
            unsafe {
                if total == 0 {
                    let _ = state.list.SetProgressState(state.hwnd, TBPF_INDETERMINATE);
                } else {
                    let _ = state.list.SetProgressState(state.hwnd, TBPF_NORMAL);
                    let _ = state
                        .list
                        .SetProgressValue(state.hwnd, transferred.min(total), total);
                }
            }
        });
    }

    /// Removes the overlay from the taskbar button.
    pub fn clear() {
        STATE.with(|state| {
            let mut state = state.borrow_mut();
            let Some(state) = state.as_mut() else {
                return;
            };
            unsafe {
                let _ = state.list.SetProgressState(state.hwnd, TBPF_NOPROGRESS);
            }
        });
    }
}

pub use platform::{clear, init, set_progress};
