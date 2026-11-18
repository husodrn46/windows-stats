//! Fixed, removable and optical drives with label, file system, free and total space.

use crate::util::{pcwstr, wide};
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::Storage::FileSystem::{CreateFileW, GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW, QueryDosDeviceW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};
use windows::Win32::System::Ioctl::{IOCTL_STORAGE_GET_DEVICE_NUMBER, STORAGE_DEVICE_NUMBER};
use windows::Win32::System::IO::DeviceIoControl;

const DRIVE_REMOVABLE: u32 = 2;
const DRIVE_FIXED: u32 = 3;
const DRIVE_CDROM: u32 = 5;
/// STORAGE_DEVICE_NUMBER.DeviceType of a volume on a disk (FILE_DEVICE_DISK)
const FILE_DEVICE_DISK: u32 = 7;

#[derive(Clone, Debug)]
pub struct Drive {
    /// "C:" locally, a mount point ("/", "/home") on remote machines
    pub name: String,
    pub label: String,
    /// "" for fixed disks, "USB" for removable, "DVD" for optical
    pub kind: &'static str,
    /// file system name as Windows reports it: NTFS, FAT32, exFAT, UDF, CDFS
    pub fs: String,
    /// the volume cannot be written to (optical media, write-protected cards)
    pub readonly: bool,
    pub free: u64,
    pub total: u64,
}

const FILE_READ_ONLY_VOLUME: u32 = 0x0008_0000;

/// The drive letter maps to `\Device\Floppy<n>` (QueryDosDevice reads the object manager only,
/// the hardware stays idle).
fn is_floppy(letter: char) -> bool {
    let name = wide(&format!("{letter}:"));
    let mut target = [0u16; 256];
    let n = unsafe { QueryDosDeviceW(pcwstr(&name), Some(&mut target)) } as usize;
    if n == 0 {
        // no answer: the classic floppy letters are still not worth a spin-up
        return letter == 'A' || letter == 'B';
    }
    String::from_utf16_lossy(&target[..n.min(target.len())]).to_ascii_lowercase().contains("\\device\\floppy")
}

/// A drive letter Windows calls fixed but that is not a volume on a disk: cloud drives such as
/// Google Drive ("G:") report themselves as a disk file system, and their "free space" is the
/// cloud quota or the cache. Unknown when the query fails (counted as a disk).
fn virtual_drive(letter: char) -> bool {
    let path = wide(&format!("\\\\.\\{letter}:"));
    unsafe {
        let Ok(h) = CreateFileW(pcwstr(&path), 0, FILE_SHARE_READ | FILE_SHARE_WRITE, None, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, None) else { return false };
        let mut n = STORAGE_DEVICE_NUMBER::default();
        let mut got = 0u32;
        let ok = DeviceIoControl(
            h,
            IOCTL_STORAGE_GET_DEVICE_NUMBER,
            None,
            0,
            Some(&mut n as *mut _ as *mut _),
            std::mem::size_of::<STORAGE_DEVICE_NUMBER>() as u32,
            Some(&mut got),
            None,
        )
        .is_ok();
        let _ = CloseHandle(h);
        ok && n.DeviceType != FILE_DEVICE_DISK
    }
}

/// The local fixed drive with the least free space, for the disk-space alert. Removable and
/// optical drives are not touched; volumes under 1 GB (small vendor partitions) and cloud drives
/// (see `virtual_drive`) are skipped.
pub fn lowest_fixed() -> Option<Drive> {
    let mask = unsafe { GetLogicalDrives() };
    let mut best: Option<Drive> = None;
    for i in 0..26 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let letter = (b'A' + i as u8) as char;
        let root = wide(&format!("{letter}:\\"));
        unsafe {
            if GetDriveTypeW(pcwstr(&root)) != DRIVE_FIXED || virtual_drive(letter) {
                continue;
            }
            let mut free = 0u64;
            let mut total = 0u64;
            if GetDiskFreeSpaceExW(pcwstr(&root), None, Some(&mut total), Some(&mut free)).is_err() || total < 1 << 30 {
                continue;
            }
            if best.as_ref().map_or(true, |b| free < b.free) {
                best = Some(Drive { name: format!("{letter}:"), label: String::new(), kind: "", fs: String::new(), readonly: false, free, total });
            }
        }
    }
    best
}

pub fn list() -> Vec<Drive> {
    let mask = unsafe { GetLogicalDrives() };
    let mut out = Vec::new();
    for i in 0..26 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let letter = (b'A' + i as u8) as char;
        let root = wide(&format!("{letter}:\\"));
        unsafe {
            let kind = match GetDriveTypeW(pcwstr(&root)) {
                DRIVE_FIXED => "",
                // floppy drives are "removable" too; asking them for free space spins the motor
                // every second, so they are recognised by their device name and skipped without
                // touching them
                DRIVE_REMOVABLE if is_floppy(letter) => continue,
                DRIVE_REMOVABLE => "USB",
                DRIVE_CDROM => "DVD",
                _ => continue,
            };
            let mut free = 0u64;
            let mut total = 0u64;
            // fails for an empty card reader / optical drive: skip those
            if GetDiskFreeSpaceExW(pcwstr(&root), None, Some(&mut total), Some(&mut free)).is_err() || total == 0 {
                continue;
            }
            let mut name = [0u16; 64];
            let mut fs_name = [0u16; 32];
            let mut flags = 0u32;
            let _ = GetVolumeInformationW(pcwstr(&root), Some(&mut name), None, None, Some(&mut flags), Some(&mut fs_name));
            let label = String::from_utf16_lossy(&name).trim_end_matches('\0').to_string();
            let fs = String::from_utf16_lossy(&fs_name).trim_end_matches('\0').to_string();
            let readonly = kind == "DVD" || flags & FILE_READ_ONLY_VOLUME != 0;
            out.push(Drive { name: format!("{letter}:"), label, kind, fs, readonly, free, total });
        }
    }
    out
}
