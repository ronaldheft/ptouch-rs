// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Huang Rui <vowstar@gmail.com>
// SPDX-FileCopyrightText: Dominic Radermacher and the ptouch-print contributors
//
// Portions derived from ptouch-print, licensed GPL-3.0-or-later:
// https://git.familie-radermacher.ch/linux/ptouch-print.git

//! USB transport layer for Brother P-Touch printers.
//!
//! Provides the [`PtouchDevice`] struct for opening, initializing, and
//! communicating with a P-Touch printer over USB.

use std::time::{Duration, Instant};

use log::{debug, info, warn};
use rusb::{Context, DeviceHandle, UsbContext};

use crate::device::{self, BROTHER_VENDOR_ID, DeviceFlags, DeviceInfo};
use crate::error::{PtouchError, Result};
use crate::protocol;
use crate::status::{PrinterStatus, STATUS_PACKET_SIZE};
use crate::tape;

/// Default USB timeout for bulk transfers.
const USB_TIMEOUT: Duration = Duration::from_secs(5);

/// Short timeout for flushing stale USB data.
const USB_FLUSH_TIMEOUT: Duration = Duration::from_millis(100);

/// Delay between status read retries.
const STATUS_RETRY_DELAY: Duration = Duration::from_millis(100);

/// Maximum number of status read retries.
const STATUS_MAX_RETRIES: usize = 10;

/// Maximum time to process automatic status transfers after a print command.
const PRINT_STATUS_TIMEOUT: Duration = Duration::from_secs(30);

/// Bound each USB read so transient silence cannot consume the whole print
/// lifecycle deadline in a single transfer.
const PRINT_STATUS_POLL_TIMEOUT: Duration = Duration::from_millis(250);

/// USB interface number for P-Touch printers.
const USB_INTERFACE: u8 = 0;

/// A connection to a Brother P-Touch USB printer.
pub struct PtouchDevice {
    /// USB device handle.
    handle: DeviceHandle<Context>,
    /// Device information from the supported device table.
    dev_info: DeviceInfo,
    /// Bulk OUT endpoint address.
    ep_out: u8,
    /// Bulk IN endpoint address.
    ep_in: u8,
    /// Most recently read printer status.
    status: Option<PrinterStatus>,
    /// Tape width in pixels (resolved after status query).
    tape_width_px: Option<u16>,
    /// Whether the device has been initialized.
    initialized: bool,
}

impl PtouchDevice {
    /// Open a P-Touch printer by USB vendor/product ID.
    ///
    /// Scans the USB bus for a device matching the given VID/PID, looks it up
    /// in the supported device table, claims the USB interface, and returns
    /// a [`PtouchDevice`] ready for initialization.
    ///
    /// # Errors
    ///
    /// Returns [`PtouchError::DeviceNotFound`] if no matching USB device is
    /// found or the device is not in the supported table. Returns
    /// [`PtouchError::PLiteMode`] if the device is in PLite mode. Returns
    /// [`PtouchError::UnsupportedRaster`] if the device does not support
    /// raster printing.
    pub fn open(vid: u16, pid: u16) -> Result<Self> {
        let dev_info = device::find_device(vid, pid)
            .ok_or(PtouchError::DeviceNotFound)?
            .clone();

        if dev_info.flags.contains(DeviceFlags::PLITE) {
            return Err(PtouchError::PLiteMode(dev_info.name.to_string()));
        }

        if dev_info.flags.contains(DeviceFlags::UNSUP_RASTER) {
            return Err(PtouchError::UnsupportedRaster(dev_info.name.to_string()));
        }

        info!(
            "Opening device: {} (VID={:#06x}, PID={:#06x})",
            dev_info.name, vid, pid
        );

        let context = Context::new()?;
        let handle = context
            .open_device_with_vid_pid(vid, pid)
            .ok_or(PtouchError::DeviceNotFound)?;

        // Detach kernel driver if active (non-fatal)
        if handle.kernel_driver_active(USB_INTERFACE).unwrap_or(false) {
            debug!("Detaching kernel driver from interface {}", USB_INTERFACE);
            if let Err(e) = handle.detach_kernel_driver(USB_INTERFACE) {
                warn!("Failed to detach kernel driver: {} (continuing)", e);
            }
        }

        handle.claim_interface(USB_INTERFACE)?;

        // Find the bulk endpoints
        let (ep_out, ep_in) = find_bulk_endpoints(&handle)?;
        debug!("Endpoints: OUT={:#04x}, IN={:#04x}", ep_out, ep_in);

        Ok(PtouchDevice {
            handle,
            dev_info,
            ep_out,
            ep_in,
            status: None,
            tape_width_px: None,
            initialized: false,
        })
    }

