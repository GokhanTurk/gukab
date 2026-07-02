//! Serial / console connection: an ephemeral (never persisted) connection to a
//! local serial device, driven by the shared [`crate::session`] engine so it has
//! the same macros / expects / logging / colorization as an SSH session.

pub mod client;

use serialport::{DataBits, FlowControl, Parity as SpParity, SerialPortType, StopBits};

/// Baud rates offered as presets in the form and cycled live with `Ctrl+B`.
pub const BAUD_PRESETS: [u32; 5] = [9600, 19200, 38400, 57600, 115200];

#[derive(Clone, Copy, PartialEq)]
pub enum Parity {
    None,
    Even,
    Odd,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Flow {
    None,
    Software,
    Hardware,
}

/// Everything needed to open a serial line. Built transiently by the console form;
/// never written to config. Defaults (8-N-1, no flow) match virtually all network
/// device console cables.
#[derive(Clone)]
pub struct SerialParams {
    pub device: String,
    pub baud: u32,
    /// 5..=8
    pub data_bits: u8,
    pub parity: Parity,
    /// 1..=2
    pub stop_bits: u8,
    pub flow: Flow,
}

impl SerialParams {
    pub fn data_bits_sp(&self) -> DataBits {
        match self.data_bits {
            5 => DataBits::Five,
            6 => DataBits::Six,
            7 => DataBits::Seven,
            _ => DataBits::Eight,
        }
    }

    pub fn parity_sp(&self) -> SpParity {
        match self.parity {
            Parity::None => SpParity::None,
            Parity::Even => SpParity::Even,
            Parity::Odd => SpParity::Odd,
        }
    }

    pub fn stop_bits_sp(&self) -> StopBits {
        if self.stop_bits == 2 {
            StopBits::Two
        } else {
            StopBits::One
        }
    }

    pub fn flow_sp(&self) -> FlowControl {
        match self.flow {
            Flow::None => FlowControl::None,
            Flow::Software => FlowControl::Software,
            Flow::Hardware => FlowControl::Hardware,
        }
    }

    /// Short label for the log folder: the device's final path component.
    pub fn log_label(&self) -> &str {
        self.device
            .rsplit('/')
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or("serial")
    }
}

/// Detected serial ports for the form's device picker, **USB adapters first**
/// (the actually-connected console cable), then others, each alphabetically.
/// Best-effort: returns an empty list if enumeration fails or is unsupported.
pub fn list_ports() -> Vec<String> {
    let mut ports = serialport::available_ports().unwrap_or_default();
    let rank = |t: &SerialPortType| match t {
        SerialPortType::UsbPort(_) => 0,
        _ => 1,
    };
    ports.sort_by(|a, b| {
        rank(&a.port_type)
            .cmp(&rank(&b.port_type))
            .then_with(|| a.port_name.cmp(&b.port_name))
    });
    ports.into_iter().map(|p| p.port_name).collect()
}

/// Default device path when nothing is auto-detected: `/dev/ttyUSB0` on Linux
/// (the common USB-serial node), `COM1` on Windows, empty elsewhere (macOS device
/// nodes are named after the adapter's serial, so there's no sensible default).
pub fn default_device() -> String {
    if cfg!(target_os = "linux") {
        "/dev/ttyUSB0".to_string()
    } else if cfg!(windows) {
        "COM1".to_string()
    } else {
        String::new()
    }
}
