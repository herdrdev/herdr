use std::ffi::{c_char, c_void, CString};

#[cfg(test)]
const TEXT_ENCODING_VARIABLE: &str = "__CF_USER_TEXT_ENCODING";

pub fn write_clipboard(bytes: &[u8]) -> bool {
    let Some(text) = clipboard_text(bytes).and_then(|text| CString::new(text).ok()) else {
        return false;
    };

    // SAFETY: The UTF-8 string is bounded by a NUL and lives for the call.
    unsafe { write_with_pasteboard(&text) }
}

fn clipboard_text(bytes: &[u8]) -> Option<&str> {
    let text = std::str::from_utf8(bytes).ok()?;
    (!text.contains('\0')).then_some(text)
}

type Id = *mut c_void;
type Sel = *const c_void;
type SendClass = unsafe extern "C" fn(Id, Sel) -> Id;
type SendText = unsafe extern "C" fn(Id, Sel, *const c_char) -> Id;
#[cfg(test)]
type SendType = unsafe extern "C" fn(Id, Sel, *const c_void) -> Id;
#[cfg(test)]
type SendBytes = unsafe extern "C" fn(Id, Sel) -> *const c_char;
type SendString = unsafe extern "C" fn(Id, Sel, Id, *const c_void) -> bool;
type SendCount = unsafe extern "C" fn(Id, Sel) -> isize;
type SendVoid = unsafe extern "C" fn(Id, Sel);

#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {
    static NSPasteboardTypeString: *const c_void;
}

#[link(name = "objc", kind = "dylib")]
unsafe extern "C" {
    fn objc_getClass(name: *const c_char) -> Id;
    fn sel_registerName(name: *const c_char) -> Sel;
    fn objc_msgSend();
}

struct AutoreleasePool(Option<Id>);

impl AutoreleasePool {
    unsafe fn new() -> Option<Self> {
        let class = class(c"NSAutoreleasePool");
        let new = selector(c"new")?;
        let pool = send_id(class, new)?;
        Some(Self(Some(pool)))
    }

    fn drain(&mut self) {
        if let Some(pool) = self.0.take() {
            unsafe {
                if let Some(drain) = selector(c"drain") {
                    send_void(pool, drain);
                }
            }
        }
    }
}

impl Drop for AutoreleasePool {
    fn drop(&mut self) {
        self.drain();
    }
}

unsafe fn write_with_pasteboard(text: &CString) -> bool {
    let Some(_pool) = AutoreleasePool::new() else {
        return false;
    };
    let Some(kind) = non_null(NSPasteboardTypeString) else {
        return false;
    };
    let Some(string_class) = non_null_id(class(c"NSString")) else {
        return false;
    };
    let Some(string) = (|| {
        send_text(
            string_class,
            selector(c"stringWithUTF8String:")?,
            text.as_ptr(),
        )
    })() else {
        return false;
    };
    let Some(pasteboard_class) = non_null_id(class(c"NSPasteboard")) else {
        return false;
    };
    let Some(pasteboard) = (|| send_id(pasteboard_class, selector(c"generalPasteboard")?))() else {
        return false;
    };

    let Some(clear_contents) = selector(c"clearContents") else {
        return false;
    };
    let Some(set_string) = selector(c"setString:forType:") else {
        return false;
    };

    send_count(pasteboard, clear_contents) != 0 && send_string(pasteboard, set_string, string, kind)
}

unsafe fn class(name: &std::ffi::CStr) -> Id {
    objc_getClass(name.as_ptr())
}

unsafe fn selector(name: &std::ffi::CStr) -> Option<Sel> {
    let selector = sel_registerName(name.as_ptr());
    (!selector.is_null()).then_some(selector)
}

unsafe fn non_null(value: *const c_void) -> Option<*const c_void> {
    (!value.is_null()).then_some(value)
}

unsafe fn non_null_id(value: Id) -> Option<Id> {
    (!value.is_null()).then_some(value)
}

unsafe fn send_id(receiver: Id, selector: Sel) -> Option<Id> {
    // SAFETY: This message returns an object reference.
    let send: SendClass = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
    let result = send(receiver, selector);
    (!result.is_null()).then_some(result)
}

unsafe fn send_string(receiver: Id, selector: Sel, string: Id, kind: *const c_void) -> bool {
    // SAFETY: Both object references and the type constant remain valid here.
    let send: SendString = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
    send(receiver, selector, string, kind)
}

unsafe fn send_count(receiver: Id, selector: Sel) -> isize {
    // SAFETY: This message returns an integer change count.
    let send: SendCount = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
    send(receiver, selector)
}

unsafe fn send_void(receiver: Id, selector: Sel) {
    // SAFETY: This drain message has no arguments or return value.
    let send: SendVoid = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
    send(receiver, selector);
}

#[cfg(test)]
unsafe fn send_type(receiver: Id, selector: Sel, kind: *const c_void) -> Option<Id> {
    // SAFETY: This message takes a type constant and returns an object.
    let send: SendType = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
    let result = send(receiver, selector, kind);
    (!result.is_null()).then_some(result)
}

unsafe fn send_text(receiver: Id, selector: Sel, text: *const c_char) -> Option<Id> {
    // SAFETY: The C string remains valid and immutable for the call.
    let send: SendText = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
    let result = send(receiver, selector, text);
    (!result.is_null()).then_some(result)
}

