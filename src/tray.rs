//! A notification-area (tray) icon so Sonder can keep playing with no window.
//!
//! The icon owns one thread that sleeps inside `GetMessageW`, so it costs no
//! CPU until the listener clicks it. Clicks become [`TrayCommand`]s on a
//! channel that the app handles on its own thread. Dropping the [`Tray`]
//! removes the icon and joins the thread.
use std::{
    cell::RefCell,
    io,
    sync::mpsc,
    thread::{self, JoinHandle},
};
use windows_sys::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM},
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Shell::{
                NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
                Shell_NotifyIconW,
            },
            WindowsAndMessaging::*,
        },
    },
    core::{PCWSTR, w},
};

/// What the listener asked for from the tray.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayCommand {
    Show,
    Toggle,
    Next,
    Previous,
    Quit,
}

/// The icon in the executable's resources (see `assets/windows.rc`).
const ICON_RESOURCE: usize = 1;
/// Sent by the shell for mouse activity on the icon.
const CALLBACK_MESSAGE: u32 = WM_APP + 1;
const ICON_ID: u32 = 1;
const CLASS_NAME: PCWSTR = w!("CadenceTrayWindow");
const MENU_ITEMS: [(usize, PCWSTR, TrayCommand); 4] = [
    (1, w!("Show Sonder"), TrayCommand::Show),
    (2, w!("Play / Pause"), TrayCommand::Toggle),
    (3, w!("Next"), TrayCommand::Next),
    (4, w!("Previous"), TrayCommand::Previous),
];
const QUIT_ITEM: usize = 5;

thread_local! {
    // The window procedure runs on the tray thread, which owns these.
    static COMMANDS: RefCell<Option<async_channel::Sender<TrayCommand>>> =
        const { RefCell::new(None) };
    static TASKBAR_CREATED: RefCell<u32> = const { RefCell::new(0) };
}

pub struct Tray {
    window: HWND,
    thread: Option<JoinHandle<()>>,
}

// SAFETY: the window handle is only used to post `WM_CLOSE`, which any thread
// may do.
unsafe impl Send for Tray {}

