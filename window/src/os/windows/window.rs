use super::gdi::*;
use super::*;
use crate::bitmaps::*;
use crate::color::Color;
use crate::{
    Dimensions, KeyCode, KeyEvent, Modifiers, MouseButtons, MouseCursor, MouseEvent,
    MouseEventKind, MousePress, Operator, PaintContext, WindowCallbacks, WindowOps, WindowOpsMut,
};
use failure::Fallible;
use promise::Future;
use std::cell::RefCell;
use std::io::Error as IoError;
use std::ptr::{null, null_mut};
use std::rc::Rc;
use winapi::shared::minwindef::*;
use winapi::shared::windef::*;
use winapi::um::libloaderapi::GetModuleHandleW;
use winapi::um::wingdi::*;
use winapi::um::winuser::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct HWindow(HWND);
unsafe impl Send for HWindow {}
unsafe impl Sync for HWindow {}

pub(crate) struct WindowInner {
    /// Non-owning reference to the window handle
    hwnd: HWindow,
    callbacks: Box<WindowCallbacks>,
}

#[derive(Debug, Clone)]
pub struct Window(HWindow);

fn rect_width(r: &RECT) -> i32 {
    r.right - r.left
}

fn rect_height(r: &RECT) -> i32 {
    r.bottom - r.top
}

fn adjust_client_to_window_dimensions(width: usize, height: usize) -> (i32, i32) {
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: width as _,
        bottom: height as _,
    };
    unsafe { AdjustWindowRect(&mut rect, WS_POPUP | WS_SYSMENU | WS_CAPTION, 0) };

    (rect_width(&rect), rect_height(&rect))
}

fn rc_to_pointer(arc: &Rc<RefCell<WindowInner>>) -> *const RefCell<WindowInner> {
    let cloned = Rc::clone(arc);
    Rc::into_raw(cloned)
}

fn rc_from_pointer(lparam: LPVOID) -> Rc<RefCell<WindowInner>> {
    // Turn it into an Rc
    let arc = unsafe { Rc::from_raw(std::mem::transmute(lparam)) };
    // Add a ref for the caller
    let cloned = Rc::clone(&arc);

    // We must not drop this ref though; turn it back into a raw pointer!
    Rc::into_raw(arc);

    cloned
}

fn rc_from_hwnd(hwnd: HWND) -> Option<Rc<RefCell<WindowInner>>> {
    let raw = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as LPVOID };
    if raw.is_null() {
        None
    } else {
        Some(rc_from_pointer(raw))
    }
}

fn take_rc_from_pointer(lparam: LPVOID) -> Rc<RefCell<WindowInner>> {
    unsafe { Rc::from_raw(std::mem::transmute(lparam)) }
}

impl Window {
    fn from_hwnd(hwnd: HWND) -> Self {
        Self(HWindow(hwnd))
    }

