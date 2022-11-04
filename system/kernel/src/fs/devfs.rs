use super::*;
use crate::{sync::Mutex, *};
use alloc::{borrow::ToOwned, collections::BTreeMap, string::String, sync::Arc};
use core::{
    mem::MaybeUninit,
    num::NonZeroU32,
    sync::atomic::{AtomicUsize, Ordering},
};
use megstd::{
    fs::FileType,
    io::{ErrorKind, Result},
};

const ROOT_INODE: INodeType = unsafe { INodeType::new_unchecked(1) };

static mut SHARED: MaybeUninit<DevFs> = MaybeUninit::uninit();

/// Device Filesystem
pub struct DevFs {
    minor_devices: Mutex<BTreeMap<MinorDevNo, Arc<ThisFsInodeEntry>>>,
    // next_major_device: AtomicUsize,
    next_minor_device: AtomicUsize,
}

impl DevFs {
    // const MAX_MAJOR_DEVICE: usize = 0x0000_FFFF;
    const MAX_MINOR_DEVICE: usize = 0x0000_FFFF;

    pub unsafe fn init() -> Arc<dyn FsDriver> {
        assert_call_once!();

        SHARED.write(Self {
            minor_devices: Mutex::new(BTreeMap::new()),
            // next_major_device: AtomicUsize::new(0),
            next_minor_device: AtomicUsize::new(1 + ROOT_INODE.get() as usize),
        });

        dev::install_drivers();

        let driver = DevFsDriver;
        Arc::new(driver)
    }

    #[inline]
    fn shared<'a>() -> &'a Self {
        unsafe { SHARED.assume_init_ref() }
    }

    // fn _next_major_device_no(&self) -> Option<MajorDevNo> {
    //     self.next_major_device
    //         .fetch_update(Ordering::SeqCst, Ordering::Relaxed, |v| {
    //             (v < Self::MAX_MAJOR_DEVICE).then(|| v + 1)
    //         })
    //         .ok()
    //         .and_then(|v| NonZeroU32::new(v as u32))
    //         .map(|v| MajorDevNo(v))
    // }

    fn _next_minor_device_no(&self) -> Option<MinorDevNo> {
        self.next_minor_device
            .fetch_update(Ordering::SeqCst, Ordering::Relaxed, |v| {
                (v < Self::MAX_MINOR_DEVICE).then(|| v + 1)
            })
            .ok()
            .and_then(|v| NonZeroU32::new(v as u32))
            .map(|v| MinorDevNo(v))
    }

    pub fn install_minor_device(driver: Arc<dyn DeviceFileDriver>) -> Option<MinorDevNo> {
        let shared = Self::shared();
        let Some(dev_no) = shared._next_minor_device_no() else { return None };
        let entry = ThisFsInodeEntry {
            file_type: driver.info().file_type,
            dev_no,
            name: driver.name(),
            driver,
        };
        shared
            .minor_devices
            .lock()
            .unwrap()
            .insert(dev_no, Arc::new(entry));
        Some(dev_no)
    }

    #[inline]
    fn get_file(dev_no: MinorDevNo) -> Option<Arc<ThisFsInodeEntry>> {
        DevFs::shared()
            .minor_devices
            .lock()
            .unwrap()
            .get(&dev_no)
            .map(|v| v.clone())
    }

    // #[inline]
    // fn stat(dev_no: MinorDevNo) -> Option<FsRawMetaData> {
    //     Self::get_file(dev_no).map(|v| v.as_ref().into())
    // }
}

struct DevFsDriver;

impl FsDriver for DevFsDriver {
    fn device_name(&self) -> String {
        "devfs".to_owned()
    }

    fn description(&self) -> String {
        "".to_owned()
    }

    fn root_dir(&self) -> INodeType {
        ROOT_INODE
    }

    fn read_dir(&self, dir: INodeType, index: usize) -> Option<FsRawDirEntry> {
        if dir == ROOT_INODE {
            let shared = DevFs::shared();
            shared
                .minor_devices
                .lock()
                .unwrap()
                .values()
                .nth(index)
                .map(|dir_ent| {
                    FsRawDirEntry::new(dir_ent.inode(), dir_ent.name(), dir_ent.as_ref().into())
                })
        } else {
            None
        }
    }

