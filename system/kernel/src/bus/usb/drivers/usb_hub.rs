//! USB Hub Class Driver (09_xx_xx)

use super::super::*;
use crate::{
    task::{scheduler::Timer, Task},
    *,
};
use alloc::sync::Arc;
use bitflags::*;
use core::{mem::transmute, num::NonZeroU8, pin::Pin, time::Duration};
use num_traits::FromPrimitive;

pub struct UsbHubStarter;

impl UsbHubStarter {
    #[inline]
    pub fn new() -> Arc<dyn UsbClassDriverStarter> {
        Arc::new(Self {})
    }
}

impl UsbClassDriverStarter for UsbHubStarter {
    fn instantiate(&self, device: &Arc<UsbDevice>) -> bool {
        let class = device.class();
        match class {
            UsbClass::HUB_FS | UsbClass::HUB_HS_MTT | UsbClass::HUB_HS_STT | UsbClass::HUB_SS => (),
            _ => return false,
        }

        let config = device.current_configuration();
        let mut current_interface = None;
        for interface in config.interfaces() {
            if interface.class() == class {
                current_interface = Some(interface);
                break;
            }
        }
        let interface = match current_interface.or(config.interfaces().first()) {
            Some(v) => v,
            None => return false,
        };
        let if_no = interface.if_no();
        let endpoint = match interface.endpoints().first() {
            Some(v) => v,
            None => todo!(),
        };
        let ep = endpoint.address();
        let ps = endpoint.descriptor().max_packet_size();
        if ps > 8 {
            return false;
        }
        device.configure_endpoint(endpoint.descriptor()).unwrap();

        match class {
            UsbClass::HUB_FS | UsbClass::HUB_HS_MTT | UsbClass::HUB_HS_STT => {
                UsbManager::register_xfer_task(Task::new(Usb2HubDriver::_usb_hub_task(
                    device.clone(),
                    if_no,
                    ep,
                    ps,
                )));
            }
            UsbClass::HUB_SS => {
                UsbManager::register_xfer_task(Task::new(Usb3HubDriver::_usb_hub_task(
                    device.clone(),
                    if_no,
                    ep,
                    ps,
                )));
            }
            _ => (),
        }

        true
    }
}

pub struct Usb2HubDriver {
    device: Arc<UsbDevice>,
    hub_desc: UsbHub2Descriptor,
    lock: Pin<Arc<AsyncSharedLockTemp>>,
}