    /// Open the first Brother P-Touch printer found on the USB bus.
    ///
    /// Scans all USB devices, looking for any with the Brother vendor ID
    /// that matches an entry in the supported device table.
    pub fn open_first() -> Result<Self> {
        let context = Context::new()?;
        let devices = context.devices()?;

        for usb_dev in devices.iter() {
            let desc = match usb_dev.device_descriptor() {
                Ok(d) => d,
                Err(_) => continue,
            };

            if desc.vendor_id() != BROTHER_VENDOR_ID {
                continue;
            }

            if let Some(dev_info) = device::find_device(desc.vendor_id(), desc.product_id()) {
                if dev_info.flags.contains(DeviceFlags::PLITE)
                    || dev_info.flags.contains(DeviceFlags::UNSUP_RASTER)
                {
                    continue;
                }

                return Self::open(desc.vendor_id(), desc.product_id());
            }
        }

        Err(PtouchError::DeviceNotFound)
    }

    /// Get a reference to the device info.
    pub fn device_info(&self) -> &DeviceInfo {
        &self.dev_info
    }

    /// Get the device flags.
    pub fn flags(&self) -> DeviceFlags {
        self.dev_info.flags
    }

    /// Get the most recently read printer status, if available.
    pub fn status(&self) -> Option<&PrinterStatus> {
        self.status.as_ref()
    }

    /// Get the tape width in pixels, if known.
    pub fn tape_width_px(&self) -> Option<u16> {
        self.tape_width_px
    }

    /// Get the maximum printable pixels for this device.
    pub fn max_px(&self) -> u16 {
        self.dev_info.max_px
    }

    /// Whether the device has been initialized.
    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

    /// Send raw bytes to the printer (bulk OUT transfer).
    pub fn send(&self, data: &[u8]) -> Result<()> {
        let written = self
            .handle
            .write_bulk(self.ep_out, data, USB_TIMEOUT)
            .map_err(|e| {
                if e == rusb::Error::Timeout {
                    PtouchError::Timeout
                } else {
                    PtouchError::UsbError(e)
                }
            })?;

        if written != data.len() {
            return Err(PtouchError::SendFailed(format!(
                "Expected to write {} bytes, wrote {}",
                data.len(),
                written
            )));
        }

        Ok(())
    }

    /// Receive raw bytes from the printer (bulk IN transfer).
    ///
    /// Returns the number of bytes actually read into `buf`.
    pub fn receive(&self, buf: &mut [u8]) -> Result<usize> {
        self.receive_with_timeout(buf, USB_TIMEOUT)
    }

    /// Receive raw bytes with a caller-provided timeout.
    fn receive_with_timeout(&self, buf: &mut [u8], timeout: Duration) -> Result<usize> {
        let read = self
            .handle
            .read_bulk(self.ep_in, buf, timeout)
            .map_err(|e| {
                if e == rusb::Error::Timeout {
                    PtouchError::Timeout
                } else {
                    PtouchError::UsbError(e)
                }
            })?;

        Ok(read)
    }

    /// Flush stale data from the USB IN endpoint.
    ///
    /// Performs short-timeout reads and discards any data until the pipe
    /// is empty. This prevents stale responses from confusing subsequent
    /// command/response exchanges.
    fn flush_input(&self) {
        let mut buf = [0u8; 64];
        loop {
            match self
                .handle
                .read_bulk(self.ep_in, &mut buf, USB_FLUSH_TIMEOUT)
            {
                Ok(n) if n > 0 => {
                    debug!("Flushed {} stale bytes from USB IN", n);
                }
                _ => break,
            }
        }
    }

