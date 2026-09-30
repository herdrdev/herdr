use std::{
    collections::HashMap,
    io,
    ptr::null_mut,
    sync::{mpsc, Arc, LazyLock, Mutex},
    time::Duration,
};

use sha2::{Digest, Sha256};
use windows::{
    core::HSTRING,
    Data::Xml::Dom::XmlDocument,
    Win32::System::Com::{CoCreateGuid, CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED},
    UI::Notifications::{ToastNotification, ToastNotificationManager, ToastNotifier},
};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, WPARAM},
    System::{Console::GetConsoleWindow, LibraryLoader::GetModuleHandleW, Registry::*},
    UI::WindowsAndMessaging::*,
};

const APP_ID: &str = "Herdr.Desktop";
const WINDOW_CLASS: &str = "HerdrNotificationActivation";
const ACTIVATION_MESSAGE: &str = "Herdr.Notification.Activate";
const SHOW_MESSAGE: u32 = WM_APP + 1;
const ACTIVATOR: &str = "{D58C72D3-4548-4A40-B55B-092EF0B365C3}";

type Callback = Arc<dyn Fn() + Send + Sync>;

struct Activation {
    token: u128,
    callback: Callback,
}

// ponytail: one entry per notified target for this client lifetime. Retiring by
// history loses clicks already removed by Windows; add retention only if measured.
static ACTIVATIONS: LazyLock<Mutex<HashMap<String, Activation>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static SERVICE: Mutex<Option<Service>> = Mutex::new(None);

#[derive(Clone)]
struct Service {
    window: usize,
    requests: mpsc::Sender<Request>,
}

struct Request {
    title: String,
    body: Option<String>,
    key: String,
    callback: Callback,
    ready: mpsc::SyncSender<io::Result<bool>>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|err| err.into_inner())
}

fn winrt<T>(result: windows::core::Result<T>) -> io::Result<T> {
    result.map_err(io::Error::other)
}

struct ComApartment;

impl ComApartment {
    fn new() -> io::Result<Self> {
        winrt(unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok() })?;
        Ok(Self)
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}

fn random_token() -> io::Result<u128> {
    winrt(unsafe { CoCreateGuid() }).map(|guid| guid.to_u128())
}

fn issue_activation(
    activations: &mut HashMap<String, Activation>,
    key: &str,
    callback: Callback,
) -> io::Result<u128> {
    if let Some(activation) = activations.get_mut(key) {
        activation.callback = callback;
        return Ok(activation.token);
    }
    let token = random_token()?;
    activations.insert(key.into(), Activation { token, callback });
    Ok(token)
}

fn scheme_for_executable(executable: &std::path::Path) -> String {
    let hash = Sha256::digest(executable.as_os_str().as_encoded_bytes());
    format!(
        "herdr-notification-{:016x}",
        u64::from_be_bytes(hash[..8].try_into().expect("eight bytes"))
    )
}

struct RegistryKey(HKEY);

impl RegistryKey {
    fn create(path: &str) -> io::Result<Self> {
        let mut handle = null_mut();
        let status = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                super::wide_null(path).as_ptr(),
                0,
                null_mut(),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE,
                std::ptr::null(),
                &mut handle,
                null_mut(),
            )
        };
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        Ok(Self(handle))
    }

    fn set(&self, name: &str, value: &str) -> io::Result<()> {
        let value = super::wide_null(value);
        let status = unsafe {
            RegSetValueExW(
                self.0,
                super::wide_null(name).as_ptr(),
                0,
                REG_SZ,
                value.as_ptr().cast(),
                (value.len() * 2) as u32,
            )
        };
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        Ok(())
    }
}

impl Drop for RegistryKey {
    fn drop(&mut self) {
        unsafe {
            RegCloseKey(self.0);
        }
    }
}

