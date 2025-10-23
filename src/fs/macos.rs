use std::{
    collections::{HashMap, HashSet},
    ffi::{CStr, CString},
    mem::MaybeUninit,
    os::unix::prelude::OsStrExt,
    path::PathBuf,
    ptr::{self, NonNull},
};

use crate::{DeviceId, DeviceKind, StorageDevice, StorageEvent, StorageVolume, VolumeId};
use anyhow::Context;
use libc::c_void;
use objc2_core_foundation::{
    CFBoolean, CFDictionary, CFNumber, CFRetained, CFRunLoop, CFString, CFURL, CFUUID, Type,
    kCFRunLoopDefaultMode,
};
use objc2_disk_arbitration::{
    DADisk, DARegisterDiskAppearedCallback, DARegisterDiskDescriptionChangedCallback,
    DARegisterDiskDisappearedCallback, DASession, kDADiskDescriptionDeviceInternalKey,
    kDADiskDescriptionDeviceModelKey, kDADiskDescriptionDevicePathKey,
    kDADiskDescriptionMediaEjectableKey, kDADiskDescriptionMediaNameKey,
    kDADiskDescriptionMediaRemovableKey, kDADiskDescriptionMediaSizeKey,
    kDADiskDescriptionMediaUUIDKey, kDADiskDescriptionMediaWritableKey,
    kDADiskDescriptionVolumeNameKey, kDADiskDescriptionVolumePathKey,
    kDADiskDescriptionVolumeUUIDKey, kDADiskDescriptionWatchVolumePath,
};
use tracing::trace;

struct CallbackContext {
    tx: flume::Sender<StorageEvent>,
}

pub fn get_devices() -> anyhow::Result<(Vec<StorageDevice>, Vec<StorageVolume>)> {
    trace!("getting current devices and volumes");
    let session = unsafe { DASession::new(None).context("could not create disk session") }?;
    let mut devices = HashMap::new();
    let mut volumes = Vec::new();

    // Get list of mounted volumes using getfsstat
    let mut fsstat = Vec::new();
    let mut buf_size = 0;

    // First call to get required buffer size
    unsafe {
        buf_size = libc::getfsstat(ptr::null_mut(), 0, libc::MNT_NOWAIT);
    }

    if buf_size < 0 {
        return Err(std::io::Error::last_os_error()).context("getfsstat failed");
    }

    trace!("getfsstat indicates {} mounted filesystems", buf_size);

    // Allocate buffer and get actual data
    fsstat.resize_with(buf_size as usize, || unsafe {
        MaybeUninit::zeroed().assume_init()
    });

    let count = unsafe {
        libc::getfsstat(
            fsstat.as_mut_ptr(),
            buf_size * std::mem::size_of::<libc::statfs>() as i32,
            libc::MNT_NOWAIT,
        )
    };

    if count < 0 {
        return Err(std::io::Error::last_os_error()).context("getfsstat failed");
    }

    fsstat.truncate(count as usize);

    // Process each mounted filesystem
    for fs in fsstat {
        // Create BSD name from device name
        let dev_name = unsafe { CStr::from_ptr(fs.f_mntfromname.as_ptr()) };
        trace!("device name: {:?}", dev_name);

        // Create disk object from BSD name
        let disk = unsafe {
            DADisk::from_bsd_name(
                None,
                &session,
                NonNull::new(dev_name.as_ptr() as *mut _).unwrap(),
            )
        };

        let Some(disk) = disk else {
            trace!("could not create disk object for {:?}", dev_name);
            continue;
        };

        // Get disk description
        let disk_desc = unsafe { disk.description() };

        let Some(disk_desc) = disk_desc else {
            trace!("could not get disk description for {:?}", dev_name);
            continue;
        };

        let disk_desc: &CFDictionary<CFString> = unsafe { disk_desc.cast_unchecked() };

        // Get volume info
        let volume = get_volume_info(disk_desc);
        trace!("got volume: {:?}", volume);

        // Get device info if we haven't seen this device before
        let device = get_device_info(disk_desc);
        trace!("got device: {:?}", device);

        if let Some(device_id) = &device.id {
            let device = devices.entry(device_id.clone()).or_insert(device);

            if let Some(volume_id) = &volume.id {
                device.volumes.insert(volume_id.clone());
            }
        }

        volumes.push(volume);
    }

    trace!(
        "found {} devices and {} volumes",
        devices.len(),
        volumes.len()
    );

    Ok((devices.into_values().collect(), volumes))
}