impl Usb2HubDriver {
    /// USB2 Hub Task (FS, HS, HS-MTT)
    async fn _usb_hub_task(
        device: Arc<UsbDevice>,
        _if_no: UsbInterfaceNumber,
        ep: UsbEndpointAddress,
        ps: u16,
    ) {
        let addr = device.addr();
        let is_mtt = device.class() == UsbClass::HUB_HS_MTT;

        let hub_desc: UsbHub2Descriptor =
            match UsbHubCommon::get_hub_descriptor(&device, UsbDescriptorType::Hub, 0) {
                Ok(v) => v,
                Err(_err) => {
                    // TODO:
                    log!("USB2 GET HUB DESCRIPTOR {:?}", _err);
                    return;
                }
            };
        match device.host().configure_hub2(&hub_desc, is_mtt) {
            Ok(_) => (),
            Err(_err) => {
                // TODO:
                log!("USB2 COFNIGURE HUB2 {:?}", _err);
                return;
            }
        }
        let hub = Arc::new(Usb2HubDriver {
            device: device.clone(),
            hub_desc,
            lock: AsyncSharedLockTemp::new(),
        });

        UsbManager::focus_hub(device.addr());
        hub.lock.lock_shared();
        UsbManager::schedule_configuration(Some(device.addr()), Box::pin(hub.clone().init_hub()));
        hub.lock.wait().await;
        UsbManager::unfocus_hub(device.addr());

        let n_ports = hub_desc.num_ports();
        let mut port_event = [0u8; 8];
        loop {
            match device.read_slice(ep, &mut port_event, 1, ps as usize).await {
                Ok(_) => {
                    UsbManager::focus_hub(device.addr());
                    let port_change_bitmap = (port_event[0] as u16) | ((port_event[1] as u16) << 8);
                    for i in 1..=n_ports {
                        if (port_change_bitmap & (1 << i)) != 0 {
                            let port =
                                UsbHubPortNumber(unsafe { NonZeroU8::new_unchecked(i as u8) });
                            let status = Self::get_port_status(&device, port).unwrap();
                            if status
                                .change
                                .contains(UsbHub2PortChangeBit::C_PORT_CONNECTION)
                            {
                                Timer::sleep_async(hub_desc.power_on_to_power_good()).await;
                                Self::clear_port_feature(
                                    &device,
                                    UsbHub2PortFeatureSel::C_PORT_CONNECTION,
                                    port,
                                )
                                .unwrap();

                                if status
                                    .status
                                    .contains(UsbHub2PortStatusBit::PORT_CONNECTION)
                                {
                                    // Attached
                                    hub.attach_device(port).await;
                                } else {
                                    log!("ADDR {} HUB2 PORT {} DETACHED", addr.0, i);
                                    // TODO: Detached
                                }
                            } else {
                                use UsbHub2PortFeatureSel::*;
                                if status.change.contains(UsbHub2PortChangeBit::C_PORT_ENABLE) {
                                    Self::clear_port_feature(&device, C_PORT_ENABLE, port).unwrap();
                                }
                                if status.change.contains(UsbHub2PortChangeBit::C_PORT_SUSPEND) {
                                    Self::clear_port_feature(&device, C_PORT_SUSPEND, port)
                                        .unwrap();
                                }
                                if status
                                    .change
                                    .contains(UsbHub2PortChangeBit::C_PORT_OVER_CURRENT)
                                {
                                    Self::clear_port_feature(&device, C_PORT_OVER_CURRENT, port)
                                        .unwrap();
                                }
                                if status.change.contains(UsbHub2PortChangeBit::C_PORT_RESET) {
                                    Self::clear_port_feature(&device, C_PORT_RESET, port).unwrap();
                                }
                            }
                        }
                    }
                    UsbManager::unfocus_hub(device.addr());
                }
                Err(UsbError::Aborted) => break,
                Err(_err) => {
                    // TODO:
                    log!("USB2 HUB READ ERROR {:?}", _err);
                    return;
                }
            }
        }
    }

    pub async fn init_hub(self: Arc<Self>) {
        let defer = self.lock.unlock_shared();

        let n_ports = self.hub_desc.num_ports();
        for i in 1..=n_ports {
            Self::set_port_feature(
                &self.device,
                UsbHub2PortFeatureSel::PORT_POWER,
                UsbHubPortNumber(unsafe { NonZeroU8::new_unchecked(i as u8) }),
            )
            .unwrap();
            Timer::sleep_async(Duration::from_millis(10)).await;
        }
        for i in 1..=n_ports {
            Self::clear_port_feature(
                &self.device,
                UsbHub2PortFeatureSel::C_PORT_CONNECTION,
                UsbHubPortNumber(unsafe { NonZeroU8::new_unchecked(i as u8) }),
            )
            .unwrap();
            Timer::sleep_async(Duration::from_millis(10)).await;
        }
        Timer::sleep_async(self.hub_desc.power_on_to_power_good() * 2).await;

        for i in 1..=n_ports {
            let port = UsbHubPortNumber(unsafe { NonZeroU8::new_unchecked(i as u8) });
            let status = Self::get_port_status(&self.device, port).unwrap();
            if status
                .status
                .contains(UsbHub2PortStatusBit::PORT_CONNECTION)
            {
                self.lock.lock_shared();
                self.clone()._attach_device(port).await;
            }
            Timer::sleep_async(Duration::from_millis(10)).await;
        }

        drop(defer);
    }

    pub async fn attach_device(self: &Arc<Self>, port: UsbHubPortNumber) {
        self.lock.lock_shared();
        UsbManager::schedule_configuration(
            Some(self.device.addr()),
            Box::pin(self.clone()._attach_device(port)),
        );
        self.lock.wait().await;
    }