fn register(executable: &std::path::Path, scheme: &str) -> io::Result<()> {
    let parent = executable
        .parent()
        .ok_or_else(|| io::Error::other("missing executable directory"))?;
    let host = parent.join("conpty/x64/OpenConsole.exe");
    if !host.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "Windows notification activation requires the bundled ConPTY runtime",
        ));
    }
    let identity = RegistryKey::create(&format!(r"Software\Classes\AppUserModelId\{APP_ID}"))?;
    identity.set("DisplayName", "Herdr")?;
    // Unpackaged protocol toasts use a stub activator; no COM server is installed.
    identity.set("CustomActivator", ACTIVATOR)?;
    let protocol = RegistryKey::create(&format!(r"Software\Classes\{scheme}"))?;
    protocol.set("URL Protocol", "")?;
    let command = RegistryKey::create(&format!(r"Software\Classes\{scheme}\shell\open\command"))?;
    // The pinned, app-local console host avoids flashing a new terminal on click.
    command.set(
        "",
        &format!(
            "\"{}\" --headless -- \"{}\" --notification-activate \"%1\"",
            host.display(),
            executable.display()
        ),
    )
}

fn activation_message() -> u32 {
    unsafe { RegisterWindowMessageW(super::wide_null(ACTIVATION_MESSAGE).as_ptr()) }
}

unsafe extern "system" fn window_proc(window: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if message == activation_message() {
        let token = ((w as u128) << 64) | (l as u64 as u128);
        let callback = lock(&ACTIVATIONS)
            .values()
            .find(|activation| activation.token == token)
            .map(|activation| Arc::clone(&activation.callback));
        if let Some(callback) = callback {
            callback();
        }
        return 0;
    }
    DefWindowProcW(window, message, w, l)
}

pub(crate) fn foreground_desktop_notification_host() {
    // Resolve after target validation: a Windows Terminal tab can move windows.
    unsafe {
        let host = GetAncestor(GetConsoleWindow(), GA_ROOTOWNER);
        if IsWindowVisible(host) != 0 {
            if IsIconic(host) != 0 {
                ShowWindow(host, SW_RESTORE);
            }
            SetForegroundWindow(host);
        }
    }
}

fn create_window() -> io::Result<HWND> {
    let class = super::wide_null(WINDOW_CLASS);
    let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
    let descriptor = WNDCLASSW {
        lpfnWndProc: Some(window_proc),
        hInstance: instance,
        lpszClassName: class.as_ptr(),
        ..unsafe { std::mem::zeroed() }
    };
    if unsafe { RegisterClassW(&descriptor) } == 0 {
        let err = io::Error::last_os_error();
        if err.raw_os_error() != Some(1410) {
            return Err(err);
        }
    }
    let window = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            class.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            instance,
            std::ptr::null(),
        )
    };
    if window.is_null() {
        return Err(io::Error::last_os_error());
    }
    let message = activation_message();
    if message == 0
        || unsafe { ChangeWindowMessageFilterEx(window, message, MSGFLT_ALLOW, null_mut()) } == 0
    {
        let error = io::Error::last_os_error();
        unsafe {
            DestroyWindow(window);
        }
        return Err(error);
    }
    Ok(window)
}