pub fn monitor_devices(tx: flume::Sender<StorageEvent>) -> anyhow::Result<()> {
    let session = unsafe { DASession::new(None).context("could not create disk session") }?;
    let run_loop = CFRunLoop::current().context("could not get current run loop")?;

    unsafe { session.schedule_with_run_loop(&run_loop, kCFRunLoopDefaultMode.unwrap()) };

    let context = Box::new(CallbackContext { tx });
    let context = Box::into_raw(context);

    unsafe {
        DARegisterDiskAppearedCallback(
            &session,
            None,
            Some(callbacks::disk_appeared),
            context as *mut c_void,
        )
    };

    unsafe {
        DARegisterDiskDisappearedCallback(
            &session,
            None,
            Some(callbacks::disk_disappeared),
            context as *mut c_void,
        )
    };

    unsafe {
        DARegisterDiskDescriptionChangedCallback(
            &session,
            None,
            Some(kDADiskDescriptionWatchVolumePath),
            Some(callbacks::disk_changed),
            context as *mut c_void,
        )
    };

    CFRunLoop::run();

    unsafe { session.unschedule_from_run_loop(&run_loop, kCFRunLoopDefaultMode.unwrap()) };

    // convert back into Box so it's cleaned up on drop
    let _ = unsafe { Box::from_raw(context) };

    Ok(())
}

mod callbacks {
    use std::{ffi::c_void, ptr::NonNull};

    use objc2_core_foundation::{CFArray, CFDictionary, CFString};
    use objc2_disk_arbitration::DADisk;

    use crate::StorageEvent;

    pub unsafe extern "C-unwind" fn disk_appeared(disk: NonNull<DADisk>, context: *mut c_void) {
        let disk_desc = unsafe { disk.as_ref().description() };
        let Some(disk_desc) = disk_desc else {
            return;
        };

        let disk_desc: &CFDictionary<CFString> = unsafe { disk_desc.cast_unchecked() };

        let context = &mut *(context as *mut super::CallbackContext);

        let volume = super::get_volume_info(&disk_desc);
        let device = super::get_device_info(&disk_desc);

        let _ = context.tx.send(StorageEvent::AddDevice { device });
        let _ = context.tx.send(StorageEvent::AddVolume { volume });
    }

    pub unsafe extern "C-unwind" fn disk_changed(
        disk: NonNull<DADisk>,
        _keys: NonNull<CFArray>,
        context: *mut c_void,
    ) {
        let disk_desc = unsafe { disk.as_ref().description() };
        let Some(disk_desc) = disk_desc else {
            return;
        };

        let disk_desc: &CFDictionary<CFString> = unsafe { disk_desc.cast_unchecked() };

        let context = &mut *(context as *mut super::CallbackContext);

        let volume = super::get_volume_info(&disk_desc);
        let device = super::get_device_info(&disk_desc);

        let _ = context.tx.send(StorageEvent::AddDevice { device });
        let _ = context.tx.send(StorageEvent::AddVolume { volume });
    }

    pub unsafe extern "C-unwind" fn disk_disappeared(disk: NonNull<DADisk>, context: *mut c_void) {
        let disk_desc = unsafe { disk.as_ref().description() };
        let Some(disk_desc) = disk_desc else {
            return;
        };

        let disk_desc: &CFDictionary<CFString> = unsafe { disk_desc.cast_unchecked() };

        let context = &mut *(context as *mut super::CallbackContext);

        // Get IDs before the device is fully removed
        let volume_id = super::get_volume_info(&disk_desc).id;
        let device_id = super::get_device_info(&disk_desc).id;

        if let Some(id) = volume_id {
            let _ = context.tx.send(StorageEvent::RemoveVolume { id });
        }

        if let Some(id) = device_id {
            let _ = context.tx.send(StorageEvent::RemoveDevice { id });
        }
    }
}

unsafe fn get_from_dict<V: objc2_core_foundation::Type>(
    disk_desc: &CFDictionary<CFString>,
    key: &CFString,
) -> Option<CFRetained<V>> {
    let value: Option<CFRetained<V>> = unsafe { disk_desc.cast_unchecked().get(key) };

    value
}

