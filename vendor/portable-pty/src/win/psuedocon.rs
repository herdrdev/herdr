use super::WinChild;
use crate::cmdbuilder::CommandBuilder;
use crate::win::procthreadattr::ProcThreadAttributeList;
use anyhow::{bail, ensure, Error};
use filedescriptor::{FileDescriptor, OwnedHandle};
use lazy_static::lazy_static;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{Error as IoError, Read};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::MetadataExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::{mem, ptr};
use winapi::shared::minwindef::{DWORD, HMODULE};
use winapi::shared::winerror::{HRESULT, S_OK};
use winapi::um::handleapi::*;
use winapi::um::libloaderapi::{
    FreeLibrary, GetModuleHandleW, GetProcAddress, LoadLibraryExW,
    LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32,
};
use winapi::um::processthreadsapi::*;
use winapi::um::winbase::{
    CREATE_UNICODE_ENVIRONMENT, EXTENDED_STARTUPINFO_PRESENT, STARTF_USESTDHANDLES, STARTUPINFOEXW,
};
use winapi::um::wincon::COORD;
use winapi::um::winnt::HANDLE;

pub type HPCON = HANDLE;

pub const PSUEDOCONSOLE_INHERIT_CURSOR: DWORD = 0x1;
pub const PSEUDOCONSOLE_RESIZE_QUIRK: DWORD = 0x2;
pub const PSEUDOCONSOLE_WIN32_INPUT_MODE: DWORD = 0x4;
#[allow(dead_code)]
pub const PSEUDOCONSOLE_PASSTHROUGH_MODE: DWORD = 0x8;

type CreatePseudoConsoleFn = unsafe extern "system" fn(
    size: COORD,
    h_input: HANDLE,
    h_output: HANDLE,
    flags: DWORD,
    hpc: *mut HPCON,
) -> HRESULT;
type ResizePseudoConsoleFn = unsafe extern "system" fn(hpc: HPCON, size: COORD) -> HRESULT;
type ClosePseudoConsoleFn = unsafe extern "system" fn(hpc: HPCON);

// These fields intentionally mirror the exported Win32 symbol names.
#[allow(non_snake_case)]
struct ConPtyFuncs {
    module: HMODULE,
    owned: bool,
    CreatePseudoConsole: CreatePseudoConsoleFn,
    ResizePseudoConsole: ResizePseudoConsoleFn,
    ClosePseudoConsole: ClosePseudoConsoleFn,
}

unsafe impl Send for ConPtyFuncs {}
unsafe impl Sync for ConPtyFuncs {}

impl Drop for ConPtyFuncs {
    fn drop(&mut self) {
        if self.owned {
            unsafe { FreeLibrary(self.module) };
        }
    }
}

impl ConPtyFuncs {
    unsafe fn from_module(module: HMODULE, owned: bool) -> Result<Self, String> {
        if module.is_null() {
            return Err(IoError::last_os_error().to_string());
        }
        Ok(Self {
            module,
            owned,
            CreatePseudoConsole: load_symbol(module, b"CreatePseudoConsole\0")?,
            ResizePseudoConsole: load_symbol(module, b"ResizePseudoConsole\0")?,
            ClosePseudoConsole: load_symbol(module, b"ClosePseudoConsole\0")?,
        })
    }
}

unsafe fn load_symbol<T: Copy>(module: HMODULE, name: &'static [u8]) -> Result<T, String> {
    let symbol = GetProcAddress(module, name.as_ptr().cast());
    if symbol.is_null() {
        return Err(format!(
            "missing symbol {}",
            String::from_utf8_lossy(&name[..name.len() - 1])
        ));
    }
    Ok(mem::transmute_copy(&symbol))
}

fn load_system_conpty() -> Result<ConPtyFuncs, String> {
    let kernel_name = wide_string(OsStr::new("kernel32.dll"));
    let module = unsafe { GetModuleHandleW(kernel_name.as_ptr()) };
    unsafe { ConPtyFuncs::from_module(module, false) }
}

fn load_app_local_conpty(path: &Path) -> Result<ConPtyFuncs, String> {
    let wide_path = wide_string(path.as_os_str());
    let module = unsafe {
        LoadLibraryExW(
            wide_path.as_ptr(),
            ptr::null_mut(),
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
        )
    };
    match unsafe { ConPtyFuncs::from_module(module, true) } {
        Ok(functions) => Ok(functions),
        Err(error) => {
            if !module.is_null() {
                unsafe { FreeLibrary(module) };
            }
            Err(error)
        }
    }
}

