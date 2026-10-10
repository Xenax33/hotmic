//! Audio device enumeration and lookup.

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait};
use serde::Serialize;

/// Substring that identifies VB-Cable's playback endpoint (what we render into).
pub const VB_CABLE_RENDER: &str = "CABLE Input";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum DeviceKind {
    Input,
    Output,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeviceInfo {
    /// Stable identifier, pass it back to [`find`] to pick this device again.
    pub id: String,
    pub name: String,
    pub is_input: bool,
    pub is_output: bool,
    pub is_default_input: bool,
    pub is_default_output: bool,
    pub input_channels: u16,
    pub output_channels: u16,
    pub input_sample_rate: Option<u32>,
    pub output_sample_rate: Option<u32>,
}

fn device_id(device: &cpal::Device) -> Option<String> {
    device.id().ok().map(|id| id.to_string())
}

/// List every device the default host knows about.
pub fn list() -> Result<Vec<DeviceInfo>> {
    let host = cpal::default_host();
    let default_in = host.default_input_device().and_then(|d| device_id(&d));
    let default_out = host.default_output_device().and_then(|d| device_id(&d));

    let mut out = Vec::new();
    for device in host.devices().context("enumerating audio devices")? {
        let Some(id) = device_id(&device) else { continue };
        let name = device
            .description()
            .map(|d| d.name().to_string())
            .unwrap_or_else(|_| id.clone());
        let in_cfg = device.default_input_config().ok();
        let out_cfg = device.default_output_config().ok();
        out.push(DeviceInfo {
            is_default_input: default_in.as_deref() == Some(id.as_str()),
            is_default_output: default_out.as_deref() == Some(id.as_str()),
            is_input: in_cfg.is_some(),
            is_output: out_cfg.is_some(),
            input_channels: in_cfg.as_ref().map(|c| c.channels()).unwrap_or(0),
            output_channels: out_cfg.as_ref().map(|c| c.channels()).unwrap_or(0),
            input_sample_rate: in_cfg.as_ref().map(|c| c.sample_rate()),
            output_sample_rate: out_cfg.as_ref().map(|c| c.sample_rate()),
            id,
            name,
        });
    }
    Ok(out)
}

/// Resolve a device from a user supplied spec.
///
/// `spec` may be empty (system default for `kind`), an exact id from [`DeviceInfo::id`], or a
/// case-insensitive substring of the device name. The first matching device of the right kind wins.
pub fn find(spec: &str, kind: DeviceKind) -> Result<cpal::Device> {
    let host = cpal::default_host();
    let supports = |d: &cpal::Device| match kind {
        DeviceKind::Input => d.default_input_config().is_ok(),
        DeviceKind::Output => d.default_output_config().is_ok(),
    };

    if spec.trim().is_empty() {
        return match kind {
            DeviceKind::Input => host.default_input_device(),
            DeviceKind::Output => host.default_output_device(),
        }
        .ok_or_else(|| anyhow!("no default {kind:?} device"));
    }

    let wanted = spec.to_lowercase();
    let mut by_name = None;
    for device in host.devices().context("enumerating audio devices")? {
        if !supports(&device) {
            continue;
        }
        if device_id(&device).as_deref() == Some(spec) {
            return Ok(device);
        }
        if by_name.is_none() {
            let name = device
                .description()
                .map(|d| d.name().to_lowercase())
                .unwrap_or_default();
            if name.contains(&wanted) {
                by_name = Some(device);
            }
        }
    }
    by_name.ok_or_else(|| anyhow!("no {kind:?} device matching '{spec}'"))
}

/// The VB-Cable render endpoint, if the driver is installed.
pub fn find_vb_cable() -> Option<DeviceInfo> {
    list()
        .ok()?
        .into_iter()
        .find(|d| d.is_output && d.name.contains(VB_CABLE_RENDER))
}