    /// Initialize the printer.
    ///
    /// Sends the init sequence (100 zeros + ESC @) and queries the status.
    /// Raster start is sent per-job in `print_raster()`.
    pub fn init(&mut self) -> Result<()> {
        // Flush any stale data from previous sessions
        self.flush_input();

        // Send the init command (100 zeros + ESC @)
        self.send(&protocol::cmd_init())?;

        // Request and read status
        self.get_status()?;

        if self
            .dev_info
            .flags
            .contains(DeviceFlags::AUTO_STATUS_NOTIFICATION)
        {
            // Enable the phase changes used by the post-print readiness handshake.
            self.send(&protocol::cmd_auto_status_notification(true))?;
        }

        self.initialized = true;
        info!(
            "Device initialized: {}, tape={}mm ({}px)",
            self.dev_info.name,
            self.status.as_ref().map_or(0, |s| s.media_width),
            self.tape_width_px.unwrap_or(0)
        );

        Ok(())
    }

    /// Query printer status without sending the init command.
    ///
    /// Flushes stale USB data and reads the printer status. Unlike
    /// [`init`](Self::init), this does not send the 100-zero + ESC @
    /// reset sequence, so it will not disturb the printer.
    pub fn query_status(&mut self) -> Result<&PrinterStatus> {
        self.flush_input();
        self.get_status()
    }

    /// Request and read the printer status.
    ///
    /// Sends the status request command and reads the 32-byte response.
    /// Retries up to STATUS_MAX_RETRIES times with STATUS_RETRY_DELAY
    /// between attempts.
    /// Updates internal status and tape width fields.
    pub fn get_status(&mut self) -> Result<&PrinterStatus> {
        self.send(&protocol::cmd_status_request())?;

        let mut buf = [0u8; STATUS_PACKET_SIZE];
        let mut frames = StatusFrameBuffer::new();
        let mut response = None;

        // Retry loop: sleep then read
        for attempt in 0..STATUS_MAX_RETRIES {
            std::thread::sleep(STATUS_RETRY_DELAY);

            match self.handle.read_bulk(self.ep_in, &mut buf, USB_TIMEOUT) {
                Ok(0) => {
                    debug!("Empty status read (attempt {})", attempt + 1);
                    continue;
                }
                Ok(n) => {
                    frames.push(&buf[..n]);
                    response = frames.pop();
                }
                Err(rusb::Error::Timeout) => {
                    debug!("Status read timeout (attempt {})", attempt + 1);
                    continue;
                }
                Err(e) => return Err(PtouchError::UsbError(e)),
            }

            if response.is_some() {
                break;
            }
            debug!(
                "Short status read ({} bytes, attempt {})",
                frames.len(),
                attempt + 1
            );
        }

        let Some(response) = response else {
            // Flush junk data before returning error
            self.flush_input();
            return Err(PtouchError::StatusError(format!(
                "Status packet too short: {} bytes (expected {})",
                frames.len(),
                STATUS_PACKET_SIZE
            )));
        };

        let status = match parse_status_packet(&response, "Invalid status header") {
            Ok(status) => status,
            Err(error) => {
                self.flush_input();
                return Err(error);
            }
        };

        debug!(
            "Status: type={}, media_width={}mm, media_type={}, tape_color={}, text_color={}",
            status.status_type_name(),
            status.media_width,
            status.media_type_name(),
            status.tape_color_name(),
            status.text_color_name()
        );

        if status.has_error() {
            warn!("Printer reports error: {}", status.error_description());
        }

        // Resolve tape width to pixel count for this printer's resolution,
        // clamped to the head width (wide tapes exceed narrow heads).
        self.tape_width_px = tape::tape_pixels(status.media_width, self.dev_info.dpi)
            .map(|px| px.min(self.dev_info.max_px));
        if self.tape_width_px.is_none() && status.media_width != 0 {
            warn!("Unknown tape width: {} mm", status.media_width);
        }

        self.status = Some(status);

        // The unwrap is safe because we just assigned Some above
        Ok(self.status.as_ref().unwrap())
    }

