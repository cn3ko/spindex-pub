//! Removable-drive detection: Win32 enumeration, a pure set-diff, and a
//! 2-second polling loop (started in lib.rs) that emits `device:arrived` /
//! `device:removed` events. Drive-letter changes present as a new drive —
//! known limitation, see the milestone 3 spec.

use std::collections::BTreeMap;
use std::path::Path;
use crate::core::metadata;

pub const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Device {
    /// Drive letter with colon, e.g. "E:".
    pub letter: String,
    /// Volume label, or "Removable drive" when unlabeled.
    pub label: String,
    /// Volume serial number as 8 uppercase hex digits — the identity of the
    /// physical stick, stable across drive-letter changes. None when Windows
    /// couldn't read the volume information.
    pub serial: Option<String>,
}

/// Snapshot of removable drives, keyed by letter for stable ordering.
pub type DriveSet = BTreeMap<String, Device>;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct DriveDiff {
    pub arrived: Vec<Device>,
    /// Full devices (from the previous snapshot) so the watcher can still
    /// look libraries up by volume serial after the drive is gone.
    pub removed: Vec<Device>,
}

/// Pure diff between two polls — the only logic worth testing here.
pub fn diff_drives(prev: &DriveSet, cur: &DriveSet) -> DriveDiff {
    DriveDiff {
        arrived: cur
            .values()
            .filter(|d| !prev.contains_key(&d.letter))
            .cloned()
            .collect(),
        removed: prev
            .values()
            .filter(|d| !cur.contains_key(&d.letter))
            .cloned()
            .collect(),
    }
}

#[cfg(windows)]
pub fn enumerate_removable() -> DriveSet {
    use windows_sys::Win32::Storage::FileSystem::{
        GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
    };
    // https://learn.microsoft.com/windows/win32/api/fileapi/nf-fileapi-getdrivetypew
    const DRIVE_REMOVABLE: u32 = 2;

    let mask = unsafe { GetLogicalDrives() };
    let mut out = DriveSet::new();
    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let letter = (b'A' + i as u8) as char;
        let root: Vec<u16> = format!("{letter}:\\").encode_utf16().chain([0]).collect();
        if unsafe { GetDriveTypeW(root.as_ptr()) } != DRIVE_REMOVABLE {
            continue;
        }
        let mut name = [0u16; 261];
        let mut serial_raw: u32 = 0;
        let ok = unsafe {
            GetVolumeInformationW(
                root.as_ptr(),
                name.as_mut_ptr(),
                name.len() as u32,
                &mut serial_raw,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                0,
            )
        };
        let serial = (ok != 0 && serial_raw != 0).then(|| format!("{serial_raw:08X}"));
        let label = if ok != 0 {
            let len = name.iter().position(|&c| c == 0).unwrap_or(0);
            let s = String::from_utf16_lossy(&name[..len]).trim().to_string();
            if s.is_empty() {
                "Removable drive".to_string()
            } else {
                s
            }
        } else {
            "Removable drive".to_string()
        };
        out.insert(
            format!("{letter}:"),
            Device {
                letter: format!("{letter}:"),
                label,
                serial,
            },
        );
    }
    out
}

#[cfg(not(windows))]
pub fn enumerate_removable() -> DriveSet {
    DriveSet::new() // macOS/Linux enumeration comes with those ports
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct AudioFolders {
    /// Top-level directory names containing at least one audio file below, sorted.
    pub folders: Vec<String>,
    /// True when audio files sit directly in the root itself.
    pub root_has_audio: bool,
}

/// The spec's tree rule: each top-level folder of a stick that contains
/// audio anywhere below it becomes its own library; loose root files get
/// one extra root library. Unreadable entries are skipped silently
/// (consistent with the scanner).
pub fn audio_folders(root: &Path) -> AudioFolders {
    let mut out = AudioFolders::default();
    let Ok(entries) = std::fs::read_dir(root) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.file_name().and_then(|n| n.to_str()) == Some(crate::core::enhance::BACKUP_DIR) {
            continue;
        }
        if path.is_dir() {
            let has_audio = walkdir::WalkDir::new(&path)
                .into_iter()
                .flatten()
                .filter(|e| {
                    !e.path()
                        .components()
                        .any(|c| c.as_os_str() == crate::core::enhance::BACKUP_DIR)
                })
                .any(|e| e.file_type().is_file() && metadata::is_audio_file(e.path()));
            if has_audio {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    out.folders.push(name.to_string());
                }
            }
        } else if metadata::is_audio_file(&path) {
            out.root_has_audio = true;
        }
    }
    out.folders.sort();
    out
}