fn notification_xml(title: &str, body: Option<&str>, uri: &str) -> io::Result<XmlDocument> {
    let xml = winrt(XmlDocument::new())?;
    winrt(xml.LoadXml(&HSTRING::from(r#"<toast activationType="protocol"><visual><binding template="ToastGeneric"/></visual><audio silent="true"/></toast>"#)))?;
    let root = winrt(xml.DocumentElement())?;
    winrt(root.SetAttribute(&HSTRING::from("launch"), &HSTRING::from(uri)))?;
    let binding = winrt(winrt(xml.GetElementsByTagName(&HSTRING::from("binding")))?.Item(0))?;
    for text in std::iter::once(title).chain(body) {
        let element = winrt(xml.CreateElement(&HSTRING::from("text")))?;
        let node = winrt(xml.CreateTextNode(&HSTRING::from(text)))?;
        winrt(element.AppendChild(&node))?;
        winrt(binding.AppendChild(&element))?;
    }
    Ok(xml)
}

fn show(
    request: &Request,
    window: HWND,
    scheme: &str,
    group: &str,
    notifier: &ToastNotifier,
) -> io::Result<bool> {
    let token = {
        issue_activation(
            &mut lock(&ACTIVATIONS),
            &request.key,
            Arc::clone(&request.callback),
        )?
    };
    let uri = format!("{scheme}://{:x}/{token:032x}", window as usize);
    let xml = notification_xml(&request.title, request.body.as_deref(), &uri)?;
    let notification = winrt(ToastNotification::CreateToastNotification(&xml))?;
    winrt(notification.SetGroup(&HSTRING::from(group)))?;
    // Only pane notifications replace one another; custom messages stay distinct.
    if request.key != "general" {
        winrt(notification.SetTag(&HSTRING::from(format!("{token:032x}"))))?;
    }
    // Setting can report ELEMENT_NOT_FOUND before the first successful Show.
    winrt(notifier.Show(&notification))?;
    Ok(true)
}

fn run(requests: mpsc::Receiver<Request>, ready: mpsc::SyncSender<io::Result<usize>>) {
    let _apartment = match ComApartment::new() {
        Ok(apartment) => apartment,
        Err(err) => {
            let _ = ready.send(Err(err));
            return;
        }
    };
    let initialize = || -> io::Result<_> {
        let executable = std::env::current_exe()?;
        let scheme = scheme_for_executable(&executable);
        register(&executable, &scheme)?;
        let group = format!("{:032x}", random_token()?);
        let notifier = winrt(ToastNotificationManager::CreateToastNotifierWithId(
            &HSTRING::from(APP_ID),
        ))?;
        let window = create_window()?;
        Ok((window, scheme, group, notifier))
    };
    let (window, scheme, group, notifier) = match initialize() {
        Ok(state) => state,
        Err(err) => {
            let _ = ready.send(Err(err));
            return;
        }
    };
    if ready.send(Ok(window as usize)).is_ok() {
        let mut message: MSG = unsafe { std::mem::zeroed() };
        while unsafe { GetMessageW(&mut message, window, 0, 0) } > 0 {
            if message.message == SHOW_MESSAGE {
                while let Ok(request) = requests.try_recv() {
                    let result = show(&request, window, &scheme, &group, &notifier);
                    let _ = request.ready.send(result);
                }
            } else {
                unsafe {
                    DispatchMessageW(&message);
                }
            }
        }
    }
    unsafe {
        DestroyWindow(window);
    }
}

pub(crate) fn show_actionable_desktop_notification(
    title: &str,
    body: Option<&str>,
    key: String,
    callback: Callback,
) -> io::Result<bool> {
    let service = {
        let mut service = lock(&SERVICE);
        if service.is_none() {
            let (requests, receiver) = mpsc::channel();
            let (ready, response) = mpsc::sync_channel(1);
            std::thread::Builder::new()
                .name("herdr-windows-notification".into())
                .spawn(move || run(receiver, ready))?;
            let window = response
                .recv_timeout(Duration::from_secs(2))
                .map_err(io::Error::other)??;
            *service = Some(Service { window, requests });
        }
        service
            .as_ref()
            .expect("initialized notification service")
            .clone()
    };
    let (ready, response) = mpsc::sync_channel(1);
    service
        .requests
        .send(Request {
            title: title.into(),
            body: body.map(str::to_owned),
            key,
            callback,
            ready,
        })
        .map_err(io::Error::other)?;
    if unsafe { PostMessageW(service.window as HWND, SHOW_MESSAGE, 0, 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    response
        .recv_timeout(Duration::from_secs(2))
        .map_err(io::Error::other)?
}

pub(crate) fn show_desktop_notification(title: &str, body: Option<&str>) -> io::Result<bool> {
    show_actionable_desktop_notification(
        title,
        body,
        "general".into(),
        Arc::new(foreground_desktop_notification_host),
    )
}

fn parse_activation(uri: &str, scheme: &str) -> Option<(usize, u128)> {
    let rest = uri.strip_prefix(scheme)?.strip_prefix("://")?;
    let (window, token) = rest.split_once('/')?;
    if window.is_empty()
        || window.len() > 16
        || token.len() != 32
        || !window
            .bytes()
            .chain(token.bytes())
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return None;
    }
    let window = usize::from_str_radix(window, 16).ok()?;
    (window != 0).then_some((window, u128::from_str_radix(token, 16).ok()?))
}

pub(crate) fn maybe_activate_desktop_notification(args: &[String]) -> Option<io::Result<()>> {
    if args.get(1).map(String::as_str) != Some("--notification-activate") {
        return None;
    }
    Some((|| {
        let scheme = scheme_for_executable(&std::env::current_exe()?);
        let (window, token) = args
            .get(2)
            .filter(|_| args.len() == 3)
            .and_then(|uri| parse_activation(uri, &scheme))
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid notification activation",
                )
            })?;
        let window = window as HWND;
        let mut class = [0u16; 64];
        let length = unsafe { GetClassNameW(window, class.as_mut_ptr(), class.len() as i32) };
        if length <= 0 || String::from_utf16_lossy(&class[..length as usize]) != WINDOW_CLASS {
            return Ok(());
        }
        let mut process = 0;
        unsafe {
            GetWindowThreadProcessId(window, &mut process);
            AllowSetForegroundWindow(process);
        }
        let message = activation_message();
        if message == 0
            || unsafe { PostMessageW(window, message, (token >> 64) as usize, token as isize) } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    })())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacement_preserves_issued_click_and_uses_latest_callback() {
        let mut activations = HashMap::new();
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let first_count = Arc::clone(&count);
        let original = issue_activation(
            &mut activations,
            "endpoint/boot/pane",
            Arc::new(move || {
                first_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }),
        )
        .expect("token");
        let latest_count = Arc::clone(&count);
        let replacement = issue_activation(
            &mut activations,
            "endpoint/boot/pane",
            Arc::new(move || {
                latest_count.fetch_add(10, std::sync::atomic::Ordering::Relaxed);
            }),
        )
        .expect("replacement");
        assert_eq!(original, replacement);
        // There is deliberately no dependency on the history entry still existing.
        let callback = activations
            .values()
            .find(|entry| entry.token == original)
            .expect("issued click");
        (callback.callback)();
        assert_eq!(count.load(std::sync::atomic::Ordering::Relaxed), 10);
        let restarted =
            issue_activation(&mut activations, "endpoint/new-boot/pane", Arc::new(|| {}))
                .expect("new boot");
        assert_ne!(restarted, original);
    }

    #[test]
    fn activation_requires_exact_scheme_window_and_full_issued_token_shape() {
        let scheme = "herdr-notification-test";
        let token = 0x0123456789abcdef_fedcba9876543210u128;
        let uri = format!("{scheme}://1a/{token:032x}");
        assert_eq!(parse_activation(&uri, scheme), Some((0x1a, token)));
        for invalid in [
            "other://1a/0123456789abcdeffedcba9876543210",
            "herdr-notification-test://0/0123456789abcdeffedcba9876543210",
            "herdr-notification-test://1a/123",
            "herdr-notification-test://1a/0123456789abcdeffedcba9876543210/extra",
            "herdr-notification-test://1a/+123456789abcdeffedcba9876543210",
        ] {
            assert_eq!(parse_activation(invalid, scheme), None, "{invalid}");
        }
    }

    #[test]
    fn notification_text_remains_literal_xml_and_unicode() {
        let _apartment = ComApartment::new().expect("COM");
        let title = "Agent <done> & 😀";
        let xml = notification_xml(title, Some("body & <tag>"), "herdr-test://1/123").expect("XML");
        let texts = xml
            .GetElementsByTagName(&HSTRING::from("text"))
            .expect("texts");
        assert_eq!(
            texts.Item(0).expect("title").InnerText().expect("text"),
            HSTRING::from(title)
        );
        assert_eq!(
            texts.Item(1).expect("body").InnerText().expect("text"),
            HSTRING::from("body & <tag>")
        );
    }
}