    fn create_window(
        class_name: &str,
        name: &str,
        width: usize,
        height: usize,
        lparam: *const RefCell<WindowInner>,
    ) -> Fallible<HWND> {
        // Jamming this in here; it should really live in the application manifest,
        // but having it here means that we don't have to create a manifest
        unsafe {
            SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        }

        let class_name = wide_string(class_name);
        let class = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW | CS_OWNDC | CS_DBLCLKS,
            lpfnWndProc: Some(wnd_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: unsafe { GetModuleHandleW(null()) },
            hIcon: null_mut(),
            hCursor: null_mut(),
            hbrBackground: null_mut(),
            lpszMenuName: null(),
            lpszClassName: class_name.as_ptr(),
        };

        if unsafe { RegisterClassW(&class) } == 0 {
            let err = IoError::last_os_error();
            match err.raw_os_error() {
                Some(code)
                    if code == winapi::shared::winerror::ERROR_CLASS_ALREADY_EXISTS as i32 => {}
                _ => return Err(err.into()),
            }
        }

        let (width, height) = adjust_client_to_window_dimensions(width, height);

        let name = wide_string(name);
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                class_name.as_ptr(),
                name.as_ptr(),
                WS_OVERLAPPEDWINDOW,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                width,
                height,
                null_mut(),
                null_mut(),
                null_mut(),
                std::mem::transmute(lparam),
            )
        };

        if hwnd.is_null() {
            let err = IoError::last_os_error();
            failure::bail!("CreateWindowExW: {}", err);
        }

        Ok(hwnd)
    }

    pub fn new_window(
        class_name: &str,
        name: &str,
        width: usize,
        height: usize,
        callbacks: Box<WindowCallbacks>,
    ) -> Fallible<Window> {
        let inner = Rc::new(RefCell::new(WindowInner {
            hwnd: HWindow(null_mut()),
            callbacks,
        }));

        // Careful: `raw` owns a ref to inner, but there is no Drop impl
        let raw = rc_to_pointer(&inner);

        let hwnd = match Self::create_window(class_name, name, width, height, raw) {
            Ok(hwnd) => HWindow(hwnd),
            Err(err) => {
                // Ensure that we drop the extra ref to raw before we return
                drop(unsafe { Rc::from_raw(raw) });
                return Err(err);
            }
        };

        enable_dark_mode(hwnd.0);

        Connection::get()
            .expect("Connection::init was not called")
            .windows
            .lock()
            .unwrap()
            .insert(hwnd.clone(), Rc::clone(&inner));

        let window = Window(hwnd);
        inner.borrow_mut().callbacks.created(&window);

        Ok(window)
    }
}

fn schedule_show_window(hwnd: HWindow, show: bool) {
    // ShowWindow can call to the window proc and may attempt
    // to lock inner, so we avoid locking it ourselves here
    Future::with_executor(Connection::executor(), move || {
        unsafe {
            ShowWindow(hwnd.0, if show { SW_NORMAL } else { SW_HIDE });
        }
        Ok(())
    });
}

impl WindowOpsMut for WindowInner {
    fn show(&mut self) {
        schedule_show_window(self.hwnd, true);
    }

    fn hide(&mut self) {
        schedule_show_window(self.hwnd, false);
    }

    fn set_cursor(&mut self, cursor: Option<MouseCursor>) {
        apply_mouse_cursor(cursor);
    }

    fn invalidate(&mut self) {
        unsafe {
            InvalidateRect(self.hwnd.0, null(), 1);
        }
    }

    fn set_title(&mut self, title: &str) {
        let title = wide_string(title);
        unsafe {
            SetWindowTextW(self.hwnd.0, title.as_ptr());
        }
    }
}

impl WindowOps for Window {
    fn show(&self) {
        schedule_show_window(self.0, true);
    }

    fn hide(&self) {
        schedule_show_window(self.0, false);
    }

    fn set_cursor(&self, cursor: Option<MouseCursor>) {
        Connection::with_window_inner(self.0, move |inner| inner.set_cursor(cursor));
    }
    fn invalidate(&self) {
        Connection::with_window_inner(self.0, |inner| inner.invalidate());
    }
    fn set_title(&self, title: &str) {
        let title = title.to_owned();
        Connection::with_window_inner(self.0, move |inner| inner.set_title(&title));
    }
}

/// Set up bidirectional pointers:
/// hwnd.USERDATA -> WindowInner
/// WindowInner.hwnd -> hwnd
unsafe fn wm_nccreate(hwnd: HWND, _msg: UINT, _wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    let create: &CREATESTRUCTW = &*(lparam as *const CREATESTRUCTW);
    let inner = rc_from_pointer(create.lpCreateParams);
    SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as _);
    inner.borrow_mut().hwnd = HWindow(hwnd);

    None
}

