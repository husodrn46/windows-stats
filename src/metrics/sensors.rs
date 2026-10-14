//! Built-in sensors that need neither administrator rights nor a driver:
//!
//! * GPU temperature and fan speed from the display driver (D3DKMT adapter performance data,
//!   what Task Manager shows for dedicated GPUs; 0 means the driver does not report it);
//! * disk temperature (StorageDeviceTemperatureProperty, NVMe and most SATA drives) and whether
//!   a disk is an SSD (StorageDeviceSeekPenaltyProperty), through a handle opened without
//!   read or write access.
//!
//! Anything a device does not report comes back as None and is shown as "not available on this
//! device", never as a made-up number.

use crate::util::{pcwstr, wide};
use windows::Wdk::Graphics::Direct3D::*;
use windows::Win32::Foundation::{CloseHandle, HANDLE, LUID};
use windows::Win32::Storage::FileSystem::{CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};
use windows::Win32::System::IO::DeviceIoControl;
use windows::Win32::System::Ioctl::*;

/// Readings of one GPU.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GpuSensor {
    pub luid: (u32, i32),
    pub temp_c: Option<f32>,
    pub fan_rpm: Option<u32>,
}

/// Every adapter the display driver reports performance data for.
pub fn gpus() -> Vec<GpuSensor> {
    let mut out = Vec::new();
    unsafe {
        let mut e = D3DKMT_ENUMADAPTERS2::default();
        if D3DKMTEnumAdapters2(&mut e).is_err() || e.NumAdapters == 0 {
            return out;
        }
        let mut list = vec![D3DKMT_ADAPTERINFO::default(); e.NumAdapters as usize];
        e.pAdapters = list.as_mut_ptr();
        if D3DKMTEnumAdapters2(&mut e).is_err() {
            return out;
        }
        list.truncate(e.NumAdapters as usize);
        for a in &list {
            let mut perf = D3DKMT_ADAPTER_PERFDATA { PhysicalAdapterIndex: 0, ..Default::default() };
            let mut q = D3DKMT_QUERYADAPTERINFO {
                hAdapter: a.hAdapter,
                Type: KMTQAITYPE_ADAPTERPERFDATA,
                pPrivateDriverData: &mut perf as *mut _ as *mut _,
                PrivateDriverDataSize: std::mem::size_of::<D3DKMT_ADAPTER_PERFDATA>() as u32,
            };
            if D3DKMTQueryAdapterInfo(&mut q).is_ok() {
                // tenths of a degree; 0 = not reported
                let temp = (perf.Temperature > 0 && perf.Temperature < 1500).then(|| perf.Temperature as f32 / 10.0);
                let fan = (perf.FanRPM > 0).then_some(perf.FanRPM);
                let LUID { LowPart, HighPart } = a.AdapterLuid;
                out.push(GpuSensor { luid: (LowPart, HighPart), temp_c: temp, fan_rpm: fan });
            }
            let mut c = D3DKMT_CLOSEADAPTER { hAdapter: a.hAdapter };
            let _ = D3DKMTCloseAdapter(&mut c);
        }
    }
    out
}

fn open_disk(index: u32) -> Option<HANDLE> {
    let path = wide(&format!("\\\\.\\PhysicalDrive{index}"));
    // no read / write access: the property queries below do not need it (and no admin rights)
    unsafe { CreateFileW(pcwstr(&path), 0, FILE_SHARE_READ | FILE_SHARE_WRITE, None, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, None).ok() }
}

fn property(h: HANDLE, id: STORAGE_PROPERTY_ID, out: &mut [u8]) -> Option<u32> {
    let q = STORAGE_PROPERTY_QUERY { PropertyId: id, QueryType: PropertyStandardQuery, AdditionalParameters: [0] };
    let mut got = 0u32;
    unsafe {
        DeviceIoControl(
            h,
            IOCTL_STORAGE_QUERY_PROPERTY,
            Some(&q as *const _ as *const _),
            std::mem::size_of::<STORAGE_PROPERTY_QUERY>() as u32,
            Some(out.as_mut_ptr() as *mut _),
            out.len() as u32,
            Some(&mut got),
            None,
        )
        .ok()?;
    }
    Some(got)
}

/// (temperature °C, SSD) of PhysicalDriveN, each None when the drive does not tell.
pub fn disk(index: u32) -> (Option<f32>, Option<bool>) {
    let Some(h) = open_disk(index) else { return (None, None) };
    let mut buf = vec![0u8; 512];
    let temp = property(h, StorageDeviceTemperatureProperty, &mut buf).and_then(|got| {
        if (got as usize) < std::mem::size_of::<STORAGE_TEMPERATURE_DATA_DESCRIPTOR>() {
            return None;
        }
        let d = unsafe { &*(buf.as_ptr() as *const STORAGE_TEMPERATURE_DATA_DESCRIPTOR) };
        if d.InfoCount == 0 {
            return None;
        }
        let t = d.TemperatureInfo[0].Temperature;
        // a few drives report 0 or nonsense instead of "unsupported"
        (t > 0 && t < 150).then_some(t as f32)
    });
    let mut sp = vec![0u8; std::mem::size_of::<DEVICE_SEEK_PENALTY_DESCRIPTOR>()];
    let ssd = property(h, StorageDeviceSeekPenaltyProperty, &mut sp).and_then(|got| {
        if (got as usize) < std::mem::size_of::<DEVICE_SEEK_PENALTY_DESCRIPTOR>() {
            return None;
        }
        let d = unsafe { &*(sp.as_ptr() as *const DEVICE_SEEK_PENALTY_DESCRIPTOR) };
        Some(!d.IncursSeekPenalty.as_bool())
    });
    unsafe {
        let _ = CloseHandle(h);
    }
    (temp, ssd)
}
