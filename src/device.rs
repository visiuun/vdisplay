//! DS916 / MARS GAMING MC-3TCORELCD screen protocol (USB CDC, VID 33C3 / PID F101).
//!
//! Every frame is a 60-byte header followed by a baseline JPEG.

use serialport::SerialPort;
use std::io::Write as _;
use std::os::windows::io::AsRawHandle;
use std::time::Duration;

pub const VID: u16 = 0x33C3;
pub const PID: u16 = 0xF101;

const HEADER: [u8; 60] = [
    0x00, 0x3C, 0x00, 0x00, 0x00, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, //
    0x00, 0x4F, 0x54, 0x06, 0x00, 0x00, 0x00, 0x00, 0x1B, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, //
    0x00, 0x1B, 0x00, 0x00, 0x00, 0x00, 0x00, 0x89, 0xB3, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x00,
    0x09, //
    0x00, 0x00, 0x01, 0x00, 0x03, 0x00, 0x02, 0x03, 0x00, 0x00, 0x00, 0x00,
];

/// Developer safety switch: `VDISPLAY_DRYRUN=1` never touches the port or vendor apps.
pub fn dry_run() -> bool {
    std::env::var_os("VDISPLAY_DRYRUN").is_some()
}

/// Builds header + JPEG into `out` (reused between frames to avoid allocations).
pub fn build_frame(jpeg: &[u8], first: bool, out: &mut Vec<u8>) {
    out.clear();
    out.extend_from_slice(&HEADER);
    out[0] = if first { 0x03 } else { 0x00 };
    let len = jpeg.len() as u32;
    out[8..12].copy_from_slice(&(len + 60).to_le_bytes());
    out[56..60].copy_from_slice(&len.to_le_bytes());
    out.extend_from_slice(jpeg);
}

/// Finds the screen's COM port. Only ports that are actually present are returned,
/// never stale registry entries for unplugged devices.
pub fn find_port() -> Option<String> {
    if dry_run() {
        return None;
    }
    let ports = serialport::available_ports().unwrap_or_default();
    for p in &ports {
        if let serialport::SerialPortType::UsbPort(u) = &p.port_type
            && u.vid == VID
            && u.pid == PID
        {
            return Some(p.port_name.clone());
        }
    }
    // Fallback: registry lookup, cross-checked against the present ports.
    let present: Vec<String> = ports.iter().map(|p| p.port_name.to_uppercase()).collect();
    registry_ports()
        .into_iter()
        .find(|p| present.contains(&p.to_uppercase()))
}

fn registry_ports() -> Vec<String> {
    use winreg::RegKey;
    use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_READ};
    let mut out = Vec::new();
    let base = r"SYSTEM\CurrentControlSet\Enum\USB";
    let Ok(usb) = RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey_with_flags(base, KEY_READ) else {
        return out;
    };
    for dev in usb.enum_keys().flatten() {
        if !dev
            .to_uppercase()
            .contains(&format!("VID_{VID:04X}&PID_{PID:04X}"))
        {
            continue;
        }
        let Ok(dk) = usb.open_subkey_with_flags(&dev, KEY_READ) else {
            continue;
        };
        for inst in dk.enum_keys().flatten() {
            if let Ok(pk) =
                dk.open_subkey_with_flags(format!(r"{inst}\Device Parameters"), KEY_READ)
                && let Ok(name) = pk.get_value::<String, _>("PortName")
                && !name.is_empty()
            {
                out.push(name);
            }
        }
    }
    out
}

pub enum WriteError {
    Timeout,
    Fatal(String),
}

pub struct Link {
    port: serialport::COMPort,
    pub name: String,
}

impl Link {
    pub fn open(name: &str) -> Result<Self, String> {
        let mut port = serialport::new(name, 115_200)
            .timeout(Duration::from_millis(1500))
            .open_native()
            .map_err(|e| e.to_string())?;
        unsafe {
            let h = windows::Win32::Foundation::HANDLE(port.as_raw_handle());
            let _ = windows::Win32::Devices::Communication::SetupComm(h, 16_384, 262_144);
        }
        // DTR/RTS high so the CDC firmware knows a host is attached.
        let _ = port.write_data_terminal_ready(true);
        let _ = port.write_request_to_send(true);
        let _ = port.clear(serialport::ClearBuffer::All);
        Ok(Self {
            port,
            name: name.to_string(),
        })
    }

    /// Bytes still queued for transmission (USB backpressure indicator).
    pub fn queued(&self) -> u32 {
        self.port.bytes_to_write().unwrap_or(0)
    }

    /// Reads and discards anything the MCU sent so its FIFO never fills.
    pub fn drain_input(&mut self) {
        if let Ok(n) = self.port.bytes_to_read()
            && n > 0
        {
            let mut buf = vec![0u8; n as usize];
            let _ = std::io::Read::read(&mut self.port, &mut buf);
        }
    }

    pub fn reset_output(&mut self) {
        let _ = self.port.clear(serialport::ClearBuffer::Output);
    }

    /// Writes a whole packet. On Windows a COMMTIMEOUTS expiry shows up as a short (or zero)
    /// write rather than an error, so both are reported as `Timeout`.
    pub fn write(&mut self, packet: &[u8]) -> Result<(), WriteError> {
        let mut rest = packet;
        while !rest.is_empty() {
            match self.port.write(rest) {
                Ok(0) => return Err(WriteError::Timeout),
                Ok(n) => rest = &rest[n..],
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WriteZero
                    ) =>
                {
                    return Err(WriteError::Timeout);
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(WriteError::Fatal(e.to_string())),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn header_layout() {
        let mut out = Vec::new();
        super::build_frame(&[1, 2, 3], true, &mut out);
        assert_eq!(out.len(), 63);
        assert_eq!(out[0], 3);
        assert_eq!(u32::from_le_bytes(out[8..12].try_into().unwrap()), 63);
        assert_eq!(u32::from_le_bytes(out[56..60].try_into().unwrap()), 3);
        assert_eq!(&out[17..20], b"OT\x06");
    }
}
