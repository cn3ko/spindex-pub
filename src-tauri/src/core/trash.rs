//! Recycle-Bin deletion via the Win32 shell. The ONLY place in SpinDex that
//! deletes audio files; reached exclusively from the user-confirmed Delete
//! action. On volumes without a Recycle Bin (USB sticks) the shell deletes
//! permanently — the frontend confirm dialog states this.

use std::path::Path;

use anyhow::{bail, Result};

/// The double-null-terminated UTF-16 list `SHFileOperationW` expects:
/// every path NUL-terminated, plus one extra NUL closing the list.
pub fn wide_path_list(paths: &[&Path]) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    let mut buf: Vec<u16> = Vec::new();
    for p in paths {
        buf.extend(p.as_os_str().encode_wide());
        buf.push(0);
    }
    buf.push(0);
    buf
}

/// Sends all files to the Recycle Bin in one shell call. Paths must be
/// absolute. The shell's return code is unreliable for partial failures —
/// callers must re-check `Path::exists` per file afterwards.
pub fn recycle(paths: &[&Path]) -> Result<()> {
    use windows_sys::Win32::UI::Shell::{
        SHFileOperationW, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT,
        FO_DELETE, SHFILEOPSTRUCTW,
    };

    if paths.is_empty() {
        return Ok(());
    }
    let list = wide_path_list(paths);
    let mut op = SHFILEOPSTRUCTW {
        hwnd: std::ptr::null_mut(),
        wFunc: FO_DELETE as u32,
        pFrom: list.as_ptr(),
        pTo: std::ptr::null(),
        fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_NOERRORUI | FOF_SILENT) as u16,
        fAnyOperationsAborted: 0,
        hNameMappings: std::ptr::null_mut(),
        lpszProgressTitle: std::ptr::null(),
    };
    let rc = unsafe { SHFileOperationW(&mut op) };
    if rc != 0 {
        bail!("shell delete failed (code 0x{rc:X})");
    }
    if op.fAnyOperationsAborted != 0 {
        bail!("delete was aborted");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn utf16(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn wide_list_empty_is_single_terminator() {
        assert_eq!(wide_path_list(&[]), vec![0]);
    }

    #[test]
    fn wide_list_one_path_double_terminated() {
        let p = Path::new(r"C:\Music\track one.mp3");
        let mut expected = utf16(r"C:\Music\track one.mp3");
        expected.push(0);
        expected.push(0);
        assert_eq!(wide_path_list(&[p]), expected);
    }

    #[test]
    fn wide_list_many_paths_nul_separated() {
        let a = Path::new(r"C:\a.mp3");
        let b = Path::new(r"D:\b [x].flac");
        let mut expected = utf16(r"C:\a.mp3");
        expected.push(0);
        expected.extend(utf16(r"D:\b [x].flac"));
        expected.push(0);
        expected.push(0);
        assert_eq!(wide_path_list(&[a, b]), expected);
    }
}