fn wide_string(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(Some(0)).collect()
}

fn load_conpty() -> ConPtyFuncs {
    // If the kernel doesn't export these functions then their system is
    // too old and we cannot run.
    let system = load_system_conpty().unwrap_or_else(|error| {
        panic!(
            "this system does not support conpty. Windows 10 October 2018 or newer is required: {}",
            error
        )
    });

    if std::env::var("HERDR_WINDOWS_CONPTY").as_deref() == Ok("system") {
        log::warn!("HERDR_WINDOWS_CONPTY=system disables Herdr's bundled ConPTY");
        return system;
    }

    match app_local_conpty_path() {
        Ok(Some(path)) => load_app_local_conpty(&path).unwrap_or_else(|error| {
            panic!(
                "failed to load verified app-local ConPTY from {}: {error}; set HERDR_WINDOWS_CONPTY=system to use the Windows system ConPTY",
                path.display()
            )
        }),
        Ok(None) => system,
        Err(error) => panic!(
            "Herdr's app-local ConPTY bundle is invalid: {}; reinstall Herdr or set HERDR_WINDOWS_CONPTY=system to use the Windows system ConPTY",
            error
        ),
    }
}

const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
const CONPTY_FILES: &[(&str, &str)] = &[
    (
        "conpty.dll",
        "39fba2713e2495117b1591ae8c32a3b904bea7aa66069cf7815e2844c76d75d8",
    ),
    (
        "x64/OpenConsole.exe",
        "b7fd936c2668b87b9ecf7b3366dc6568afc1c6f981874cba3e955a1c35cf8160",
    ),
    (
        "arm64/OpenConsole.exe",
        "ed7622fd0d3bedc9ab9f122f5e58edf0def9e7999224f52dd395ba9f54edbe09",
    ),
];

fn app_local_conpty_path() -> Result<Option<PathBuf>, String> {
    if std::env::consts::ARCH != "x86_64" {
        return Ok(None);
    }
    let executable = std::env::current_exe()
        .map_err(|error| format!("cannot locate the Herdr executable: {error}"))?;
    let executable_dir = executable
        .parent()
        .ok_or_else(|| "Herdr executable has no parent directory".to_string())?;
    let bundle = executable_dir.join("conpty");
    if !bundle.exists() {
        return Ok(None);
    }
    reject_reparse_point(&bundle)?;

    let mut expected = BTreeSet::from(["herdr-conpty.json".to_string()]);
    for (relative, expected_hash) in CONPTY_FILES {
        expected.insert((*relative).to_string());
        verify_bundle_file(&bundle, relative, expected_hash)?;
    }
    let marker = bundle.join("herdr-conpty.json");
    verify_regular_file(&marker)?;

    let actual = bundle_files(&bundle)?;
    if actual != expected {
        return Err(format!(
            "unexpected bundle layout; expected {expected:?}, found {actual:?}"
        ));
    }
    Ok(Some(bundle.join("conpty.dll")))
}

fn verify_bundle_file(root: &Path, relative: &str, expected_hash: &str) -> Result<(), String> {
    let path = root.join(relative);
    verify_regular_file(&path)?;
    let mut file = File::open(&path).map_err(|error| format!("cannot open {}: {error}", path.display()))?;
    let mut digest = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    let actual_hash = format!("{:x}", digest.finalize());
    if actual_hash != expected_hash {
        return Err(format!(
            "{} has hash {actual_hash}, expected {expected_hash}",
            path.display()
        ));
    }
    Ok(())
}

fn verify_regular_file(path: &Path) -> Result<(), String> {
    let metadata = path
        .symlink_metadata()
        .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
    if !metadata.is_file() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(format!("{} is not a regular file", path.display()));
    }
    Ok(())
}

fn reject_reparse_point(path: &Path) -> Result<(), String> {
    let metadata = path
        .symlink_metadata()
        .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
    if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(format!("{} is not a regular directory", path.display()));
    }
    Ok(())
}

fn bundle_files(root: &Path) -> Result<BTreeSet<String>, String> {
    fn visit(root: &Path, directory: &Path, files: &mut BTreeSet<String>) -> Result<(), String> {
        reject_reparse_point(directory)?;
        for entry in std::fs::read_dir(directory)
            .map_err(|error| format!("cannot read {}: {error}", directory.display()))?
        {
            let entry = entry.map_err(|error| error.to_string())?;
            let path = entry.path();
            let metadata = path
                .symlink_metadata()
                .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
            if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                return Err(format!("{} is a reparse point", path.display()));
            }
            if metadata.is_dir() {
                visit(root, &path, files)?;
            } else if metadata.is_file() {
                let relative = path
                    .strip_prefix(root)
                    .map_err(|error| error.to_string())?
                    .to_string_lossy()
                    .replace('\\', "/");
                files.insert(relative);
            } else {
                return Err(format!("{} is not a regular file", path.display()));
            }
        }
        Ok(())
    }

    let mut files = BTreeSet::new();
    visit(root, root, &mut files)?;
    Ok(files)
}