    async fn _attach_device(self: Arc<Self>, port: UsbHubPortNumber) {
        let defer = self.lock.unlock_shared();

        Self::set_port_feature(&self.device, UsbHub2PortFeatureSel::PORT_RESET, port).unwrap();
        Timer::sleep_async(self.hub_desc.power_on_to_power_good()).await;

        let status = Self::get_port_status(&self.device, port).unwrap();
        if status
            .change
            .contains(UsbHub2PortChangeBit::C_PORT_CONNECTION)
        {
            Self::clear_port_feature(&self.device, UsbHub2PortFeatureSel::C_PORT_CONNECTION, port)
                .unwrap();
        }
        if status.change.contains(UsbHub2PortChangeBit::C_PORT_ENABLE) {
            Self::clear_port_feature(&self.device, UsbHub2PortFeatureSel::C_PORT_ENABLE, port)
                .unwrap();
        }
        if status.change.contains(UsbHub2PortChangeBit::C_PORT_SUSPEND) {
            Self::clear_port_feature(&self.device, UsbHub2PortFeatureSel::C_PORT_SUSPEND, port)
                .unwrap();
        }
        if status
            .change
            .contains(UsbHub2PortChangeBit::C_PORT_OVER_CURRENT)
        {
            Self::clear_port_feature(
                &self.device,
                UsbHub2PortFeatureSel::C_PORT_OVER_CURRENT,
                port,
            )
            .unwrap();
        }
        if status.change.contains(UsbHub2PortChangeBit::C_PORT_RESET) {
            Self::clear_port_feature(&self.device, UsbHub2PortFeatureSel::C_PORT_RESET, port)
                .unwrap();
        }
        Timer::sleep_async(self.hub_desc.power_on_to_power_good()).await;

        if status
            .status
            .contains(UsbHub2PortStatusBit::PORT_CONNECTION)
        {
            let speed = status.status.speed();
            let _child = self.device.host().attach_device(port, speed).unwrap();
        }

        drop(defer);
    }

    pub fn get_port_status(
        device: &UsbDevice,
        port: UsbHubPortNumber,
    ) -> Result<UsbHub2PortStatus, UsbError> {
        UsbHubCommon::get_port_status(device, port)
    }

    pub fn set_port_feature(
        device: &UsbDevice,
        feature_sel: UsbHub2PortFeatureSel,
        port: UsbHubPortNumber,
    ) -> Result<(), UsbError> {
        UsbHubCommon::set_port_feature(device, feature_sel, port)
    }

    pub fn clear_port_feature(
        device: &UsbDevice,
        feature_sel: UsbHub2PortFeatureSel,
        port: UsbHubPortNumber,
    ) -> Result<(), UsbError> {
        UsbHubCommon::clear_port_feature(device, feature_sel, port)
    }
}

pub struct Usb3HubDriver {
    device: Arc<UsbDevice>,
    hub_desc: UsbHub3Descriptor,
    lock: Pin<Arc<AsyncSharedLockTemp>>,
}

