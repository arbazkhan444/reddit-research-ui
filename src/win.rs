//! Small Win32 helpers so the native title bar matches the app theme.

type Hwnd = isize;

const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
const DWMWA_BORDER_COLOR: u32 = 34;
const DWMWA_CAPTION_COLOR: u32 = 35;
const DWMWA_TEXT_COLOR: u32 = 36;

#[link(name = "user32")]
extern "system" {
    fn EnumWindows(
        callback: Option<unsafe extern "system" fn(Hwnd, isize) -> i32>,
        extra: isize,
    ) -> i32;
    fn GetWindowThreadProcessId(window: Hwnd, pid: *mut u32) -> u32;
    fn IsWindowVisible(window: Hwnd) -> i32;
    fn GetWindowTextLengthW(window: Hwnd) -> i32;
}

#[link(name = "dwmapi")]
extern "system" {
    fn DwmSetWindowAttribute(
        window: Hwnd,
        attribute: u32,
        value: *const core::ffi::c_void,
        size: u32,
    ) -> i32;
}

struct Lookup {
    pid: u32,
    found: Hwnd,
}

unsafe extern "system" fn find_main_window(window: Hwnd, extra: isize) -> i32 {
    let lookup = &mut *(extra as *mut Lookup);
    let mut pid = 0u32;
    GetWindowThreadProcessId(window, &mut pid);
    // The event-target window is visible but has no title. Skip it.
    if pid == lookup.pid && IsWindowVisible(window) != 0 && GetWindowTextLengthW(window) > 0 {
        lookup.found = window;
        return 0;
    }
    1
}

/// The first visible top-level window owned by this process.
pub fn main_window() -> Option<Hwnd> {
    let mut lookup = Lookup {
        pid: std::process::id(),
        found: 0,
    };
    unsafe {
        EnumWindows(Some(find_main_window), &mut lookup as *mut Lookup as isize);
    }
    (lookup.found != 0).then_some(lookup.found)
}

fn colorref(color: eframe::egui::Color32) -> u32 {
    (color.r() as u32) | ((color.g() as u32) << 8) | ((color.b() as u32) << 16)
}

/// Match the caption, border and caption text to the app palette.
pub fn style_title_bar(
    window: Hwnd,
    dark: bool,
    background: eframe::egui::Color32,
    text: eframe::egui::Color32,
    border: eframe::egui::Color32,
) {
    let flag: i32 = if dark { 1 } else { 0 };
    let caption = colorref(background);
    let caption_text = colorref(text);
    let caption_border = colorref(border);
    unsafe {
        DwmSetWindowAttribute(
            window,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &flag as *const i32 as *const _,
            4,
        );
        DwmSetWindowAttribute(
            window,
            DWMWA_BORDER_COLOR,
            &caption_border as *const u32 as *const _,
            4,
        );
        DwmSetWindowAttribute(
            window,
            DWMWA_CAPTION_COLOR,
            &caption as *const u32 as *const _,
            4,
        );
        DwmSetWindowAttribute(
            window,
            DWMWA_TEXT_COLOR,
            &caption_text as *const u32 as *const _,
            4,
        );
    }
}
