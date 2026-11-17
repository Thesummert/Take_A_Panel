use super::EventHandler;
use anyhow::{Context, Result};
use global_hotkey::{
    GlobalHotKeyEvent, HotKeyState,
    hotkey::{Code, HotKey, Modifiers},
};
use std::{
    cell::RefCell,
    ptr::null_mut,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::JoinHandle,
};
use windows_sys::Win32::{
    Foundation::{LPARAM, LRESULT, WPARAM},
    System::{LibraryLoader::GetModuleHandleW, Threading::GetCurrentThreadId},
    UI::{Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};

const ACTIVATE: u32 = WM_APP + 1;
// An unassigned virtual key marks the Win press as used, so swallowing Space
// doesn't leave a bare Win press that opens Start when the user releases Win.
const MENU_MASK_KEY: u16 = 0xE8;

pub(super) fn is_win_space(hotkey: HotKey) -> bool {
    hotkey.key == Code::Space && hotkey.mods == Modifiers::SUPER
}

pub(super) struct WinSpaceHook {
    thread_id: u32,
    enabled: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl WinSpaceHook {
    pub fn new(id: u32, handler: Option<EventHandler>) -> Result<Self> {
        let enabled = Arc::new(AtomicBool::new(false));
        let active = enabled.clone();
        let (send, ready) = mpsc::sync_channel(1);
        let thread = std::thread::Builder::new()
            .name("win-space-hook".into())
            .spawn(move || {
                // Keep the hook on its own message thread. The hook only updates
                // state and posts a notification; UI work happens after it returns.
                let mut message: MSG = unsafe { std::mem::zeroed() };
                unsafe {
                    PeekMessageW(&mut message, null_mut(), 0, 0, PM_NOREMOVE);
                }
                STATE.with(|state| {
                    *state.borrow_mut() = Some(HookState {
                        keys: Keys::current(),
                        enabled: active.clone(),
                    })
                });
                let hook = unsafe {
                    SetWindowsHookExW(
                        WH_KEYBOARD_LL,
                        Some(keyboard_hook),
                        GetModuleHandleW(std::ptr::null()),
                        0,
                    )
                };
                if hook.is_null() {
                    let _ = send.send(Err(std::io::Error::last_os_error()));
                    return;
                }
                let _guard = HookHandle(hook);
                if send.send(Ok(unsafe { GetCurrentThreadId() })).is_err() {
                    return;
                }
                while unsafe { GetMessageW(&mut message, null_mut(), 0, 0) } > 0 {
                    if message.message == ACTIVATE && active.load(Ordering::Acquire) {
                        if let Some(handler) = &handler {
                            handler(GlobalHotKeyEvent {
                                id,
                                state: HotKeyState::Pressed,
                            });
                        }
                    } else {
                        unsafe {
                            TranslateMessage(&message);
                            DispatchMessageW(&message);
                        }
                    }
                }
            })
            .context("无法启动 Win+Space 拦截线程")?;
        match ready.recv().context("Win+Space 拦截线程启动失败")? {
            Ok(thread_id) => Ok(Self {
                thread_id,
                enabled,
                thread: Some(thread),
            }),
            Err(error) => {
                let _ = thread.join();
                Err(error).context("无法拦截 Win+Space")
            }
        }
    }

    pub fn enable(&self) {
        self.enabled.store(true, Ordering::Release);
    }
}

impl Drop for WinSpaceHook {
    fn drop(&mut self) {
        self.enabled.store(false, Ordering::Release);
        unsafe {
            PostThreadMessageW(self.thread_id, WM_QUIT, 0, 0);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct HookHandle(HHOOK);
impl Drop for HookHandle {
    fn drop(&mut self) {
        unsafe {
            UnhookWindowsHookEx(self.0);
        }
    }
}

struct HookState {
    keys: Keys,
    enabled: Arc<AtomicBool>,
}
thread_local! {
    static STATE: RefCell<Option<HookState>> = const { RefCell::new(None) };
}

#[derive(Default)]
struct Keys {
    modifiers: u16,
    space_down: bool,
    suppress_space: bool,
}

#[derive(Debug, PartialEq, Eq)]
enum Action {
    Pass,
    Suppress,
    Activate,
}

impl Keys {
    fn current() -> Self {
        let mut keys = Self::default();
        for key in [
            VK_LWIN,
            VK_RWIN,
            VK_LCONTROL,
            VK_RCONTROL,
            VK_LMENU,
            VK_RMENU,
            VK_LSHIFT,
            VK_RSHIFT,
        ] {
            if unsafe { GetAsyncKeyState(i32::from(key)) } < 0 {
                keys.event(key, true, false);
            }
        }
        keys.space_down = unsafe { GetAsyncKeyState(i32::from(VK_SPACE)) } < 0;
        keys
    }

    fn event(&mut self, key: u16, down: bool, enabled: bool) -> Action {
        let modifier = match key {
            VK_LWIN => 1,
            VK_RWIN => 2,
            VK_LCONTROL | VK_CONTROL => 4,
            VK_RCONTROL => 8,
            VK_LMENU | VK_MENU => 16,
            VK_RMENU => 32,
            VK_LSHIFT | VK_SHIFT => 64,
            VK_RSHIFT => 128,
            _ => 0,
        };
        if down {
            self.modifiers |= modifier;
        } else {
            self.modifiers &= !modifier;
        }
        if key != VK_SPACE {
            return Action::Pass;
        }
        if !down {
            self.space_down = false;
            return if std::mem::take(&mut self.suppress_space) {
                Action::Suppress
            } else {
                Action::Pass
            };
        }
        let first = !std::mem::replace(&mut self.space_down, true);
        if self.suppress_space {
            return Action::Suppress;
        }
        if enabled && first && self.modifiers & 3 != 0 && self.modifiers & !3 == 0 {
            self.suppress_space = true;
            return Action::Activate;
        }
        Action::Pass
    }
}

unsafe extern "system" fn keyboard_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32
        && matches!(
            wparam as u32,
            WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_SYSKEYUP
        )
    {
        let event = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
        let down = matches!(wparam as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
        let action = STATE.with(|state| {
            let mut state = state.borrow_mut();
            state.as_mut().map_or(Action::Pass, |state| {
                state.keys.event(
                    event.vkCode as u16,
                    down,
                    state.enabled.load(Ordering::Acquire),
                )
            })
        });
        if action == Action::Activate {
            // Send outside the RefCell borrow: SendInput may reenter the hook.
            mask_start_menu();
            unsafe {
                PostThreadMessageW(GetCurrentThreadId(), ACTIVATE, 0, 0);
            }
        }
        if action != Action::Pass {
            return 1;
        }
    }
    unsafe { CallNextHookEx(null_mut(), code, wparam, lparam) }
}

fn mask_start_menu() {
    let input = |flags| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: MENU_MASK_KEY,
                dwFlags: flags,
                ..unsafe { std::mem::zeroed() }
            },
        },
    };
    let inputs = [input(0), input(KEYEVENTF_KEYUP)];
    unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            std::mem::size_of::<INPUT>() as i32,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consumes_one_press_and_the_entire_space_sequence() {
        for win in [VK_LWIN, VK_RWIN] {
            let mut keys = Keys::default();
            assert_eq!(keys.event(win, true, true), Action::Pass);
            assert_eq!(keys.event(VK_SPACE, true, true), Action::Activate);
            assert_eq!(keys.event(VK_SPACE, true, true), Action::Suppress);
            assert_eq!(keys.event(win, false, true), Action::Pass);
            assert_eq!(keys.event(VK_SPACE, true, true), Action::Suppress);
            assert_eq!(keys.event(VK_SPACE, false, true), Action::Suppress);
            assert_eq!(keys.event(VK_SPACE, true, true), Action::Pass);
            keys.event(VK_SPACE, false, true);
            keys.event(win, true, true);
            assert_eq!(keys.event(VK_SPACE, true, true), Action::Activate);
        }
    }

    #[test]
    fn passes_plain_space_extra_modifiers_and_disabled_binding() {
        for modifier in [
            VK_LCONTROL,
            VK_RCONTROL,
            VK_LSHIFT,
            VK_RSHIFT,
            VK_LMENU,
            VK_RMENU,
        ] {
            let mut keys = Keys::default();
            keys.event(VK_LWIN, true, true);
            keys.event(modifier, true, true);
            assert_eq!(keys.event(VK_SPACE, true, true), Action::Pass);
            assert_eq!(keys.event(VK_SPACE, false, true), Action::Pass);
            keys.event(modifier, false, true);
            assert_eq!(keys.event(VK_SPACE, true, true), Action::Activate);
        }
        let mut keys = Keys::default();
        assert_eq!(keys.event(VK_SPACE, true, true), Action::Pass);
        keys.event(VK_LWIN, true, true);
        assert_eq!(keys.event(VK_SPACE, true, true), Action::Pass);
        keys.event(VK_SPACE, false, true);
        assert_eq!(keys.event(VK_SPACE, true, false), Action::Pass);
        keys.event(VK_SPACE, false, false);
        assert_eq!(keys.event(VK_SPACE, true, true), Action::Activate);
    }
}