    /// Print raster image data.
    ///
    /// `lines` is a slice of raster line buffers, each `ceil(max_px/8)` bytes
    /// wide. The printer will print one raster line per entry.
    ///
    /// # Arguments
    /// * `lines` - Raster image data, one byte-slice per line.
    /// * `chain_print` - If true, don't cut the tape (chain mode).
    /// * `precut` - If true AND device supports precut, send precut command.
    /// * `quality` - Print quality mode (device must support non-standard).
    ///
    /// # Errors
    ///
    /// Returns [`PtouchError::NotInitialized`] if [`init`](Self::init) was
    /// not called, or [`PtouchError::UnsupportedQuality`] if a non-standard
    /// quality is requested on a device without quality modes.
    pub fn print_raster(
        &mut self,
        lines: &[Vec<u8>],
        chain_print: bool,
        precut: bool,
        quality: protocol::PrintQuality,
    ) -> Result<()> {
        if !self.initialized {
            return Err(PtouchError::NotInitialized);
        }

        if quality != protocol::PrintQuality::Standard
            && !self.dev_info.flags.contains(DeviceFlags::LEGACY_HIRES)
        {
            return Err(PtouchError::UnsupportedQuality(
                self.dev_info.name.to_string(),
            ));
        }

        let opts = protocol::JobOptions {
            media_width: self.status.as_ref().map_or(0, |s| s.media_width),
            chain_print,
            precut,
            quality,
        };

        let job = protocol::build_print_job(lines, self.dev_info.flags, &opts);
        self.send_job(job)?;

        if self
            .dev_info
            .flags
            .contains(DeviceFlags::WAIT_FOR_RECEIVE_READY)
        {
            self.wait_until_ready()
        } else {
            self.receive_print_completion()
        }
    }

    /// Feed tape forward and cut.
    ///
    /// Prints a minimal blank strip (a few blank raster lines) then
    /// ejects and cuts. The printer needs actual raster data to engage
    /// the feed mechanism.
    pub fn feed_and_cut(&mut self) -> Result<()> {
        if !self.initialized {
            return Err(PtouchError::NotInitialized);
        }

        // One blank line makes the printer engage the feed mechanism.
        let lines = vec![protocol::rasterline_blank(self.dev_info.max_px)];
        let opts = protocol::JobOptions {
            media_width: self.status.as_ref().map_or(0, |s| s.media_width),
            ..protocol::JobOptions::default()
        };

        let job = protocol::build_print_job(&lines, self.dev_info.flags, &opts);
        self.send_job(job)?;

        if self
            .dev_info
            .flags
            .contains(DeviceFlags::WAIT_FOR_RECEIVE_READY)
        {
            self.wait_until_ready()?;
        }

        info!("Feed and cut");
        Ok(())
    }

    fn send_job(&self, job: Vec<Vec<u8>>) -> Result<()> {
        for chunk in job {
            self.send(&chunk)?;
        }

        Ok(())
    }

    fn wait_until_ready(&mut self) -> Result<()> {
        let status = receive_print_status(|buf, timeout| self.receive_with_timeout(buf, timeout))?;
        self.status = Some(status);
        Ok(())
    }

    /// Preserve the original best-effort completion read for models whose
    /// readiness lifecycle has not been documented or tested.
    fn receive_print_completion(&mut self) -> Result<()> {
        let mut response = [0u8; STATUS_PACKET_SIZE];
        match self.receive(&mut response) {
            Ok(n) if n >= STATUS_PACKET_SIZE => {
                if let Some(status) = PrinterStatus::from_bytes(&response) {
                    if status.has_error() {
                        return Err(PtouchError::StatusError(status.error_description()));
                    }
                    debug!("Print completed: status_type={}", status.status_type_name());
                    self.status = Some(status);
                }
            }
            Ok(n) => {
                debug!("Short status response after print: {} bytes", n);
            }
            Err(PtouchError::Timeout) => {
                debug!("Timeout waiting for print completion status");
            }
            Err(error) => return Err(error),
        }

        Ok(())
    }