/// Safely ejects a removable drive ("E:"): lock the volume (retrying while
/// something still holds a file open), dismount it, allow removal, eject.
/// Fails with a clear error when the drive is busy — e.g. a track on it is
/// still playing.
#[cfg(windows)]
pub fn eject_drive(letter: &str) -> anyhow::Result<()> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::IO::DeviceIoControl;

    const GENERIC_READ: u32 = 0x8000_0000;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const FSCTL_LOCK_VOLUME: u32 = 0x0009_0018;
    const FSCTL_DISMOUNT_VOLUME: u32 = 0x0009_0020;
    const IOCTL_STORAGE_MEDIA_REMOVAL: u32 = 0x002D_4804;
    const IOCTL_STORAGE_EJECT_MEDIA: u32 = 0x002D_4808;

    let path: Vec<u16> = format!("\\\\.\\{letter}").encode_utf16().chain([0]).collect();
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        anyhow::bail!("could not open drive {letter}");
    }
    let ioctl = |code: u32, input: &[u8]| -> bool {
        let mut returned = 0u32;
        unsafe {
            DeviceIoControl(
                handle,
                code,
                input.as_ptr().cast(),
                input.len() as u32,
                std::ptr::null_mut(),
                0,
                &mut returned,
                std::ptr::null_mut(),
            ) != 0
        }
    };
    let result = (|| {
        let mut locked = false;
        for _ in 0..5 {
            if ioctl(FSCTL_LOCK_VOLUME, &[]) {
                locked = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
        if !locked {
            anyhow::bail!("drive {letter} is busy — stop playback or close open files first");
        }
        if !ioctl(FSCTL_DISMOUNT_VOLUME, &[]) {
            anyhow::bail!("could not dismount drive {letter}");
        }
        // PREVENT_MEDIA_REMOVAL { PreventMediaRemoval: FALSE }
        let _ = ioctl(IOCTL_STORAGE_MEDIA_REMOVAL, &[0u8]);
        if !ioctl(IOCTL_STORAGE_EJECT_MEDIA, &[]) {
            anyhow::bail!("could not eject drive {letter}");
        }
        Ok(())
    })();
    unsafe { CloseHandle(handle) };
    result
}

#[cfg(not(windows))]
pub fn eject_drive(_letter: &str) -> anyhow::Result<()> {
    anyhow::bail!("eject is only implemented on Windows")
}

/// Bytes free for the current user on drive `letter` ("D:"); None when the
/// drive cannot be asked.
#[cfg(windows)]
pub fn free_space(letter: &str) -> Option<u64> {
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let root: Vec<u16> = format!("{letter}\\").encode_utf16().chain(std::iter::once(0)).collect();
    let mut free: u64 = 0;
    // SAFETY: `root` is NUL-terminated and outlives the call; the two optional
    // out-parameters are null, which the API allows.
    let ok = unsafe { GetDiskFreeSpaceExW(root.as_ptr(), &mut free, std::ptr::null_mut(), std::ptr::null_mut()) };
    (ok != 0).then_some(free)
}

#[cfg(not(windows))]
pub fn free_space(_letter: &str) -> Option<u64> {
    None
}

/// Rewrites a library root for a stick that reappeared under a new drive
/// letter, preserving the per-folder suffix ("D:\MP3\" -> "E:\MP3\").
/// Parked placeholder roots ("missing://<id><suffix>", written by
/// `db::park_conflicting_removable`) un-park: the digits after the prefix
/// are skipped and the remaining folder suffix reattaches to the new
/// letter ("missing://7\MP3\" -> "E:\MP3\"); legacy parked rows with no
/// suffix (pre-fix) restore to just the bare drive root ("missing://7" ->
/// "E:\"). Anything else passes through unchanged.
pub fn swap_drive_prefix(old_root: &str, new_letter: &str) -> String {
    let bytes = old_root.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        format!("{new_letter}{}", &old_root[2..])
    } else if let Some(rest) = old_root.strip_prefix("missing://") {
        let suffix_start = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
        let suffix = &rest[suffix_start..];
        if suffix.is_empty() {
            format!("{new_letter}\\")
        } else {
            format!("{new_letter}{suffix}")
        }
    } else {
        old_root.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(devices: &[(&str, &str)]) -> DriveSet {
        devices
            .iter()
            .map(|(l, n)| {
                (
                    l.to_string(),
                    Device {
                        letter: l.to_string(),
                        label: n.to_string(),
                        serial: Some(format!("{:08X}", l.as_bytes()[0] as u32)),
                    },
                )
            })
            .collect()
    }

    #[test]
    fn diff_detects_arrival_removal_and_stability() {
        let prev = set(&[("E:", "KINGSTON")]);
        let cur = set(&[("E:", "KINGSTON"), ("F:", "SANDISK")]);

        let d = diff_drives(&prev, &cur);
        assert_eq!(d.arrived.len(), 1);
        assert_eq!(d.arrived[0].letter, "F:");
        assert!(d.removed.is_empty());

        let d = diff_drives(&cur, &prev);
        assert!(d.arrived.is_empty());
        // removal carries the full device so the watcher can look the
        // library up by volume serial after the drive is already gone
        assert_eq!(d.removed.len(), 1);
        assert_eq!(d.removed[0].letter, "F:");

        let d = diff_drives(&cur, &cur);
        assert_eq!(d, DriveDiff::default(), "no change, no events");

        let d = diff_drives(&DriveSet::new(), &DriveSet::new());
        assert_eq!(d, DriveDiff::default());
    }

    #[test]
    fn simultaneous_swap_reports_both() {
        let prev = set(&[("E:", "OLD")]);
        let cur = set(&[("F:", "NEW")]);
        let d = diff_drives(&prev, &cur);
        assert_eq!(d.arrived[0].letter, "F:");
        assert_eq!(d.removed.len(), 1);
        assert_eq!(d.removed[0].letter, "E:");
    }

    #[test]
    fn swap_drive_prefix_keeps_folder_suffix() {
        assert_eq!(swap_drive_prefix("D:\\MP3\\", "E:"), "E:\\MP3\\");
        assert_eq!(swap_drive_prefix("D:\\", "E:"), "E:\\");
        // parked placeholder roots un-park with their folder suffix restored
        assert_eq!(swap_drive_prefix("missing://7\\MP3\\", "E:"), "E:\\MP3\\");
        assert_eq!(swap_drive_prefix("missing://7\\", "E:"), "E:\\");
        assert_eq!(swap_drive_prefix("missing://7", "E:"), "E:\\"); // legacy parked row, suffix lost pre-fix
    }

    #[test]
    fn audio_folders_finds_top_level_folders_with_audio() {
        let dir = std::env::temp_dir().join(format!("spindex-devtest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // MP3/a.mp3 (direct), WAV/sub/b.wav (nested), _Serato_/db.bin (no audio), Empty/
        std::fs::create_dir_all(dir.join("MP3")).unwrap();
        std::fs::write(dir.join("MP3").join("a.mp3"), b"x").unwrap();
        std::fs::create_dir_all(dir.join("WAV").join("sub")).unwrap();
        std::fs::write(dir.join("WAV").join("sub").join("b.wav"), b"x").unwrap();
        std::fs::create_dir_all(dir.join("_Serato_")).unwrap();
        std::fs::write(dir.join("_Serato_").join("db.bin"), b"x").unwrap();
        std::fs::create_dir_all(dir.join("Empty")).unwrap();

        let af = audio_folders(&dir);
        assert_eq!(af.folders, vec!["MP3".to_string(), "WAV".to_string()]);
        assert!(!af.root_has_audio);

        // loose audio in the root flips root_has_audio
        std::fs::write(dir.join("loose.flac"), b"x").unwrap();
        assert!(audio_folders(&dir).root_has_audio);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn audio_folders_excludes_backup_dir() {
        let dir = std::env::temp_dir().join(format!("spindex-devtest-backup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("MP3")).unwrap();
        std::fs::write(dir.join("MP3").join("a.mp3"), b"x").unwrap();
        // a top-level backup dir must never surface as a library folder...
        std::fs::create_dir_all(dir.join(crate::core::enhance::BACKUP_DIR)).unwrap();
        std::fs::write(dir.join(crate::core::enhance::BACKUP_DIR).join("b.mp3"), b"x").unwrap();
        // ...and audio nested under a backup dir inside a real folder must not
        // count towards that folder's has_audio check either.
        std::fs::create_dir_all(dir.join("WAV").join(crate::core::enhance::BACKUP_DIR)).unwrap();
        std::fs::write(
            dir.join("WAV").join(crate::core::enhance::BACKUP_DIR).join("c.wav"),
            b"x",
        )
        .unwrap();

        let af = audio_folders(&dir);
        assert_eq!(af.folders, vec!["MP3".to_string()]);
        assert!(!af.root_has_audio);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
