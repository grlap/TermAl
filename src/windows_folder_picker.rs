//! Native Windows project-folder selection, called by paths.rs.
//!
//! Owns the COM apartment and dialog lifetime, not project creation or path
//! admission. A dedicated STA thread avoids inheriting a Tokio worker's COM
//! mode. Only the selected path crosses threads; no shell or PowerShell runs.

use std::sync::atomic::{AtomicBool, Ordering};
use windows::Win32::Foundation::ERROR_CANCELLED;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoCreateInstance,
    CoInitializeEx, CoTaskMemFree, CoUninitialize,
};
use windows::Win32::System::Ole::IOleWindow;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext,
};
use windows::Win32::UI::Shell::{
    FDE_OVERWRITE_RESPONSE, FDE_SHAREVIOLATION_RESPONSE, FDEOR_DEFAULT, FDESVR_DEFAULT,
    FOS_FORCEFILESYSTEM, FOS_NOCHANGEDIR, FOS_PATHMUSTEXIST, FOS_PICKFOLDERS, FileOpenDialog,
    IFileDialog, IFileDialogEvents, IFileDialogEvents_Impl, IFileOpenDialog, IShellItem,
    SHCreateItemFromParsingName, SIGDN_FILESYSPATH,
};
use windows::Win32::UI::WindowsAndMessaging::{SW_RESTORE, SetForegroundWindow, ShowWindowAsync};
use windows::core::{HRESULT, HSTRING, Interface, Ref, Result as WindowsResult, implement, w};

pub(super) fn pick(default_workdir: &str) -> Result<Option<String>, String> {
    let default_workdir = default_workdir.to_owned();
    std::thread::Builder::new()
        .name("project-folder-picker".into())
        .spawn(move || pick_on_sta(&default_workdir).map_err(|err| err.to_string()))
        .map_err(|err| format!("could not start folder picker thread: {err}"))?
        .join()
        .map_err(|_| "folder picker thread panicked".to_owned())?
}

struct ComApartment;

/// Scope DPI awareness to this UI thread, not the headless host process. The
/// native dialog (including its child controls) must be created in PMv2 mode.
struct PickerDpiContext(DPI_AWARENESS_CONTEXT);

impl PickerDpiContext {
    fn enter() -> WindowsResult<Self> {
        // SAFETY: modifies only the calling thread; Drop restores that thread.
        let previous =
            unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        if previous.0.is_null() {
            return Err(windows::core::Error::from_thread());
        }
        Ok(Self(previous))
    }
}

impl Drop for PickerDpiContext {
    fn drop(&mut self) {
        // SAFETY: previous is a valid context returned on this same STA thread.
        unsafe { SetThreadDpiAwarenessContext(self.0) };
    }
}

#[implement(IFileDialogEvents)]
struct RestoreOnOpen {
    requested: AtomicBool,
}

impl RestoreOnOpen {
    fn request_once(&self, restore: impl FnOnce() -> bool) {
        if !self.requested.swap(true, Ordering::Relaxed) && !restore() {
            // No HWND yet / queue failed: the next folder event may retry.
            self.requested.store(false, Ordering::Relaxed);
        }
    }
}

impl IFileDialogEvents_Impl for RestoreOnOpen_Impl {
    fn OnFolderChange(&self, dialog: Ref<IFileDialog>) -> WindowsResult<()> {
        self.request_once(|| {
            let Some(dialog) = dialog.as_ref() else {
                return false;
            };
            let Ok(window) = dialog.cast::<IOleWindow>() else {
                return false;
            };
            // SAFETY: this callback runs on the dialog's STA and only targets
            // its own HWND. Defer restoration until Show's message loop runs:
            // the host's inherited hidden/minimized startup state must not win
            // after an early synchronous ShowWindow during initialization.
            unsafe {
                let Ok(hwnd) = window.GetWindow() else {
                    return false;
                };
                let queued = ShowWindowAsync(hwnd, SW_RESTORE).as_bool();
                if queued {
                    // Best effort under Windows' foreground-lock policy. Never
                    // steal input via AttachThreadInput or make it always-on-top.
                    let _ = SetForegroundWindow(hwnd);
                }
                queued
            }
        });
        Ok(())
    }