    /// Release the USB interface and close the device.
    pub fn close(self) -> Result<()> {
        self.handle.release_interface(USB_INTERFACE)?;
        info!("Device closed: {}", self.dev_info.name);
        Ok(())
    }
}

/// Receive the printer's automatic status after a print command.
fn receive_print_status<F>(mut receive: F) -> Result<PrinterStatus>
where
    F: FnMut(&mut [u8], Duration) -> Result<usize>,
{
    receive_print_status_with_timeout(&mut receive, PRINT_STATUS_TIMEOUT)
}

fn receive_print_status_with_timeout<F>(mut receive: F, timeout: Duration) -> Result<PrinterStatus>
where
    F: FnMut(&mut [u8], Duration) -> Result<usize>,
{
    let started = Instant::now();
    let mut frames = StatusFrameBuffer::new();

    loop {
        let Some(remaining) = timeout.checked_sub(started.elapsed()) else {
            return Err(PtouchError::Timeout);
        };

        let mut transfer = [0u8; STATUS_PACKET_SIZE];
        let read_timeout = remaining.min(PRINT_STATUS_POLL_TIMEOUT);
        let read = match receive(&mut transfer, read_timeout) {
            Ok(read) => read,
            Err(PtouchError::Timeout) => {
                debug!("No print status available yet");
                continue;
            }
            Err(error) => return Err(error),
        };

        if read > transfer.len() {
            return Err(PtouchError::StatusError(format!(
                "USB read reported {} bytes for a {}-byte buffer",
                read,
                transfer.len()
            )));
        }

        // A successful zero-byte bulk transfer is USB framing, not a Brother
        // status packet. Keep waiting within the overall deadline.
        if read == 0 {
            debug!("Ignoring zero-length USB transfer after print");
            continue;
        }

        frames.push(&transfer[..read]);
        if frames.len() < STATUS_PACKET_SIZE {
            debug!(
                "Accumulated {} of {} print-status bytes",
                frames.len(),
                STATUS_PACKET_SIZE
            );
            continue;
        }

        let Some(response) = frames.pop() else {
            continue;
        };
        let status = parse_status_packet(&response, "Invalid status header after print")?;

        debug!(
            "Print status: type={}, phase_type={:#04x}, phase={:#04x}{:02x}",
            status.status_type_name(),
            status.phase_type,
            status.phase_number_hi,
            status.phase_number_lo
        );

        if print_status_is_ready(&status)? {
            debug!("Printer is ready to receive the next page");
            return Ok(status);
        }
    }
}

struct StatusFrameBuffer {
    pending: Vec<u8>,
}