/// Called when the window is being destroyed.
/// Goal is to release the WindowInner reference that was stashed
/// in the window by wm_nccreate.
unsafe fn wm_ncdestroy(
    hwnd: HWND,
    _msg: UINT,
    _wparam: WPARAM,
    _lparam: LPARAM,
) -> Option<LRESULT> {
    let raw = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as LPVOID;
    if !raw.is_null() {
        let inner = take_rc_from_pointer(raw);
        let mut inner = inner.borrow_mut();
        inner.callbacks.destroy();
        inner.hwnd = HWindow(null_mut());
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
    }

    None
}

fn enable_dark_mode(hwnd: HWND) {
    // Prefer to run in dark mode. This could be made configurable without
    // a huge amount of effort, but I think it's fine to just be always
    // dark mode by default :-p
    // Note that the MS terminal app uses the logic found here for this
    // stuff:
    // https://github.com/microsoft/terminal/blob/9b92986b49bed8cc41fde4d6ef080921c41e6d9e/src/interactivity/win32/windowtheme.cpp#L62
    use winapi::um::dwmapi::DwmSetWindowAttribute;
    use winapi::um::uxtheme::SetWindowTheme;

    const DWMWA_USE_IMMERSIVE_DARK_MODE: DWORD = 19;
    unsafe {
        SetWindowTheme(
            hwnd as _,
            wide_string("DarkMode_Explorer").as_slice().as_ptr(),
            std::ptr::null_mut(),
        );

        let enabled: BOOL = 1;
        DwmSetWindowAttribute(
            hwnd as _,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &enabled as *const _ as *const _,
            std::mem::size_of_val(&enabled) as u32,
        );
    }
}

struct GdiGraphicsContext {
    bitmap: GdiBitmap,
    dpi: u32,
}

impl PaintContext for GdiGraphicsContext {
    fn clear_rect(
        &mut self,
        dest_x: isize,
        dest_y: isize,
        width: usize,
        height: usize,
        color: Color,
    ) {
        self.bitmap.clear_rect(dest_x, dest_y, width, height, color)
    }

    fn clear(&mut self, color: Color) {
        self.bitmap.clear(color);
    }

    fn get_dimensions(&self) -> Dimensions {
        let (pixel_width, pixel_height) = self.bitmap.image_dimensions();
        Dimensions {
            pixel_width,
            pixel_height,
            dpi: self.dpi as usize,
        }
    }

    fn draw_image_subset(
        &mut self,
        dest_x: isize,
        dest_y: isize,
        src_x: usize,
        src_y: usize,
        width: usize,
        height: usize,
        im: &dyn BitmapImage,
        operator: Operator,
    ) {
        self.bitmap
            .draw_image_subset(dest_x, dest_y, src_x, src_y, width, height, im, operator)
    }

    fn draw_line(
        &mut self,
        start_x: isize,
        start_y: isize,
        dest_x: isize,
        dest_y: isize,
        color: Color,
        operator: Operator,
    ) {
        self.bitmap
            .draw_line(start_x, start_y, dest_x, dest_y, color, operator);
    }
}

unsafe fn wm_size(hwnd: HWND, _msg: UINT, _wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    if let Some(inner) = rc_from_hwnd(hwnd) {
        let mut inner = inner.borrow_mut();
        let pixel_width = LOWORD(lparam as DWORD) as usize;
        let pixel_height = HIWORD(lparam as DWORD) as usize;
        inner.callbacks.resize(Dimensions {
            pixel_width,
            pixel_height,
            dpi: GetDpiForWindow(hwnd) as usize,
        });
    }
    None
}