impl Usb3HubDriver {
    async fn _usb_hub_task(
        device: Arc<UsbDevice>,
        _if_no: UsbInterfaceNumber,
        ep: UsbEndpointAddress,
        ps: u16,
    ) {
        let addr = device.addr();
        let hub_desc: UsbHub3Descriptor =
            match UsbHubCommon::get_hub_descriptor(&device, UsbDescriptorType::Hub3, 0) {
                Ok(v) => v,
                Err(_err) => {
                    // TODO:
                    log!("USB3 GET HUB DESCRIPTOR {:?}", _err);
                    return;
                }
            };
        Self::set_depth(&device).unwrap();

        let hub = Arc::new(Usb3HubDriver {
            device: device.clone(),
            hub_desc,
            lock: AsyncSharedLockTemp::new(),
        });

        // let max_exit_latency = ss_dev_cap.u1_dev_exit_lat() + ss_dev_cap.u2_dev_exit_lat();

        UsbManager::focus_hub(device.addr());
        hub.lock.lock_shared();
        UsbManager::schedule_configuration(Some(device.addr()), Box::pin(hub.clone().init_hub()));
        hub.lock.wait().await;
        UsbManager::unfocus_hub(device.addr());

        let n_ports = hub_desc.num_ports();
        let mut port_event = [0u8; 8];
        loop {
            match device.read_slice(ep, &mut port_event, 1, ps as usize).await {
                Ok(_) => {
                    let port_change_bitmap = (port_event[0] as u16) | ((port_event[1] as u16) << 8);
                    UsbManager::focus_hub(device.addr());
                    for i in 1..=n_ports {
                        if (port_change_bitmap & (1 << i)) != 0 {
                            let port =
                                UsbHubPortNumber(unsafe { NonZeroU8::new_unchecked(i as u8) });
                            let status = Self::get_port_status(&device, port).unwrap();
                            if status
                                .change
                                .contains(UsbHub3PortChangeBit::C_PORT_CONNECTION)
                            {
                                Timer::sleep_async(hub_desc.power_on_to_power_good()).await;
                                Self::clear_port_feature(
                                    &device,
                                    UsbHub3PortFeatureSel::C_PORT_CONNECTION,
                                    port,
                                )
                                .unwrap();

                                if status
                                    .status
                                    .contains(UsbHub3PortStatusBit::PORT_CONNECTION)
                                {
                                    // Attached
                                    hub.attach_device(port).await;
                                } else {
                                    log!("ADDR {} HUB3 PORT {} DETACHED", addr.0, i);
                                    // TODO: Detached
                                }
                            } else {
                                use UsbHub3PortFeatureSel::*;
                                if status
                                    .change
                                    .contains(UsbHub3PortChangeBit::C_BH_PORT_RESET)
                                {
                                    Self::clear_port_feature(&device, C_BH_PORT_RESET, port)
                                        .unwrap();
                                }
                                if status.change.contains(UsbHub3PortChangeBit::C_PORT_RESET) {
                                    Self::clear_port_feature(&device, C_PORT_RESET, port).unwrap();
                                }
                                if status
                                    .change
                                    .contains(UsbHub3PortChangeBit::C_PORT_OVER_CURRENT)
                                {
                                    Self::clear_port_feature(&device, C_PORT_OVER_CURRENT, port)
                                        .unwrap();
                                }
                                if status
                                    .change
                                    .contains(UsbHub3PortChangeBit::C_PORT_LINK_STATE)
                                {
                                    Self::clear_port_feature(&device, C_PORT_LINK_STATE, port)
                                        .unwrap();
                                }
                                if status
                                    .change
                                    .contains(UsbHub3PortChangeBit::C_PORT_CONFIG_ERROR)
                                {
                                    Self::clear_port_feature(&device, C_PORT_CONFIG_ERROR, port)
                                        .unwrap();
                                }
                            }
                        }
                    }
                    UsbManager::unfocus_hub(device.addr());
                }
                Err(UsbError::Aborted) => break,
                Err(_err) => {
                    // TODO:
                    log!("USB3 HUB READ ERROR {:?}", _err);
                    return;
                }
            }
        }
    }

    pub async fn init_hub(self: Arc<Self>) {
        let defer = self.lock.unlock_shared();

        match self.device.host().configure_hub3(&self.hub_desc, 0) {
            Ok(_) => (),
            Err(_err) => {
                // TODO:
                log!("USB3 COFNIGURE HUB3 {:?}", _err);
                return;
            }
        }
        let n_ports = self.hub_desc.num_ports();

        for i in 1..=n_ports {
            let port = UsbHubPortNumber(unsafe { NonZeroU8::new_unchecked(i as u8) });
            let status = Self::get_port_status(&self.device, port).unwrap();
            Self::set_port_feature(&self.device, UsbHub3PortFeatureSel::PORT_POWER, port).unwrap();
            Timer::sleep_async(Duration::from_millis(10)).await;
            // Timer::sleep_async(hub_desc.power_on_to_power_good()).await;
            if status
                .status
                .contains(UsbHub3PortStatusBit::PORT_CONNECTION | UsbHub3PortStatusBit::PORT_ENABLE)
            {
                self.lock.lock_shared();
                self.clone()._attach_device(port).await;
            }
        }

        drop(defer);
    }

    pub async fn attach_device(self: &Arc<Self>, port: UsbHubPortNumber) {
        self.lock.lock_shared();
        UsbManager::schedule_configuration(
            Some(self.device.addr()),
            Box::pin(self.clone()._attach_device(port)),
        );
        self.lock.wait().await;
    }