impl StatusFrameBuffer {
    fn new() -> Self {
        Self {
            pending: Vec::with_capacity(STATUS_PACKET_SIZE * 2),
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        self.pending.extend_from_slice(bytes);
    }

    fn len(&self) -> usize {
        self.pending.len()
    }

    fn pop(&mut self) -> Option<[u8; STATUS_PACKET_SIZE]> {
        if self.pending.len() < STATUS_PACKET_SIZE {
            return None;
        }

        let mut frame = [0u8; STATUS_PACKET_SIZE];
        frame.copy_from_slice(&self.pending[..STATUS_PACKET_SIZE]);
        self.pending.drain(..STATUS_PACKET_SIZE);
        Some(frame)
    }
}

fn parse_status_packet(
    response: &[u8; STATUS_PACKET_SIZE],
    invalid_header_message: &str,
) -> Result<PrinterStatus> {
    let status = PrinterStatus::from_bytes(response)
        .ok_or_else(|| PtouchError::StatusError("Failed to parse status packet".to_string()))?;

    if status.print_head_mark != 0x80 || status.size != 0x20 {
        return Err(PtouchError::StatusError(format!(
            "{}: mark={:#04x} size={:#04x}",
            invalid_header_message, status.print_head_mark, status.size
        )));
    }

    Ok(status)
}

fn print_status_is_ready(status: &PrinterStatus) -> Result<bool> {
    if status.has_error() || status.status_type == 0x02 {
        let description = if status.has_error() {
            status.error_description()
        } else {
            "Printer reported an unspecified error".to_string()
        };
        return Err(PtouchError::StatusError(description));
    }

    if status.status_type == 0x04 {
        return Err(PtouchError::StatusError("Printer turned off".to_string()));
    }

    Ok(status.is_waiting_to_receive())
}

/// Find the bulk IN and OUT endpoints for the printer interface.
fn find_bulk_endpoints(handle: &DeviceHandle<Context>) -> Result<(u8, u8)> {
    let device = handle.device();
    let config = device.active_config_descriptor()?;

    let mut ep_out: Option<u8> = None;
    let mut ep_in: Option<u8> = None;

    for interface in config.interfaces() {
        for desc in interface.descriptors() {
            if desc.interface_number() != USB_INTERFACE {
                continue;
            }
            for endpoint in desc.endpoint_descriptors() {
                if endpoint.transfer_type() != rusb::TransferType::Bulk {
                    continue;
                }
                match endpoint.direction() {
                    rusb::Direction::Out => {
                        ep_out = Some(endpoint.address());
                    }
                    rusb::Direction::In => {
                        ep_in = Some(endpoint.address());
                    }
                }
            }
        }
    }

    match (ep_out, ep_in) {
        (Some(out), Some(inp)) => Ok((out, inp)),
        _ => Err(PtouchError::DeviceNotFound),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;

    fn status_packet(status_type: u8, phase_type: u8) -> [u8; STATUS_PACKET_SIZE] {
        let mut packet = [0u8; STATUS_PACKET_SIZE];
        packet[0] = 0x80;
        packet[1] = 0x20;
        packet[18] = status_type;
        packet[19] = phase_type;
        packet
    }

    #[test]
    fn print_status_waits_until_reception_is_possible() {
        let mut packets = VecDeque::from([
            status_packet(0x06, 0x01),
            status_packet(0x01, 0x00),
            status_packet(0x06, 0x00),
        ]);

        let status = receive_print_status(|buf, _timeout| {
            let packet = packets.pop_front().ok_or(PtouchError::Timeout)?;
            buf.copy_from_slice(&packet);
            Ok(packet.len())
        })
        .unwrap();

        assert_eq!(status.status_type, 0x06);
        assert_eq!(status.phase_type, 0x00);
        assert!(packets.is_empty());
    }

    #[test]
    fn print_status_does_not_accept_printing_completed_as_ready() {
        let mut packets = VecDeque::from([status_packet(0x01, 0x00)]);

        let result = receive_print_status_with_timeout(
            |buf, _timeout| {
                let packet = packets.pop_front().ok_or(PtouchError::Timeout)?;
                buf.copy_from_slice(&packet);
                Ok(packet.len())
            },
            Duration::from_millis(1),
        );

        assert!(matches!(result, Err(PtouchError::Timeout)));
    }

    #[test]
    fn print_status_propagates_printer_errors() {
        let mut packet = status_packet(0x02, 0x00);
        packet[8] = 0x04;

        let result = receive_print_status(|buf, _timeout| {
            buf.copy_from_slice(&packet);
            Ok(packet.len())
        });

        assert!(matches!(
            result,
            Err(PtouchError::StatusError(message)) if message == "Cutter jam"
        ));
    }

    #[test]
    fn print_status_propagates_power_off() {
        let packet = status_packet(0x04, 0x00);

        let result = receive_print_status(|buf, _timeout| {
            buf.copy_from_slice(&packet);
            Ok(packet.len())
        });

        assert!(matches!(
            result,
            Err(PtouchError::StatusError(message)) if message == "Printer turned off"
        ));
    }

    #[test]
    fn print_status_times_out_after_incomplete_packet() {
        let mut transfers = VecDeque::from([Ok(vec![0u8; 12]), Err(PtouchError::Timeout)]);

        let result = receive_print_status_with_timeout(
            |buf, _timeout| match transfers.pop_front().unwrap_or(Err(PtouchError::Timeout)) {
                Ok(transfer) => {
                    buf[..transfer.len()].copy_from_slice(&transfer);
                    Ok(transfer.len())
                }
                Err(error) => Err(error),
            },
            Duration::from_millis(1),
        );

        assert!(matches!(result, Err(PtouchError::Timeout)));
    }

    #[test]
    fn print_status_ignores_zero_length_usb_transfers() {
        let mut transfers = VecDeque::from([
            Vec::new(),
            status_packet(0x06, 0x01).to_vec(),
            status_packet(0x01, 0x00).to_vec(),
            status_packet(0x06, 0x00).to_vec(),
        ]);

        let status = receive_print_status(|buf, _timeout| {
            let transfer = transfers.pop_front().ok_or(PtouchError::Timeout)?;
            buf[..transfer.len()].copy_from_slice(&transfer);
            Ok(transfer.len())
        })
        .unwrap();

        assert!(status.is_waiting_to_receive());
        assert!(transfers.is_empty());
    }

    #[test]
    fn print_status_accumulates_fragmented_packets() {
        let ready = status_packet(0x06, 0x00);
        let mut transfers = VecDeque::from([ready[..11].to_vec(), ready[11..].to_vec()]);

        let status = receive_print_status(|buf, _timeout| {
            let transfer = transfers.pop_front().ok_or(PtouchError::Timeout)?;
            buf[..transfer.len()].copy_from_slice(&transfer);
            Ok(transfer.len())
        })
        .unwrap();

        assert!(status.is_waiting_to_receive());
        assert!(transfers.is_empty());
    }

    #[test]
    fn status_frame_buffer_preserves_partial_next_packet() {
        let first = status_packet(0x01, 0x00);
        let second = status_packet(0x06, 0x00);
        let mut frames = StatusFrameBuffer::new();

        frames.push(&first[..9]);
        frames.push(&[first[9..].as_ref(), second[..7].as_ref()].concat());

        assert_eq!(frames.pop(), Some(first));
        assert_eq!(frames.len(), 7);

        frames.push(&second[7..]);
        assert_eq!(frames.pop(), Some(second));
        assert_eq!(frames.len(), 0);
    }

    #[test]
    fn print_status_tolerates_transient_usb_timeouts() {
        let ready = status_packet(0x06, 0x00);
        let mut transfers = VecDeque::from([
            Err(PtouchError::Timeout),
            Err(PtouchError::Timeout),
            Ok(ready.to_vec()),
        ]);

        let status = receive_print_status(|buf, _timeout| {
            match transfers.pop_front().ok_or(PtouchError::Timeout)? {
                Ok(transfer) => {
                    buf[..transfer.len()].copy_from_slice(&transfer);
                    Ok(transfer.len())
                }
                Err(error) => Err(error),
            }
        })
        .unwrap();

        assert!(status.is_waiting_to_receive());
        assert!(transfers.is_empty());
    }

    #[test]
    fn consecutive_pages_each_wait_for_their_receiving_phase() {
        let mut transfers = VecDeque::from([
            status_packet(0x06, 0x01),
            status_packet(0x01, 0x00),
            status_packet(0x06, 0x00),
            status_packet(0x06, 0x01),
            status_packet(0x01, 0x00),
            status_packet(0x06, 0x00),
        ]);
        let mut receive = |buf: &mut [u8], _timeout: Duration| {
            let packet = transfers.pop_front().ok_or(PtouchError::Timeout)?;
            buf.copy_from_slice(&packet);
            Ok(packet.len())
        };

        let first = receive_print_status(&mut receive).unwrap();
        let second = receive_print_status(&mut receive).unwrap();

        assert!(first.is_waiting_to_receive());
        assert!(second.is_waiting_to_receive());
        assert!(transfers.is_empty());
    }

    #[test]
    fn print_status_has_an_overall_deadline() {
        let result = receive_print_status_with_timeout(|_, _| Ok(0), Duration::ZERO);

        assert!(matches!(result, Err(PtouchError::Timeout)));
    }
}