unsafe fn wm_paint(hwnd: HWND, _msg: UINT, _wparam: WPARAM, _lparam: LPARAM) -> Option<LRESULT> {
    if let Some(inner) = rc_from_hwnd(hwnd) {
        let mut inner = inner.borrow_mut();

        let mut ps = PAINTSTRUCT {
            fErase: 0,
            fIncUpdate: 0,
            fRestore: 0,
            hdc: std::ptr::null_mut(),
            rcPaint: RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            },
            rgbReserved: [0; 32],
        };
        let dc = BeginPaint(hwnd, &mut ps);

        let mut rect = RECT {
            left: 0,
            bottom: 0,
            right: 0,
            top: 0,
        };
        GetClientRect(hwnd, &mut rect);
        let width = rect_width(&rect) as usize;
        let height = rect_height(&rect) as usize;

        if width > 0 && height > 0 {
            let dpi = GetDpiForWindow(hwnd);
            let bitmap = GdiBitmap::new_compatible(width, height, dc).unwrap();
            let mut context = GdiGraphicsContext { dpi, bitmap };

            inner.callbacks.paint(&mut context);
            BitBlt(
                dc,
                0,
                0,
                width as i32,
                height as i32,
                context.bitmap.hdc(),
                0,
                0,
                SRCCOPY,
            );
        }

        EndPaint(hwnd, &mut ps);

        Some(0)
    } else {
        None
    }
}

fn mods_and_buttons(wparam: WPARAM) -> (Modifiers, MouseButtons) {
    let mut modifiers = Modifiers::default();
    let mut buttons = MouseButtons::default();
    if wparam & MK_CONTROL != 0 {
        modifiers |= Modifiers::CTRL;
    }
    if wparam & MK_SHIFT != 0 {
        modifiers |= Modifiers::SHIFT;
    }
    if wparam & MK_LBUTTON != 0 {
        buttons |= MouseButtons::LEFT;
    }
    if wparam & MK_MBUTTON != 0 {
        buttons |= MouseButtons::MIDDLE;
    }
    if wparam & MK_RBUTTON != 0 {
        buttons |= MouseButtons::RIGHT;
    }
    // TODO: XBUTTON1 and XBUTTON2?
    (modifiers, buttons)
}

fn mouse_coords(lparam: LPARAM) -> (u16, u16) {
    // These are signed, but we only care about things inside the window...
    let x = (lparam & 0xffff) as i16;
    let y = ((lparam >> 16) & 0xffff) as i16;

    // ... so we truncate to positive values only
    (x.max(0) as u16, y.max(0) as u16)
}

fn apply_mouse_cursor(cursor: Option<MouseCursor>) {
    match cursor {
        None => unsafe {
            SetCursor(null_mut());
        },
        Some(cursor) => unsafe {
            SetCursor(LoadCursorW(
                null_mut(),
                match cursor {
                    MouseCursor::Arrow => IDC_ARROW,
                    MouseCursor::Hand => IDC_HAND,
                    MouseCursor::Text => IDC_IBEAM,
                },
            ));
        },
    }
}

unsafe fn mouse_button(hwnd: HWND, msg: UINT, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    if let Some(inner) = rc_from_hwnd(hwnd) {
        let (modifiers, mouse_buttons) = mods_and_buttons(wparam);
        let (x, y) = mouse_coords(lparam);
        let event = MouseEvent {
            kind: match msg {
                WM_LBUTTONDOWN => MouseEventKind::Press(MousePress::Left),
                WM_LBUTTONUP => MouseEventKind::Release(MousePress::Left),
                WM_RBUTTONDOWN => MouseEventKind::Press(MousePress::Right),
                WM_RBUTTONUP => MouseEventKind::Release(MousePress::Right),
                WM_MBUTTONDOWN => MouseEventKind::Press(MousePress::Middle),
                WM_MBUTTONUP => MouseEventKind::Release(MousePress::Middle),
                WM_LBUTTONDBLCLK => MouseEventKind::DoubleClick(MousePress::Left),
                WM_RBUTTONDBLCLK => MouseEventKind::DoubleClick(MousePress::Right),
                WM_MBUTTONDBLCLK => MouseEventKind::DoubleClick(MousePress::Middle),
                _ => return None,
            },
            x,
            y,
            mouse_buttons,
            modifiers,
        };
        let mut inner = inner.borrow_mut();
        inner
            .callbacks
            .mouse_event(&event, &Window::from_hwnd(hwnd));
        Some(0)
    } else {
        None
    }
}

