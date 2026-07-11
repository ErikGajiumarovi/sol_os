use x86_64::instructions::port::Port;

const CONFIG_ADDRESS: u16 = 0xcf8;
const CONFIG_DATA: u16 = 0xcfc;

const CLASS_SERIAL_BUS: u8 = 0x0c;
const SUBCLASS_USB: u8 = 0x03;
const PROGIF_XHCI: u8 = 0x30;

#[derive(Debug, Clone, Copy)]
pub struct XhciController {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
    pub mmio_base: u64,
}

/// Locate the first PCI xHCI controller and enable memory decoding plus bus
/// mastering. This uses PCI configuration mechanism #1, which is available on
/// Q35 and conventional x86 firmware platforms after UEFI exits.
pub fn find_xhci_controller() -> Option<XhciController> {
    for bus in 0..=u8::MAX {
        for device in 0..32 {
            if let Some(controller) = scan_function(bus, device, 0) {
                return Some(controller);
            }

            let header = read_u32(bus, device, 0, 0x0c);
            if header & 0x80_0000 == 0 {
                continue;
            }
            for function in 1..8 {
                if let Some(controller) = scan_function(bus, device, function) {
                    return Some(controller);
                }
            }
        }
    }
    None
}

fn scan_function(bus: u8, device: u8, function: u8) -> Option<XhciController> {
    let id = read_u32(bus, device, function, 0x00);
    if id & 0xffff == 0xffff {
        return None;
    }
    let class = read_u32(bus, device, function, 0x08);
    let class_code = (class >> 24) as u8;
    let subclass = (class >> 16) as u8;
    let programming_interface = (class >> 8) as u8;
    if (class_code, subclass, programming_interface)
        != (CLASS_SERIAL_BUS, SUBCLASS_USB, PROGIF_XHCI)
    {
        return None;
    }

    let low_bar = read_u32(bus, device, function, 0x10);
    if low_bar & 0x01 != 0 {
        return None;
    }
    let bar_type = (low_bar >> 1) & 0x03;
    let mmio_base = match bar_type {
        0x00 => (low_bar & !0x0f) as u64,
        0x02 => {
            let high_bar = read_u32(bus, device, function, 0x14);
            ((high_bar as u64) << 32) | (low_bar as u64 & !0x0f)
        }
        _ => return None,
    };
    if mmio_base == 0 {
        return None;
    }

    let command_status = read_u32(bus, device, function, 0x04);
    write_u32(bus, device, function, 0x04, command_status | 0x0000_0006);

    Some(XhciController {
        bus,
        device,
        function,
        mmio_base,
    })
}

fn read_u32(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    let address = config_address(bus, device, function, offset);
    // SAFETY: configuration mechanism #1 uses these two globally defined x86
    // ports. Startup is single-core and interrupts do not access PCI config.
    unsafe {
        Port::<u32>::new(CONFIG_ADDRESS).write(address);
        Port::<u32>::new(CONFIG_DATA).read()
    }
}

fn write_u32(bus: u8, device: u8, function: u8, offset: u8, value: u32) {
    let address = config_address(bus, device, function, offset);
    // SAFETY: see `read_u32`; this write changes only the selected function's
    // command register after it was positively identified as xHCI.
    unsafe {
        Port::<u32>::new(CONFIG_ADDRESS).write(address);
        Port::<u32>::new(CONFIG_DATA).write(value);
    }
}

const fn config_address(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    0x8000_0000
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | (offset as u32 & 0xfc)
}