    fn find_file(&self, dir: INodeType, lpc: &str) -> Result<INodeType> {
        if dir == ROOT_INODE {
            let shared = DevFs::shared();
            shared
                .minor_devices
                .lock()
                .unwrap()
                .values()
                .find(|v| v.name() == lpc)
                .map(|v| v.inode())
                .ok_or(ErrorKind::NotFound.into())
        } else {
            Err(ErrorKind::NotFound.into())
        }
    }

    fn open(self: Arc<Self>, inode: INodeType) -> Result<Arc<dyn FsAccessToken>> {
        let Ok(dev_no) = inode.try_into() else {
            return Err(ErrorKind::NotFound.into())
        };
        DevFs::get_file(dev_no)
            .ok_or(ErrorKind::NotFound.into())
            .and_then(|v| v.driver.open())
    }

    fn stat(&self, inode: INodeType) -> Option<FsRawMetaData> {
        if inode == ROOT_INODE {
            Some(FsRawMetaData::new(FileType::Dir, 0))
        } else {
            inode
                .try_into()
                .ok()
                .and_then(|dev_no| DevFs::get_file(dev_no))
                .map(|v| v.as_ref().into())
        }
    }
}

struct ThisFsInodeEntry {
    file_type: FileType,
    dev_no: MinorDevNo,
    name: String,
    driver: Arc<dyn DeviceFileDriver>,
}

impl ThisFsInodeEntry {
    #[inline]
    pub fn inode(&self) -> INodeType {
        self.dev_no.into()
    }

    #[inline]
    pub fn name<'a>(&'a self) -> &'a str {
        self.name.as_str()
    }
}

impl From<&ThisFsInodeEntry> for FsRawMetaData {
    fn from(src: &ThisFsInodeEntry) -> Self {
        Self::new(src.file_type, src.driver.info().size as i64)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct MajorDevNo(NonZeroU32);

impl const From<MajorDevNo> for INodeType {
    #[inline]
    fn from(value: MajorDevNo) -> Self {
        unsafe { INodeType::new_unchecked((value.0.get() as u64) << 48) }
    }
}

impl TryFrom<INodeType> for MajorDevNo {
    type Error = ();

    #[inline]
    fn try_from(value: INodeType) -> core::result::Result<Self, Self::Error> {
        let value = value.get() >> 48;
        ((value as usize) < DevFs::MAX_MINOR_DEVICE)
            .then(|| NonZeroU32::new(value as u32))
            .flatten()
            .map(|v| MajorDevNo(v))
            .ok_or(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct MinorDevNo(NonZeroU32);

impl const From<MinorDevNo> for INodeType {
    #[inline]
    fn from(value: MinorDevNo) -> Self {
        unsafe { INodeType::new_unchecked(value.0.get() as u64) }
    }
}

impl TryFrom<INodeType> for MinorDevNo {
    type Error = ();

    #[inline]
    fn try_from(value: INodeType) -> core::result::Result<Self, Self::Error> {
        ((value.get() as usize) < DevFs::MAX_MINOR_DEVICE)
            .then(|| MinorDevNo(unsafe { NonZeroU32::new_unchecked(value.get() as u32) }))
            .ok_or(())
    }
}

pub trait DeviceFileDriver {
    fn name(&self) -> String;

    fn info(&self) -> &DeviceCharacteristics;

    fn open(&self) -> Result<Arc<dyn FsAccessToken>>;
}

#[derive(Debug, Clone, Copy)]
pub struct DeviceCharacteristics {
    pub file_type: FileType,
    pub size: usize,
}

// impl DeviceCharacteristics {}

impl From<DeviceCharacteristics> for FsRawMetaData {
    #[inline]
    fn from(value: DeviceCharacteristics) -> Self {
        Self::new(value.file_type, value.size as OffsetType)
    }
}
