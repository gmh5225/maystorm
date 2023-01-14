pub mod cpu;

#[doc(hidden)]
pub mod apic;
#[doc(hidden)]
pub mod hpet;
pub mod page;
#[doc(hidden)]
pub mod ps2;
#[doc(hidden)]
pub mod rtc;

#[doc(hidden)]
pub mod hal_x64;
pub use hal_x64 as hal;

use crate::{assert_call_once, system::*};
use core::arch::asm;
use megstd::time::SystemTime;

pub struct Arch;

impl Arch {
    pub unsafe fn init() {
        assert_call_once!();

        cpu::Cpu::init();

        let acpi = System::acpi().unwrap();

        if let Some(madt) = acpi.find_first::<myacpi::madt::Madt>() {
            apic::Apic::init(madt);
        } else {
            panic!("NO APIC");
        }

        rtc::Rtc::init();
    }

    pub unsafe fn late_init() {
        assert_call_once!();

        let _ = ps2::Ps2::init();

        let device = System::current_device();

        if let Some((manufacturer, model)) = device.manufacturer_name().zip(device.model_name()) {
            match (manufacturer, model) {
                ("GPD", "MicroPC") => {
                    // WORKAROUND: Enable the GPD MicroPC's built-in keyboard
                    // SBRG.H_EC.KBCD = 0x11
                    Self::wr_ec(0x11, 0x00);
                }
                _ => (),
            }
        }
    }

    /// Issue WR_EC command to embedded controller (expr)
    unsafe fn wr_ec(addr: u8, data: u8) {
        Self::ec_wait_for_ibf();
        asm!("out 0x66, al", in("al") 0x81u8);
        Self::ec_wait_for_ibf();
        asm!("out 0x62, al", in("al") addr);
        Self::ec_wait_for_ibf();
        asm!("out 0x62, al", in("al") data);
    }

    /// Wait for embedded controller (expr)
    unsafe fn ec_wait_for_ibf() {
        loop {
            let al: u8;
            asm!("in al, 0x66", out("al") al);
            if (al & 0x02) == 0 {
                break;
            }
            asm!("pause");
        }
    }

    #[inline]
    pub fn system_time() -> SystemTime {
        rtc::Rtc::system_time()
    }
}