#[cfg(test)]
unsafe fn send_bytes(receiver: Id, selector: Sel) -> Option<&'static std::ffi::CStr> {
    // SAFETY: This message returns a NUL-terminated UTF-8 buffer owned by the
    // receiver. The buffer remains valid until the surrounding pool is drained.
    let send: SendBytes = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
    let result = send(receiver, selector);
    (!result.is_null()).then_some(std::ffi::CStr::from_ptr(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    static CLIPBOARD_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn clipboard_text_accepts_unicode_and_rejects_invalid_input() {
        assert_eq!(clipboard_text(b"ASCII"), Some("ASCII"));
        assert_eq!(clipboard_text("天地abc".as_bytes()), Some("天地abc"));
        assert_eq!(clipboard_text(&[0xff]), None);
        assert_eq!(clipboard_text(b"a\0b"), None);
    }

    #[test]
    #[ignore = "changes the system pasteboard and restores text if unchanged"]
    fn clipboard_round_trips_ascii_and_cjk_in_legacy_encoding_environment() {
        let _environment = EnvironmentGuard {
            text_encoding: legacy_text_encoding(),
        };
        set_legacy_text_encoding();
        let _guard = system_clipboard_guard();

        assert!(write_clipboard(b"ASCII"));
        assert_eq!(read_clipboard_text(), Some("ASCII".to_owned()));
        assert!(write_clipboard("天地abc".as_bytes()));
        assert_eq!(read_clipboard_text(), Some("天地abc".to_owned()));
    }

    #[test]
    #[ignore = "changes the system pasteboard and restores text if unchanged"]
    fn invalid_clipboard_input_does_not_clear_existing_text() {
        let _environment = EnvironmentGuard {
            text_encoding: legacy_text_encoding(),
        };
        set_legacy_text_encoding();
        let _guard = system_clipboard_guard();

        assert!(write_clipboard(b"sentinel"));
        let expected = read_clipboard_text();

        assert!(!write_clipboard(&[0xff]));
        assert_eq!(read_clipboard_text(), expected);
        assert!(!write_clipboard(b"a\0b"));
        assert_eq!(read_clipboard_text(), expected);
    }

    struct ClipboardGuard {
        original: Option<String>,
        original_count: Option<isize>,
        _lock: MutexGuard<'static, ()>,
    }

    struct EnvironmentGuard {
        text_encoding: Option<String>,
    }

    impl Drop for EnvironmentGuard {
        fn drop(&mut self) {
            restore_text_encoding(self.text_encoding.take());
        }
    }

    impl Drop for ClipboardGuard {
        fn drop(&mut self) {
            if current_change_count() == self.original_count {
                if let Some(original) = self.original.as_ref() {
                    write_clipboard(original.as_bytes());
                } else {
                    clear_clipboard();
                }
            }
        }
    }

    fn system_clipboard_guard() -> ClipboardGuard {
        let lock = CLIPBOARD_TEST_LOCK
            .lock()
            .expect("another clipboard test panicked");
        ClipboardGuard {
            original: read_clipboard_text(),
            original_count: current_change_count(),
            _lock: lock,
        }
    }

    fn legacy_text_encoding() -> Option<String> {
        std::env::var(TEXT_ENCODING_VARIABLE).ok()
    }

    fn set_legacy_text_encoding() {
        // The issue reproduces when CoreFoundation falls back to MacRoman for
        // command-line tools. Restore this process-global value after the test.
        std::env::set_var(TEXT_ENCODING_VARIABLE, "0x1F7:0x19:0x34");
    }

    fn restore_text_encoding(value: Option<String>) {
        if let Some(value) = value {
            std::env::set_var(TEXT_ENCODING_VARIABLE, value);
        } else {
            std::env::remove_var(TEXT_ENCODING_VARIABLE);
        }
    }

    fn clear_clipboard() {
        unsafe {
            let Some(pasteboard) = general_pasteboard() else {
                return;
            };
            let Some(clear_contents) = selector(c"clearContents") else {
                return;
            };
            send_count(pasteboard, clear_contents);
        }
    }

    fn current_change_count() -> Option<isize> {
        unsafe {
            let pasteboard = general_pasteboard()?;
            let change_count = selector(c"changeCount")?;
            Some(send_count(pasteboard, change_count))
        }
    }

    unsafe fn general_pasteboard() -> Option<Id> {
        unsafe {
            let pasteboard_class = non_null_id(class(c"NSPasteboard"))?;
            send_id(pasteboard_class, selector(c"generalPasteboard")?)
        }
    }

    fn read_clipboard_text() -> Option<String> {
        unsafe {
            let _pool = AutoreleasePool::new();
            let kind = non_null(NSPasteboardTypeString)?;
            let pasteboard_class = non_null_id(class(c"NSPasteboard"))?;
            let pasteboard = send_id(pasteboard_class, selector(c"generalPasteboard")?)?;
            let string = send_type(pasteboard, selector(c"stringForType:")?, kind)?;
            send_bytes(string, selector(c"UTF8String")?)?
                .to_str()
                .ok()
                .map(str::to_owned)
        }
    }
}