unsafe fn mouse_move(hwnd: HWND, _msg: UINT, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    if let Some(inner) = rc_from_hwnd(hwnd) {
        let (modifiers, mouse_buttons) = mods_and_buttons(wparam);
        let (x, y) = mouse_coords(lparam);
        let event = MouseEvent {
            kind: MouseEventKind::Move,
            x,
            y,
            mouse_buttons,
            modifiers,
        };

        let mut inner = inner.borrow_mut();
        inner
            .callbacks
            .mouse_event(&event, &Window::from_hwnd(hwnd));
        Some(0)
    } else {
        None
    }
}

unsafe fn mouse_wheel(hwnd: HWND, msg: UINT, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    if let Some(inner) = rc_from_hwnd(hwnd) {
        let (modifiers, mouse_buttons) = mods_and_buttons(wparam);
        let (x, y) = mouse_coords(lparam);
        let position = ((wparam >> 16) & 0xffff) as i16 / WHEEL_DELTA;
        let event = MouseEvent {
            kind: if msg == WM_MOUSEHWHEEL {
                MouseEventKind::HorzWheel(position)
            } else {
                MouseEventKind::VertWheel(position)
            },
            x,
            y,
            mouse_buttons,
            modifiers,
        };
        let mut inner = inner.borrow_mut();
        inner
            .callbacks
            .mouse_event(&event, &Window::from_hwnd(hwnd));
        Some(0)
    } else {
        None
    }
}