    fn OnFileOk(&self, _: Ref<IFileDialog>) -> WindowsResult<()> {
        Ok(())
    }
    fn OnFolderChanging(&self, _: Ref<IFileDialog>, _: Ref<IShellItem>) -> WindowsResult<()> {
        Ok(())
    }
    fn OnSelectionChange(&self, _: Ref<IFileDialog>) -> WindowsResult<()> {
        Ok(())
    }
    fn OnTypeChange(&self, _: Ref<IFileDialog>) -> WindowsResult<()> {
        Ok(())
    }
    fn OnShareViolation(
        &self,
        _: Ref<IFileDialog>,
        _: Ref<IShellItem>,
    ) -> WindowsResult<FDE_SHAREVIOLATION_RESPONSE> {
        Ok(FDESVR_DEFAULT)
    }
    fn OnOverwrite(
        &self,
        _: Ref<IFileDialog>,
        _: Ref<IShellItem>,
    ) -> WindowsResult<FDE_OVERWRITE_RESPONSE> {
        Ok(FDEOR_DEFAULT)
    }
}

struct DialogEventSubscription {
    dialog: IFileOpenDialog,
    cookie: u32,
}

impl DialogEventSubscription {
    fn attach(dialog: &IFileOpenDialog) -> WindowsResult<Self> {
        let events: IFileDialogEvents = RestoreOnOpen {
            requested: AtomicBool::new(false),
        }
        .into();
        // SAFETY: COM retains the sink until Unadvise; both live on this STA.
        let cookie = unsafe { dialog.Advise(&events)? };
        Ok(Self {
            dialog: dialog.clone(),
            cookie,
        })
    }
}

impl Drop for DialogEventSubscription {
    fn drop(&mut self) {
        // SAFETY: remove the sink before releasing the dialog/apartment, also
        // on cancel/error. No cycle: the sink stores no COM interface.
        unsafe {
            let _ = self.dialog.Unadvise(self.cookie);
        }
    }
}

impl ComApartment {
    fn initialize() -> WindowsResult<Self> {
        // SAFETY: this dedicated thread owns all COM objects until they drop.
        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE).ok()?;
        }
        Ok(Self)
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        // SAFETY: balances successful initialization (including S_FALSE), after
        // the dialog and shell items have been released on this same thread.
        unsafe { CoUninitialize() };
    }
}

fn create_dialog(default_workdir: &str) -> WindowsResult<IFileOpenDialog> {
    // SAFETY: callers hold a live STA apartment; COM wrappers own/release their
    // interfaces and all string arguments remain alive for each synchronous call.
    unsafe {
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
        dialog.SetOptions(
            dialog.GetOptions()?
                | FOS_PICKFOLDERS
                | FOS_FORCEFILESYSTEM
                | FOS_PATHMUSTEXIST
                | FOS_NOCHANGEDIR,
        )?;
        dialog.SetTitle(w!("Choose a folder for this project"))?;
        // A deleted/unavailable starting directory must not disable the picker.
        if std::path::Path::new(default_workdir).is_dir() {
            let folder: IShellItem =
                SHCreateItemFromParsingName(&HSTRING::from(default_workdir), None)?;
            dialog.SetFolder(&folder)?;
        }
        Ok(dialog)
    }
}

fn dialog_accepted(result: WindowsResult<()>) -> WindowsResult<bool> {
    match result {
        Ok(()) => Ok(true),
        Err(err) if err.code() == HRESULT::from_win32(ERROR_CANCELLED.0) => Ok(false),
        Err(err) => Err(err),
    }
}

fn filesystem_path(item: &IShellItem) -> WindowsResult<String> {
    // SAFETY: GetDisplayName returns a NUL-terminated COM allocation. Copy it
    // before freeing it, including when UTF-16 decoding fails. Never trim a
    // native path or pass it through shell/output encoding.
    unsafe {
        let path = item.GetDisplayName(SIGDN_FILESYSPATH)?;
        let result = path.to_string();
        CoTaskMemFree(Some(path.0.cast()));
        result.map_err(Into::into)
    }
}