impl Tray {
    /// Adds the icon. Commands arrive on the returned receiver, which ends once
    /// the `Tray` is dropped.
    pub fn start() -> io::Result<(Self, async_channel::Receiver<TrayCommand>)> {
        let (commands, receiver) = async_channel::bounded(8);
        let (ready, ready_receiver) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("cadence-tray".into())
            .spawn(move || run(commands, ready))?;
        match ready_receiver.recv() {
            Ok(Ok(window)) => Ok((
                Self {
                    window: window as HWND,
                    thread: Some(thread),
                },
                receiver,
            )),
            Ok(Err(error)) => {
                let _ = thread.join();
                Err(error)
            }
            Err(_) => Err(io::Error::other("the tray thread stopped while starting")),
        }
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        // SAFETY: posting to a window handle is valid from any thread; if the
        // window is already gone the call just fails.
        unsafe {
            PostMessageW(self.window, WM_CLOSE, 0, 0);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run(commands: async_channel::Sender<TrayCommand>, ready: mpsc::Sender<io::Result<isize>>) {
    // SAFETY: plain Win32 calls with valid, NUL-terminated constants. The
    // window and its message loop live and die on this thread.
    unsafe {
        let instance = GetModuleHandleW(std::ptr::null());
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: CLASS_NAME,
            ..std::mem::zeroed()
        };
        RegisterClassExW(&class);
        // A hidden top-level window, not a message-only one: only top-level
        // windows are told when Explorer restarts and the icon must be re-added.
        let window = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            CLASS_NAME,
            w!("Sonder"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            std::ptr::null(),
        );
        if window.is_null() {
            let _ = ready.send(Err(io::Error::last_os_error()));
            return;
        }
        COMMANDS.with(|slot| *slot.borrow_mut() = Some(commands));
        TASKBAR_CREATED.with(|id| *id.borrow_mut() = RegisterWindowMessageW(w!("TaskbarCreated")));
        if !add_icon(window) {
            DestroyWindow(window);
            let _ = ready.send(Err(io::Error::other("could not add the tray icon")));
            return;
        }
        let _ = ready.send(Ok(window as isize));
        let mut message = std::mem::zeroed::<MSG>();
        while GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        UnregisterClassW(CLASS_NAME, instance);
    }
    COMMANDS.with(|slot| slot.borrow_mut().take());
}

unsafe fn icon_data(window: HWND) -> NOTIFYICONDATAW {
    // SAFETY: an all-zero NOTIFYICONDATAW is valid; the fields used are set.
    let mut data: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    data.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    data.hWnd = window;
    data.uID = ICON_ID;
    data
}

unsafe fn add_icon(window: HWND) -> bool {
    // SAFETY: documented Shell_NotifyIcon usage with a window we own.
    unsafe {
        let mut data = icon_data(window);
        data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        data.uCallbackMessage = CALLBACK_MESSAGE;
        data.hIcon = LoadImageW(
            GetModuleHandleW(std::ptr::null()),
            ICON_RESOURCE as PCWSTR,
            IMAGE_ICON,
            GetSystemMetrics(SM_CXSMICON),
            GetSystemMetrics(SM_CYSMICON),
            LR_SHARED,
        ) as _;
        for (slot, unit) in data.szTip.iter_mut().zip("Sonder".encode_utf16()) {
            *slot = unit;
        }
        Shell_NotifyIconW(NIM_ADD, &data) != 0
    }
}

fn send(command: TrayCommand) {
    COMMANDS.with(|slot| {
        if let Some(commands) = slot.borrow().as_ref() {
            // Full only if the app is not listening; dropping is correct then.
            let _ = commands.try_send(command);
        }
    });
}

unsafe fn show_menu(window: HWND) {
    // SAFETY: standard popup-menu sequence on the owning thread.
    unsafe {
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return;
        }
        for (id, label, _) in MENU_ITEMS {
            AppendMenuW(menu, MF_STRING, id, label);
        }
        AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
        AppendMenuW(menu, MF_STRING, QUIT_ITEM, w!("Quit Sonder"));
        let mut cursor = POINT { x: 0, y: 0 };
        GetCursorPos(&mut cursor);
        // Without this the menu would not close when the listener clicks away.
        SetForegroundWindow(window);
        let chosen = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN,
            cursor.x,
            cursor.y,
            0,
            window,
            std::ptr::null(),
        );
        PostMessageW(window, WM_NULL, 0, 0);
        DestroyMenu(menu);
        let chosen = chosen as usize;
        if chosen == QUIT_ITEM {
            send(TrayCommand::Quit);
        } else if let Some((_, _, command)) = MENU_ITEMS.iter().find(|(id, ..)| *id == chosen) {
            send(*command);
        }
    }
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: called by Windows with the arguments for this message.
    unsafe {
        match message {
            CALLBACK_MESSAGE => {
                match (lparam & 0xFFFF) as u32 {
                    WM_LBUTTONUP | WM_LBUTTONDBLCLK => send(TrayCommand::Show),
                    WM_MBUTTONUP => send(TrayCommand::Toggle),
                    WM_RBUTTONUP | WM_CONTEXTMENU => show_menu(window),
                    _ => {}
                }
                0
            }
            WM_CLOSE => {
                Shell_NotifyIconW(NIM_DELETE, &icon_data(window));
                DestroyWindow(window);
                0
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                0
            }
            _ if message != 0 && message == TASKBAR_CREATED.with(|id| *id.borrow()) => {
                // Explorer restarted and dropped every icon.
                add_icon(window);
                0
            }
            _ => DefWindowProcW(window, message, wparam, lparam),
        }
    }
}