    pub async fn _attach_device(self: Arc<Self>, port: UsbHubPortNumber) {
        let defer = self.lock.unlock_shared();

        // Self::clear_port_feature(&device, UsbHub3PortFeatureSel::C_PORT_RESET, port).unwrap();
        Self::set_port_feature(&self.device, UsbHub3PortFeatureSel::BH_PORT_RESET, port).unwrap();

        let deadline = Timer::new(self.hub_desc.power_on_to_power_good() * 2);
        loop {
            let status = Self::get_port_status(&self.device, port).unwrap();
            if deadline.is_expired()
                || status.status.contains(
                    UsbHub3PortStatusBit::PORT_CONNECTION | UsbHub3PortStatusBit::PORT_ENABLE,
                )
            {
                break;
            }
            Timer::sleep_async(Duration::from_millis(10)).await;
        }

        let status = Self::get_port_status(&self.device, port).unwrap();
        if status
            .change
            .contains(UsbHub3PortChangeBit::C_BH_PORT_RESET)
        {
            Self::clear_port_feature(&self.device, UsbHub3PortFeatureSel::C_BH_PORT_RESET, port)
                .unwrap();
        }
        if status.change.contains(UsbHub3PortChangeBit::C_PORT_RESET) {
            Self::clear_port_feature(&self.device, UsbHub3PortFeatureSel::C_PORT_RESET, port)
                .unwrap();
        }
        if status
            .change
            .contains(UsbHub3PortChangeBit::C_PORT_OVER_CURRENT)
        {
            Self::clear_port_feature(
                &self.device,
                UsbHub3PortFeatureSel::C_PORT_OVER_CURRENT,
                port,
            )
            .unwrap();
        }
        if status
            .change
            .contains(UsbHub3PortChangeBit::C_PORT_LINK_STATE)
        {
            Self::clear_port_feature(&self.device, UsbHub3PortFeatureSel::C_PORT_LINK_STATE, port)
                .unwrap();
        }
        if status
            .change
            .contains(UsbHub3PortChangeBit::C_PORT_CONFIG_ERROR)
        {
            Self::clear_port_feature(
                &self.device,
                UsbHub3PortFeatureSel::C_PORT_CONFIG_ERROR,
                port,
            )
            .unwrap();
        }

        let status = Self::get_port_status(&self.device, port).unwrap();
        if status
            .status
            .contains(UsbHub3PortStatusBit::PORT_CONNECTION | UsbHub3PortStatusBit::PORT_ENABLE)
        {
            let _child = self.device.host().attach_device(port, PSIV::SS).unwrap();
        }

        drop(defer);
    }

    pub fn set_depth(device: &UsbDevice) -> Result<(), UsbError> {
        device.control_nodata(
            UsbControlSetupData::request(
                UsbControlRequestBitmap::SET_CLASS,
                UsbControlRequest::SET_HUB_DEPTH,
            )
            .value(device.route_string().depth() as u16),
        )
    }

    pub fn get_port_status(
        device: &UsbDevice,
        port: UsbHubPortNumber,
    ) -> Result<UsbHub3PortStatus, UsbError> {
        UsbHubCommon::get_port_status(device, port)
    }

    pub fn set_port_feature(
        device: &UsbDevice,
        feature_sel: UsbHub3PortFeatureSel,
        port: UsbHubPortNumber,
    ) -> Result<(), UsbError> {
        UsbHubCommon::set_port_feature(device, feature_sel, port)
    }

    pub fn clear_port_feature(
        device: &UsbDevice,
        feature_sel: UsbHub3PortFeatureSel,
        port: UsbHubPortNumber,
    ) -> Result<(), UsbError> {
        UsbHubCommon::clear_port_feature(device, feature_sel, port)
    }
}

pub struct UsbHubCommon;

impl UsbHubCommon {
    #[inline]
    pub fn get_hub_descriptor<T: UsbDescriptor>(
        device: &UsbDevice,
        desc_type: UsbDescriptorType,
        index: u8,
    ) -> Result<T, UsbError> {
        device.get_descriptor(UsbControlRequestBitmap::GET_CLASS, desc_type, index)
    }

    #[inline]
    pub fn set_hub_feature(
        device: &UsbDevice,
        feature_sel: UsbHubFeatureSel,
    ) -> Result<(), UsbError> {
        device.control_nodata(
            UsbControlSetupData::request(
                UsbControlRequestBitmap::SET_CLASS,
                UsbControlRequest::SET_FEATURE,
            )
            .value(feature_sel as u16),
        )
    }