unsafe fn key(hwnd: HWND, _msg: UINT, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    if let Some(inner) = rc_from_hwnd(hwnd) {
        let mut inner = inner.borrow_mut();
        let repeat = (lparam & 0xffff) as u16;
        let scan_code = ((lparam >> 16) & 0xff) as u8;
        let releasing = (lparam & (1 << 31)) != 0;

        /*
        let alt_pressed = (lparam & (1 << 29)) != 0;
        let was_down = (lparam & (1 << 30)) != 0;
        let label = match msg {
            WM_CHAR => "WM_CHAR",
            WM_KEYDOWN => "WM_KEYDOWN",
            WM_KEYUP => "WM_KEYUP",
            WM_SYSKEYUP => "WM_SYSKEYUP",
            WM_SYSKEYDOWN => "WM_SYSKEYDOWN",
            _ => "WAT",
        };
        eprintln!(
            "{} c=`{}` repeat={} scan={} alt_pressed={} was_down={} releasing={}",
            label, wparam, repeat, scan_code, alt_pressed, was_down, releasing
        );
        */

        let mut keys = [0u8; 256];
        GetKeyboardState(keys.as_mut_ptr());

        let mut modifiers = Modifiers::default();
        if keys[VK_CONTROL as usize] & 0x80 != 0 {
            modifiers |= Modifiers::CTRL;
        }
        if keys[VK_SHIFT as usize] & 0x80 != 0 {
            modifiers |= Modifiers::SHIFT;
        }
        if keys[VK_MENU as usize] & 0x80 != 0 {
            modifiers |= Modifiers::ALT;
        }
        if keys[VK_LWIN as usize] & 0x80 != 0 || keys[VK_RWIN as usize] & 0x80 != 0 {
            modifiers |= Modifiers::SUPER;
        }

        // If control is pressed, clear the shift state.
        // That gives us a normalized, unshifted/lowercase version of the
        // key for processing elsewhere.
        if modifiers.contains(Modifiers::CTRL) {
            keys[VK_CONTROL as usize] = 0;
            keys[VK_LCONTROL as usize] = 0;
            keys[VK_RCONTROL as usize] = 0;
            keys[VK_SHIFT as usize] = 0;
            keys[VK_LSHIFT as usize] = 0;
            keys[VK_RSHIFT as usize] = 0;
        }

        let mut out = [0u16; 16];
        let res = ToUnicode(
            wparam as u32,
            scan_code as u32,
            keys.as_ptr(),
            out.as_mut_ptr(),
            out.len() as i32,
            0,
        );
        let key = match res {
            // dead key
            -1 => None,
            0 => {
                // No unicode translation, so map the scan code to a virtual key
                // code, and from there map it to our KeyCode type
                match MapVirtualKeyW(scan_code.into(), MAPVK_VSC_TO_VK_EX) as i32 {
                    0 => None,
                    VK_CANCEL => Some(KeyCode::Cancel),
                    VK_BACK => Some(KeyCode::Char('\u{8}')),
                    VK_TAB => Some(KeyCode::Char('\t')),
                    VK_CLEAR => Some(KeyCode::Clear),
                    VK_RETURN => Some(KeyCode::Char('\r')),
                    VK_SHIFT => Some(KeyCode::Shift),
                    VK_CONTROL => Some(KeyCode::Control),
                    VK_MENU => Some(KeyCode::Alt),
                    VK_PAUSE => Some(KeyCode::Pause),
                    VK_CAPITAL => Some(KeyCode::CapsLock),
                    VK_ESCAPE => Some(KeyCode::Char('\u{1b}')),
                    VK_SPACE => Some(KeyCode::Char(' ')),
                    VK_PRIOR => Some(KeyCode::PageUp),
                    VK_NEXT => Some(KeyCode::PageDown),
                    VK_END => Some(KeyCode::End),
                    VK_HOME => Some(KeyCode::Home),
                    VK_LEFT => Some(KeyCode::LeftArrow),
                    VK_UP => Some(KeyCode::UpArrow),
                    VK_RIGHT => Some(KeyCode::RightArrow),
                    VK_DOWN => Some(KeyCode::DownArrow),
                    VK_SELECT => Some(KeyCode::Select),
                    VK_PRINT => Some(KeyCode::Print),
                    VK_EXECUTE => Some(KeyCode::Execute),
                    VK_SNAPSHOT => Some(KeyCode::PrintScreen),
                    VK_INSERT => Some(KeyCode::Insert),
                    VK_DELETE => Some(KeyCode::Char('\u{7f}')),
                    VK_HELP => Some(KeyCode::Help),
                    // 0-9 happen to overlap with ascii
                    i @ 0x30..=0x39 => Some(KeyCode::Char(i as u8 as char)),
                    // a-z also overlap with ascii
                    i @ 0x41..=0x5a => Some(KeyCode::Char(i as u8 as char)),
                    VK_LWIN => Some(KeyCode::LeftWindows),
                    VK_RWIN => Some(KeyCode::RightWindows),
                    VK_APPS => Some(KeyCode::Applications),
                    VK_SLEEP => Some(KeyCode::Sleep),
                    i @ VK_NUMPAD0..=VK_NUMPAD9 => Some(KeyCode::Numpad((i - VK_NUMPAD0) as u8)),
                    VK_MULTIPLY => Some(KeyCode::Multiply),
                    VK_ADD => Some(KeyCode::Add),
                    VK_SEPARATOR => Some(KeyCode::Separator),
                    VK_SUBTRACT => Some(KeyCode::Subtract),
                    VK_DECIMAL => Some(KeyCode::Decimal),
                    VK_DIVIDE => Some(KeyCode::Divide),
                    i @ VK_F1..=VK_F24 => Some(KeyCode::Function((1 + i - VK_F1) as u8)),
                    VK_NUMLOCK => Some(KeyCode::NumLock),
                    VK_SCROLL => Some(KeyCode::ScrollLock),
                    VK_LSHIFT => Some(KeyCode::LeftShift),
                    VK_RSHIFT => Some(KeyCode::RightShift),
                    VK_LCONTROL => Some(KeyCode::LeftControl),
                    VK_RCONTROL => Some(KeyCode::RightControl),
                    VK_LMENU => Some(KeyCode::LeftAlt),
                    VK_RMENU => Some(KeyCode::RightAlt),
                    VK_BROWSER_BACK => Some(KeyCode::BrowserBack),
                    VK_BROWSER_FORWARD => Some(KeyCode::BrowserForward),
                    VK_BROWSER_REFRESH => Some(KeyCode::BrowserRefresh),
                    VK_BROWSER_STOP => Some(KeyCode::BrowserStop),
                    VK_BROWSER_SEARCH => Some(KeyCode::BrowserSearch),
                    VK_BROWSER_FAVORITES => Some(KeyCode::BrowserFavorites),
                    VK_BROWSER_HOME => Some(KeyCode::BrowserHome),
                    VK_VOLUME_MUTE => Some(KeyCode::VolumeMute),
                    VK_VOLUME_DOWN => Some(KeyCode::VolumeDown),
                    VK_VOLUME_UP => Some(KeyCode::VolumeUp),
                    VK_MEDIA_NEXT_TRACK => Some(KeyCode::MediaNextTrack),
                    VK_MEDIA_PREV_TRACK => Some(KeyCode::MediaPrevTrack),
                    VK_MEDIA_STOP => Some(KeyCode::MediaStop),
                    VK_MEDIA_PLAY_PAUSE => Some(KeyCode::MediaPlayPause),
                    _ => None,
                }
            }
            1 => Some(KeyCode::Char(std::char::from_u32_unchecked(out[0] as u32))),
            n => {
                let s = &out[0..n as usize];
                match String::from_utf16(s) {
                    Ok(s) => Some(KeyCode::Composed(s)),
                    Err(err) => {
                        eprintln!("translated to {} WCHARS, err: {}", n, err);
                        None
                    }
                }
            }
        };

        if let Some(key) = key {
            let key = KeyEvent {
                key,
                modifiers,
                repeat_count: repeat,
                key_is_down: !releasing,
            };
            let handled = inner.callbacks.key_event(&key, &Window::from_hwnd(hwnd));

            if handled {
                return Some(0);
            }
        }
    }
    None
}