lazy_static! {
    static ref CONPTY: ConPtyFuncs = load_conpty();
}

pub struct PsuedoCon {
    con: HPCON,
}

unsafe impl Send for PsuedoCon {}
unsafe impl Sync for PsuedoCon {}

impl Drop for PsuedoCon {
    fn drop(&mut self) {
        unsafe { (CONPTY.ClosePseudoConsole)(self.con) };
    }
}

impl PsuedoCon {
    pub fn new(size: COORD, input: FileDescriptor, output: FileDescriptor) -> Result<Self, Error> {
        let mut con: HPCON = INVALID_HANDLE_VALUE;
        let result = unsafe {
            (CONPTY.CreatePseudoConsole)(
                size,
                input.as_raw_handle() as _,
                output.as_raw_handle() as _,
                PSUEDOCONSOLE_INHERIT_CURSOR
                    | PSEUDOCONSOLE_RESIZE_QUIRK
                    | PSEUDOCONSOLE_WIN32_INPUT_MODE,
                &mut con,
            )
        };
        ensure!(
            result == S_OK,
            "failed to create psuedo console: HRESULT {}",
            result
        );
        Ok(Self { con })
    }

    pub fn resize(&self, size: COORD) -> Result<(), Error> {
        let result = unsafe { (CONPTY.ResizePseudoConsole)(self.con, size) };
        ensure!(
            result == S_OK,
            "failed to resize console to {}x{}: HRESULT: {}",
            size.X,
            size.Y,
            result
        );
        Ok(())
    }

    pub fn spawn_command(&self, cmd: CommandBuilder) -> anyhow::Result<WinChild> {
        let mut si: STARTUPINFOEXW = unsafe { mem::zeroed() };
        si.StartupInfo.cb = mem::size_of::<STARTUPINFOEXW>() as u32;
        // Explicitly set the stdio handles as invalid handles otherwise
        // we can end up with a weird state where the spawned process can
        // inherit the explicitly redirected output handles from its parent.
        // For example, when daemonizing wezterm-mux-server, the stdio handles
        // are redirected to a log file and the spawned process would end up
        // writing its output there instead of to the pty we just created.
        si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        si.StartupInfo.hStdInput = INVALID_HANDLE_VALUE;
        si.StartupInfo.hStdOutput = INVALID_HANDLE_VALUE;
        si.StartupInfo.hStdError = INVALID_HANDLE_VALUE;

        let mut attrs = ProcThreadAttributeList::with_capacity(1)?;
        attrs.set_pty(self.con)?;
        si.lpAttributeList = attrs.as_mut_ptr();

        let mut pi: PROCESS_INFORMATION = unsafe { mem::zeroed() };

        let (mut exe, mut cmdline) = cmd.cmdline()?;
        let cmd_os = OsString::from_wide(&cmdline);

        let cwd = cmd.current_directory();

        let res = unsafe {
            CreateProcessW(
                exe.as_mut_slice().as_mut_ptr(),
                cmdline.as_mut_slice().as_mut_ptr(),
                ptr::null_mut(),
                ptr::null_mut(),
                0,
                EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
                cmd.environment_block().as_mut_slice().as_mut_ptr() as *mut _,
                cwd.as_ref()
                    .map(|c| c.as_slice().as_ptr())
                    .unwrap_or(ptr::null()),
                &mut si.StartupInfo,
                &mut pi,
            )
        };
        if res == 0 {
            let err = IoError::last_os_error();
            let msg = format!(
                "CreateProcessW `{:?}` in cwd `{:?}` failed: {}",
                cmd_os,
                cwd.as_ref().map(|c| OsString::from_wide(c)),
                err
            );
            log::error!("{}", msg);
            bail!("{}", msg);
        }

        // Make sure we close out the thread handle so we don't leak it;
        // we do this simply by making it owned
        let _main_thread = unsafe { OwnedHandle::from_raw_handle(pi.hThread as _) };
        let proc = unsafe { OwnedHandle::from_raw_handle(pi.hProcess as _) };

        Ok(WinChild {
            proc: Mutex::new(proc),
        })
    }
}
