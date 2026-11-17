use crate::shortcuts::{Shortcuts, parse_hotkey};
use anyhow::{Context, Result, ensure};
use std::{cell::RefCell, ptr::null_mut, sync::mpsc, thread::JoinHandle, time::Duration};
use windows_sys::Win32::{
    Foundation::{LPARAM, LRESULT, WPARAM},
    System::{LibraryLoader::GetModuleHandleW, Threading::GetCurrentThreadId},
    UI::{Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};

const TEST_INPUT: usize = 0x54415054;
const BARRIER: u16 = 0xFC;
thread_local! {
    static OBSERVED: RefCell<Option<mpsc::Sender<(u16, bool)>>> = const { RefCell::new(None) };
}

// Installed before the application hook, so this sees only events that the
// application passes on. Swallow tagged test events here to leave the user's
// focused application, input language and desktop untouched.
unsafe extern "system" fn observe(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let event = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
        if event.dwExtraInfo == TEST_INPUT {
            OBSERVED.with(|sender| {
                if let Some(sender) = sender.borrow().as_ref() {
                    let _ = sender.send((
                        event.vkCode as u16,
                        matches!(wparam as u32, WM_KEYDOWN | WM_SYSKEYDOWN),
                    ));
                }
            });
            return 1;
        }
    }
    unsafe { CallNextHookEx(null_mut(), code, wparam, lparam) }
}

struct Observer {
    id: u32,
    thread: Option<JoinHandle<()>>,
    events: mpsc::Receiver<(u16, bool)>,
}
impl Observer {
    fn new() -> Result<Self> {
        let (send, events) = mpsc::channel();
        let (ready, recv) = mpsc::sync_channel(1);
        let thread = std::thread::spawn(move || {
            let mut msg: MSG = unsafe { std::mem::zeroed() };
            unsafe {
                PeekMessageW(&mut msg, null_mut(), 0, 0, PM_NOREMOVE);
            }
            OBSERVED.with(|sender| *sender.borrow_mut() = Some(send));
            let hook = unsafe {
                SetWindowsHookExW(
                    WH_KEYBOARD_LL,
                    Some(observe),
                    GetModuleHandleW(std::ptr::null()),
                    0,
                )
            };
            if hook.is_null() {
                let _ = ready.send(Err(std::io::Error::last_os_error()));
                return;
            }
            let _ = ready.send(Ok(unsafe { GetCurrentThreadId() }));
            while unsafe { GetMessageW(&mut msg, null_mut(), 0, 0) } > 0 {}
            unsafe {
                UnhookWindowsHookEx(hook);
            }
        });
        let id = recv.recv()??;
        Ok(Self {
            id,
            thread: Some(thread),
            events,
        })
    }

    fn send(&self, keys: &[(u16, bool)]) -> Result<usize> {
        let inputs: Vec<_> = keys
            .iter()
            .copied()
            .chain([(BARRIER, true), (BARRIER, false)])
            .map(|(key, down)| INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: key,
                        dwFlags: if down { 0 } else { KEYEVENTF_KEYUP },
                        dwExtraInfo: TEST_INPUT,
                        ..unsafe { std::mem::zeroed() }
                    },
                },
            })
            .collect();
        let sent = unsafe {
            SendInput(
                inputs.len() as u32,
                inputs.as_ptr(),
                std::mem::size_of::<INPUT>() as i32,
            )
        };
        ensure!(sent as usize == inputs.len(), "SendInput failed");
        let mut spaces = 0;
        loop {
            let (key, down) = self
                .events
                .recv_timeout(Duration::from_secs(3))
                .context("keyboard hook did not reach barrier")?;
            if key == VK_SPACE {
                spaces += 1;
            }
            if key == BARRIER && !down {
                return Ok(spaces);
            }
        }
    }
}
impl Drop for Observer {
    fn drop(&mut self) {
        unsafe {
            PostThreadMessageW(self.id, WM_QUIT, 0, 0);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub fn verify() -> Result<()> {
    let observer = Observer::new()?;
    let (send, events) = mpsc::channel();
    let mut shortcuts = Shortcuts::default();
    shortcuts.set_event_handler(move |event| {
        let _ = send.send(event);
    });
    let chord = [
        (VK_LWIN, true),
        (VK_SPACE, true),
        (VK_SPACE, true),
        (VK_LWIN, false),
        (VK_SPACE, false),
    ];

    // Installing the hook for an unsaved candidate must not consume keys.
    assert!(
        shortcuts
            .apply("Win+Space", || anyhow::bail!("save failed"))
            .is_err()
    );
    assert_eq!(observer.send(&chord)?, 3);
    assert!(events.try_recv().is_err());

    shortcuts.apply("Win+Space", || Ok(()))?;
    assert_eq!(observer.send(&chord)?, 0);
    let event = events.recv_timeout(Duration::from_secs(3))?;
    assert_eq!(event.id, parse_hotkey("Win+Space")?.id());
    assert!(
        events.try_recv().is_err(),
        "key repeat must not toggle twice"
    );
    assert_eq!(observer.send(&[(VK_SPACE, true), (VK_SPACE, false)])?, 2);
    assert_eq!(
        observer.send(&[
            (VK_RWIN, true),
            (VK_SPACE, true),
            (VK_SPACE, false),
            (VK_RWIN, false)
        ])?,
        0
    );
    events.recv_timeout(Duration::from_secs(3))?;

    // A failed change preserves the live interception; successful switching
    // and destruction must restore passthrough to the system.
    assert!(
        shortcuts
            .apply("Ctrl+Alt+Shift+Win+F13", || anyhow::bail!("save failed"))
            .is_err()
    );
    assert_eq!(observer.send(&chord)?, 0);
    events.recv_timeout(Duration::from_secs(3))?;
    shortcuts.apply("Ctrl+Alt+Shift+Win+F13", || Ok(()))?;
    assert_eq!(observer.send(&chord)?, 3);
    shortcuts.apply("Super+Space", || Ok(()))?;
    drop(shortcuts);
    assert_eq!(observer.send(&chord)?, 3);
    assert!(events.try_recv().is_err());
    println!(
        "Win+Space interception, repeat suppression, rollback, switching and shutdown: passed"
    );
    Ok(())
}