unsafe fn do_wnd_proc(hwnd: HWND, msg: UINT, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    match msg {
        WM_NCCREATE => wm_nccreate(hwnd, msg, wparam, lparam),
        WM_NCDESTROY => wm_ncdestroy(hwnd, msg, wparam, lparam),
        WM_PAINT => wm_paint(hwnd, msg, wparam, lparam),
        WM_SIZE => wm_size(hwnd, msg, wparam, lparam),
        WM_KEYDOWN | WM_KEYUP | WM_SYSKEYUP | WM_SYSKEYDOWN => key(hwnd, msg, wparam, lparam),
        WM_MOUSEMOVE => mouse_move(hwnd, msg, wparam, lparam),
        WM_MOUSEHWHEEL | WM_MOUSEWHEEL => mouse_wheel(hwnd, msg, wparam, lparam),
        WM_LBUTTONDBLCLK | WM_RBUTTONDBLCLK | WM_MBUTTONDBLCLK | WM_LBUTTONDOWN | WM_LBUTTONUP
        | WM_RBUTTONDOWN | WM_RBUTTONUP | WM_MBUTTONDOWN | WM_MBUTTONUP => {
            mouse_button(hwnd, msg, wparam, lparam)
        }
        WM_CLOSE => {
            if let Some(inner) = rc_from_hwnd(hwnd) {
                let mut inner = inner.borrow_mut();
                if !inner.callbacks.can_close() {
                    // Don't let it close
                    return Some(0);
                }
            }
            None
        }
        _ => None,
    }
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: UINT,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match std::panic::catch_unwind(|| {
        do_wnd_proc(hwnd, msg, wparam, lparam)
            .unwrap_or_else(|| DefWindowProcW(hwnd, msg, wparam, lparam))
    }) {
        Ok(result) => result,
        Err(_) => std::process::exit(1),
    }
}