fn pick_on_sta(default_workdir: &str) -> WindowsResult<Option<String>> {
    let _dpi = PickerDpiContext::enter()?;
    let _apartment = ComApartment::initialize()?;
    let dialog = create_dialog(default_workdir)?;
    let _events = DialogEventSubscription::attach(&dialog)?;
    // SAFETY: initialized STA; Show owns its modal message loop. TermAl's UI is
    // in a browser, so there is no application-owned HWND to use as the parent.
    if !dialog_accepted(unsafe { dialog.Show(None) })? {
        return Ok(None);
    }
    // SAFETY: a successful Show guarantees a selection; item drops before COM.
    let item = unsafe { dialog.GetResult()? };
    filesystem_path(&item).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Foundation::E_FAIL;
    use windows::Win32::UI::HiDpi::{AreDpiAwarenessContextsEqual, GetThreadDpiAwarenessContext};

    #[test]
    fn windows_folder_picker_restores_dpi_context_on_success_and_error() {
        std::thread::spawn(|| {
            // SAFETY: all context reads/writes happen on this dedicated thread.
            unsafe {
                let before = GetThreadDpiAwarenessContext();
                for fail in [false, true] {
                    let result: WindowsResult<()> = (|| {
                        let _dpi = PickerDpiContext::enter()?;
                        assert!(
                            AreDpiAwarenessContextsEqual(
                                GetThreadDpiAwarenessContext(),
                                DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2
                            )
                            .as_bool()
                        );
                        if fail {
                            return Err(E_FAIL.into());
                        }
                        Ok(())
                    })();
                    assert_eq!(result.is_err(), fail);
                    assert!(
                        AreDpiAwarenessContextsEqual(GetThreadDpiAwarenessContext(), before)
                            .as_bool()
                    );
                }
            }
        })
        .join()
        .unwrap();
    }

    #[test]
    fn windows_folder_picker_restores_only_once_not_on_later_navigation() {
        let events = RestoreOnOpen {
            requested: AtomicBool::new(false),
        };
        events.request_once(|| false);
        assert!(!events.requested.load(Ordering::Relaxed));
        events.request_once(|| true);
        assert!(events.requested.load(Ordering::Relaxed));
        events.request_once(|| panic!("later folder changes must not restore or steal focus"));
    }

    #[test]
    fn windows_folder_picker_distinguishes_cancel_from_failure() {
        assert!(dialog_accepted(Ok(())).unwrap());
        assert!(!dialog_accepted(Err(HRESULT::from_win32(ERROR_CANCELLED.0).into())).unwrap());
        assert_eq!(
            dialog_accepted(Err(E_FAIL.into())).unwrap_err().code(),
            E_FAIL
        );
    }

    #[test]
    fn windows_folder_picker_configures_native_dialog_and_unicode_path_without_showing() {
        let root = crate::TestTempRoot::create("windows-folder-picker");
        let folder = root.path().join("Zażółć 🦀 project ' & $()");
        std::fs::create_dir(&folder).unwrap();
        let folder_string = folder.to_str().unwrap().to_owned();
        std::thread::spawn(move || {
            let _dpi = PickerDpiContext::enter().unwrap();
            let _apartment = ComApartment::initialize().unwrap();
            let dialog = create_dialog(&folder_string).unwrap();
            let _events = DialogEventSubscription::attach(&dialog).unwrap();
            // SAFETY: this test owns an initialized STA; it never calls Show.
            unsafe {
                let options = dialog.GetOptions().unwrap();
                for flag in [
                    FOS_PICKFOLDERS,
                    FOS_FORCEFILESYSTEM,
                    FOS_PATHMUSTEXIST,
                    FOS_NOCHANGEDIR,
                ] {
                    assert_eq!(options & flag, flag);
                }
                let selected = filesystem_path(&dialog.GetFolder().unwrap()).unwrap();
                assert_eq!(
                    std::fs::canonicalize(selected).unwrap(),
                    std::fs::canonicalize(&folder_string).unwrap()
                );
                assert!(crate::resolve_project_root_path(&folder_string).is_ok());
                assert!(create_dialog(&format!("{folder_string}\\missing")).is_ok());
            }
        })
        .join()
        .unwrap();
    }
}