    #[inline]
    pub fn clear_hub_feature(
        device: &UsbDevice,
        feature_sel: UsbHubFeatureSel,
    ) -> Result<(), UsbError> {
        device.control_nodata(
            UsbControlSetupData::request(
                UsbControlRequestBitmap::SET_CLASS,
                UsbControlRequest::CLEAR_FEATURE,
            )
            .value(feature_sel as u16),
        )
    }

    #[inline]
    pub fn set_port_feature<T>(
        device: &UsbDevice,
        feature_sel: T,
        port: UsbHubPortNumber,
    ) -> Result<(), UsbError>
    where
        T: Into<u16>,
    {
        device.control_nodata(
            UsbControlSetupData::request(
                UsbControlRequestBitmap(0x23),
                UsbControlRequest::SET_FEATURE,
            )
            .value(feature_sel.into())
            .index(port.0.get() as u16),
        )
    }

    #[inline]
    pub fn clear_port_feature<T>(
        device: &UsbDevice,
        feature_sel: T,
        port: UsbHubPortNumber,
    ) -> Result<(), UsbError>
    where
        T: Into<u16>,
    {
        device.control_nodata(
            UsbControlSetupData::request(
                UsbControlRequestBitmap(0x23),
                UsbControlRequest::CLEAR_FEATURE,
            )
            .value(feature_sel.into())
            .index(port.0.get() as u16),
        )
    }

