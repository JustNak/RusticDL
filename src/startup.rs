use crate::branding::APP_NAME;

/// Command-line flag written into the startup entry when "start minimized" is on.
pub const MINIMIZED_ARG: &str = "--minimized";

/// Sync the OS autostart entry with the current settings.
///
/// On non-Windows platforms this is a no-op success.
pub fn apply_launch_at_startup(enabled: bool, start_minimized: bool) -> Result<(), String> {
    #[cfg(windows)]
    {
        windows_impl::apply(enabled, start_minimized)
    }
    #[cfg(not(windows))]
    {
        let _ = (enabled, start_minimized);
        Ok(())
    }
}

/// Whether the process was launched with the minimized / tray-start flag.
pub fn launched_minimized() -> bool {
    std::env::args().any(|arg| arg == MINIMIZED_ARG)
}

#[cfg(windows)]
mod windows_impl {
    use super::{APP_NAME, MINIMIZED_ARG};
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegSetValueExW, HKEY_CURRENT_USER, KEY_WRITE,
        REG_SZ,
    };

    const RUN_SUBKEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

    pub(super) fn apply(enabled: bool, start_minimized: bool) -> Result<(), String> {
        if enabled {
            set_run_value(start_minimized)
        } else {
            remove_run_value()
        }
    }

    fn set_run_value(start_minimized: bool) -> Result<(), String> {
        let exe =
            std::env::current_exe().map_err(|e| format!("Could not resolve exe path: {e}"))?;
        let exe_str = exe.to_string_lossy();
        let command = if start_minimized {
            format!("\"{exe_str}\" {MINIMIZED_ARG}")
        } else {
            format!("\"{exe_str}\"")
        };

        let subkey = wide_null(RUN_SUBKEY);
        let value_name = wide_null(APP_NAME);
        let data = wide_null(&command);
        // REG_SZ payload is the wide string including the terminating NUL, as bytes.
        let data_bytes: &[u8] =
            unsafe { std::slice::from_raw_parts(data.as_ptr() as *const u8, data.len() * 2) };

        unsafe {
            let mut hkey = Default::default();
            let status = RegOpenKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(subkey.as_ptr()),
                None,
                KEY_WRITE,
                &mut hkey,
            );
            if status != ERROR_SUCCESS {
                return Err(format!(
                    "Could not open Startup registry key (error {}).",
                    status.0
                ));
            }
            let set = RegSetValueExW(
                hkey,
                PCWSTR(value_name.as_ptr()),
                None,
                REG_SZ,
                Some(data_bytes),
            );
            let _ = RegCloseKey(hkey);
            if set != ERROR_SUCCESS {
                return Err(format!(
                    "Could not write Startup registry value (error {}).",
                    set.0
                ));
            }
        }
        Ok(())
    }

    fn remove_run_value() -> Result<(), String> {
        let subkey = wide_null(RUN_SUBKEY);
        let value_name = wide_null(APP_NAME);
        unsafe {
            let mut hkey = Default::default();
            let status = RegOpenKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(subkey.as_ptr()),
                None,
                KEY_WRITE,
                &mut hkey,
            );
            if status != ERROR_SUCCESS {
                return Ok(());
            }
            let _ = RegDeleteValueW(hkey, PCWSTR(value_name.as_ptr()));
            let _ = RegCloseKey(hkey);
        }
        Ok(())
    }

    fn wide_null(s: &str) -> Vec<u16> {
        OsStr::new(s)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }
}
