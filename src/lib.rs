use std::{collections::HashSet, path::PathBuf};

use derive_more::From;

mod fs;

pub use fs::*;

#[derive(Clone, Debug, Hash, Eq, From)]
pub struct DeviceId(pub(crate) String);

impl PartialEq for DeviceId {
    fn eq(&self, other: &Self) -> bool {
        self.0.to_lowercase() == other.0.to_lowercase()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DeviceKind {
    UsbFlashDrive,
    SdCard,
    MicroSdCard,
    InternalDrive,
    ExternalDrive,
    Other,
}

#[derive(Clone, Debug)]
pub enum StorageEvent {
    AddDevice {
        device: StorageDevice,
    },
    UpdateDevice {
        device: StorageDevice,
    },
    RemoveDevice {
        id: DeviceId,
    },
    AddVolume {
        volume: StorageVolume,
    },
    UpdateVolume {
        volume: StorageVolume,
    },
    RemoveVolume {
        id: VolumeId,
    },
    /// This event is not intended for the client, but instead indicates that
    /// the device manager should perform a refresh and emit the needed events
    /// on its own. This is required on platforms like Windows where it is not
    /// possible to tell when physical devices have been removed, etc.
    Refresh,
}

#[derive(Clone, Debug)]

pub struct StorageDevice {
    /// Unique identifier of this device. This identifier isn't necessarily
    /// specific to the device itself, and may be not be stable across
    /// disconnects and reconnects. Use the serial number for a stable ID that
    /// is tied to the device itself.
    pub id: Option<DeviceId>,

    pub display_name: Option<String>,

    pub model: Option<String>,

    pub kind: DeviceKind,

    /// Whether this device is inside of the computer or outside.
    pub internal: Option<bool>,

    /// Whether this device is considered removable or not.
    pub removable: Option<bool>,

    /// Whether this device is considered removable or not.
    pub ejectable: Option<bool>,

    /// Serial number of hardware device hosting the volume, if available
    pub serial: Option<String>,

    /// IDs of volumes detected on the device
    pub volumes: HashSet<VolumeId>,
}

#[derive(Clone, Debug, Hash, Eq, From)]
pub struct VolumeId(pub(crate) String);

impl PartialEq for VolumeId {
    fn eq(&self, other: &Self) -> bool {
        self.0.to_lowercase() == other.0.to_lowercase()
    }
}

#[derive(Clone, Debug)]
pub struct StorageVolume {
    /// Unique identifier of this volume
    pub id: Option<VolumeId>,

    pub display_name: Option<String>,

    pub device_id: Option<DeviceId>,

    /// Size of device in bytes
    pub size: Option<u64>,

    /// Amount of free space on device in bytes
    pub free: Option<u64>,

    /// Platform-specific path that references the volume itself
    pub path: Option<PathBuf>,

    /// Path(s) where the files on this volume are mounted
    pub mounts: Vec<PathBuf>,

    /// Identifier for the partition on the device
    pub partition_id: Option<String>,

    /// True if this is a "system partition" that the user should not modify or see
    pub is_system: Option<bool>,

    pub is_writable: Option<bool>,
}