    pub fn get_port_status<T: Copy>(
        device: &UsbDevice,
        port: UsbHubPortNumber,
    ) -> Result<T, UsbError> {
        let mut data = [0; 4];
        match device.control_slice(
            UsbControlSetupData::request(
                UsbControlRequestBitmap(0xA3),
                UsbControlRequest::GET_STATUS,
            )
            .value(0)
            .index(port.0.get() as u16),
            &mut data,
        ) {
            Ok(_) => {
                let result = unsafe {
                    let p = &data[0] as *const _ as *const T;
                    *p
                };
                Ok(result)
            }
            Err(err) => Err(err),
        }
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct UsbHub2PortStatus {
    status: UsbHub2PortStatusBit,
    change: UsbHub2PortChangeBit,
}

impl UsbHub2PortStatus {
    #[inline]
    pub const fn empty() -> Self {
        Self {
            status: UsbHub2PortStatusBit::empty(),
            change: UsbHub2PortChangeBit::empty(),
        }
    }

    #[inline]
    pub const fn as_u32(&self) -> u32 {
        unsafe { transmute(*self) }
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct UsbHub3PortStatus {
    status: UsbHub3PortStatusBit,
    change: UsbHub3PortChangeBit,
}

impl UsbHub3PortStatus {
    #[inline]
    pub const fn empty() -> Self {
        Self {
            status: UsbHub3PortStatusBit::empty(),
            change: UsbHub3PortChangeBit::empty(),
        }
    }

    #[inline]
    pub const fn as_u32(&self) -> u32 {
        unsafe { transmute(*self) }
    }
}

/// USB Hub Feature Selector
#[allow(non_camel_case_types)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum UsbHubFeatureSel {
    C_HUB_LOCAL_POWER = 0,
    C_HUB_OVER_CURRENT = 1,
}

/// USB2 Hub Port Feature Selector
#[allow(non_camel_case_types)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum UsbHub2PortFeatureSel {
    PORT_CONNECTION = 0,
    PORT_ENABLE = 1,
    PORT_SUSPEND = 2,
    PORT_OVER_CURRENT = 3,
    PORT_RESET = 4,
    PORT_POWER = 8,
    PORT_LOW_SPEED = 9,
    C_PORT_CONNECTION = 16,
    C_PORT_ENABLE = 17,
    C_PORT_SUSPEND = 18,
    C_PORT_OVER_CURRENT = 19,
    C_PORT_RESET = 20,
    PORT_TEST = 21,
    PORT_INDICATOR = 22,
}

impl Into<u16> for UsbHub2PortFeatureSel {
    fn into(self) -> u16 {
        self as u16
    }
}

/// USB3 Hub Port Feature Selector
#[allow(non_camel_case_types)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum UsbHub3PortFeatureSel {
    PORT_CONNECTION = 0,
    PORT_OVER_CURRENT = 3,
    PORT_RESET = 4,
    PORT_LINK_STATE = 5,
    PORT_POWER = 8,
    C_PORT_CONNECTION = 16,
    C_PORT_OVER_CURRENT = 19,
    C_PORT_RESET = 20,
    PORT_U1_TIMEOUT = 23,
    PORT_U2_TIMEOUT = 24,
    C_PORT_LINK_STATE = 25,
    C_PORT_CONFIG_ERROR = 26,
    PORT_REMOTE_WAKE_MASK = 27,
    BH_PORT_RESET = 28,
    C_BH_PORT_RESET = 29,
    FORCE_LINKPM_ACCEPT = 30,
}

impl Into<u16> for UsbHub3PortFeatureSel {
    fn into(self) -> u16 {
        self as u16
    }
}

bitflags! {
    /// USB2 Hub Port Status Bits
    pub struct UsbHub2PortStatusBit: u16 {
        const PORT_CONNECTION   = 0b0000_0000_0000_0001;
        const PORT_ENABLE       = 0b0000_0000_0000_0010;
        const PORT_SUSPEND      = 0b0000_0000_0000_0100;
        const PORT_OVER_CURRENT = 0b0000_0000_0000_1000;
        const PORT_RESET        = 0b0000_0000_0001_0000;

        const PORT_POWER        = 0b0000_0001_0000_0000;
        const PORT_LOW_SPEED    = 0b0000_0010_0000_0001;
        const PORT_HIGH_SPEED   = 0b0000_0100_0000_0001;
        const PORT_TEST         = 0b0000_1000_0000_0001;
        const PORT_INDICATOR    = 0b0001_0000_0000_0001;
    }
}

impl UsbHub2PortStatusBit {
    #[inline]
    pub fn speed(&self) -> PSIV {
        if self.contains(Self::PORT_LOW_SPEED) {
            PSIV::LS
        } else if self.contains(Self::PORT_HIGH_SPEED) {
            PSIV::HS
        } else {
            PSIV::FS
        }
    }
}

bitflags! {
    /// USB2 Hub Port Status Change Bits
    pub struct UsbHub2PortChangeBit: u16 {
        const C_PORT_CONNECTION     = 0b0000_0000_0000_0001;
        const C_PORT_ENABLE         = 0b0000_0000_0000_0010;
        const C_PORT_SUSPEND        = 0b0000_0000_0000_0100;
        const C_PORT_OVER_CURRENT   = 0b0000_0000_0000_1000;
        const C_PORT_RESET          = 0b0000_0000_0001_0000;
    }
}

bitflags! {
    /// USB3 Hub Port Status Bits
    pub struct UsbHub3PortStatusBit: u16 {
        const PORT_CONNECTION   = 0b0000_0000_0000_0001;
        const PORT_ENABLE       = 0b0000_0000_0000_0010;
        const PORT_OVER_CURRENT = 0b0000_0000_0000_1000;
        const PORT_RESET        = 0b0000_0000_0001_0000;
        const PORT_LINK_STATE   = 0b0000_0001_1110_0000;
        const PORT_POWER        = 0b0000_0010_0000_0000;
        const PORT_SPEED        = 0b0001_1100_0000_0001;
    }
}

impl UsbHub3PortStatusBit {
    #[inline]
    pub const fn link_state_raw(&self) -> usize {
        ((self.bits() & Self::PORT_LINK_STATE.bits()) as usize) >> 5
    }

    #[inline]
    pub fn link_state(&self) -> Option<Usb3LinkState> {
        FromPrimitive::from_usize(self.link_state_raw())
    }
}

bitflags! {
    /// USB3 Hub Port Status Change Bits
    pub struct UsbHub3PortChangeBit: u16 {
        const C_PORT_CONNECTION     = 0b0000_0000_0000_0001;
        const C_PORT_OVER_CURRENT   = 0b0000_0000_0000_1000;
        const C_PORT_RESET          = 0b0000_0000_0001_0000;
        const C_BH_PORT_RESET       = 0b0000_0000_0010_0000;
        const C_PORT_LINK_STATE     = 0b0000_0000_0100_0000;
        const C_PORT_CONFIG_ERROR   = 0b0000_0000_1000_0000;
    }
}