fn get_volume_info(disk_desc: &CFDictionary<CFString>) -> StorageVolume {
    let volume_id = {
        let uuid = unsafe { get_from_dict::<CFUUID>(disk_desc, kDADiskDescriptionVolumeUUIDKey) };
        uuid.and_then(|uuid| {
            CFUUID::new_string(None, Some(&uuid)).map(|uuid| VolumeId(uuid.to_string()))
        })
    };

    let mount_path = {
        let path = unsafe { get_from_dict::<CFURL>(disk_desc, kDADiskDescriptionVolumePathKey) };
        path.and_then(|path| path.to_file_path())
    };

    let size = {
        let size = unsafe { get_from_dict::<CFNumber>(disk_desc, kDADiskDescriptionMediaSizeKey) };
        size.and_then(|size| size.as_i64()).map(|size| size as u64)
    };

    let free = if let Some(mount_path) = &mount_path {
        if mount_path.as_os_str().len() > 0 {
            let mut statfs = MaybeUninit::uninit();
            let dev_mount = CString::new(mount_path.as_os_str().as_bytes()).unwrap();

            if unsafe { libc::statvfs(dev_mount.as_ptr(), statfs.as_mut_ptr()) } < 0 {
                None
            } else {
                let statfs = unsafe { statfs.assume_init() };
                Some(statfs.f_bavail as u64 * statfs.f_bsize as u64)
            }
        } else {
            None
        }
    } else {
        None
    };

    // Get the device ID from the device path
    let device_path = {
        let device_path =
            unsafe { get_from_dict::<CFString>(disk_desc, kDADiskDescriptionDevicePathKey) };
        device_path.map(|path| path.to_string())
    };

    let display_name =
        unsafe { get_from_dict::<CFString>(disk_desc, kDADiskDescriptionVolumeNameKey) };
    let display_name = display_name.map(|name| name.to_string());

    let media_writable =
        unsafe { get_from_dict::<CFBoolean>(disk_desc, kDADiskDescriptionMediaWritableKey) };
    let media_writable = media_writable.map(|writable| writable.as_bool());

    StorageVolume {
        id: volume_id,
        device_id: device_path.clone().map(|path| DeviceId(path)),
        display_name,
        size,
        free,
        path: device_path.clone().map(PathBuf::from),
        mounts: mount_path.into_iter().collect(),
        partition_id: None, // Could potentially get from BSD name
        is_writable: media_writable,
        is_system: None,
    }
}

fn get_device_info(disk_desc: &CFDictionary<CFString>) -> StorageDevice {
    let device_path =
        unsafe { get_from_dict::<CFString>(disk_desc, kDADiskDescriptionDevicePathKey) };
    let device_path = device_path.map(|path| path.to_string());
    let device_id = device_path.clone().map(|path| DeviceId(path));

    let device_serial =
        unsafe { get_from_dict::<CFUUID>(disk_desc, kDADiskDescriptionMediaUUIDKey) };
    let device_serial = device_serial
        .and_then(|serial| CFUUID::new_string(None, Some(&serial)))
        .map(|serial| serial.to_string());

    let display_name =
        unsafe { get_from_dict::<CFString>(disk_desc, kDADiskDescriptionMediaNameKey) };
    let display_name = display_name.map(|name| name.to_string());

    let is_internal =
        unsafe { get_from_dict::<CFBoolean>(disk_desc, kDADiskDescriptionDeviceInternalKey) };
    let is_internal = is_internal.map(|internal| internal.as_bool());

    let is_removable =
        unsafe { get_from_dict::<CFBoolean>(disk_desc, kDADiskDescriptionMediaRemovableKey) };
    let is_removable = is_removable.map(|removable| removable.as_bool());

    let is_ejectable =
        unsafe { get_from_dict::<CFBoolean>(disk_desc, kDADiskDescriptionMediaEjectableKey) };
    let is_ejectable = is_ejectable.map(|ejectable| ejectable.as_bool());

    let device_model =
        unsafe { get_from_dict::<CFString>(disk_desc, kDADiskDescriptionDeviceModelKey) };
    let device_model = device_model.map(|model| model.to_string());

    let kind = match device_model.as_deref() {
        Some("SD/MMC") => DeviceKind::SdCard,
        Some("Micro SD/M2") => DeviceKind::MicroSdCard,
        Some("Flash Disk") => DeviceKind::UsbFlashDrive,
        _ => match is_internal {
            Some(true) => DeviceKind::InternalDrive,
            Some(false) => DeviceKind::ExternalDrive,
            _ => DeviceKind::Other,
        },
    };

    StorageDevice {
        id: device_id,
        display_name: display_name,
        model: device_model,
        kind: kind,
        internal: is_internal,
        removable: is_removable,
        ejectable: is_ejectable,
        serial: device_serial,
        volumes: HashSet::new(), // Will be populated when processing volumes
    }
}
