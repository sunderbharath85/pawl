//! Microphone capture with cpal. The stream's data callback (cpal's own audio thread) only down-mixes to mono f32
//! and hands the block to the voice worker; nothing is stored here and nothing is written anywhere. The worker owns
//! the `Stream` and drops it to release the device.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample};
use serde::Serialize;
use std::sync::mpsc::SyncSender;

/// What the capture callback sends the worker.
pub enum CaptureMsg {
    /// Mono samples at the stream's rate, tagged with the stream's generation (late blocks of a closed stream are
    /// dropped by the worker).
    Audio { generation: u64, samples: Vec<f32> },
    /// The stream failed (device unplugged, access revoked).
    Failed { generation: u64, message: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MicInfo {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

/// The input devices of the default audio host. Never opens a stream.
pub fn list_mics() -> Vec<MicInfo> {
    let host = cpal::default_host();
    let default_id = host.default_input_device().and_then(|d| d.id().ok()).map(|id| id.to_string());
    let Ok(devices) = host.input_devices() else {
        return Vec::new();
    };
    let mut out: Vec<MicInfo> = devices
        .filter_map(|d| {
            let id = d.id().ok()?.to_string();
            let name = d.description().map(|desc| desc.name().to_string()).unwrap_or_else(|_| id.clone());
            let is_default = default_id.as_deref() == Some(id.as_str());
            Some(MicInfo { id, name, is_default })
        })
        .collect();
    out.sort_by(|a, b| b.is_default.cmp(&a.is_default).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    out.dedup_by(|a, b| a.id == b.id);
    out
}

/// An open microphone stream. Dropping it closes the device.
pub struct Capture {
    _stream: cpal::Stream,
    pub sample_rate: u32,
}

fn friendly(err: &cpal::Error) -> String {
    match err.kind() {
        cpal::ErrorKind::PermissionDenied => {
            "Pawl isn't allowed to use the microphone. Allow it in the system's privacy settings.".to_string()
        }
        cpal::ErrorKind::DeviceBusy => "The microphone is busy in another app.".to_string(),
        cpal::ErrorKind::DeviceNotAvailable => "The microphone is not available (unplugged?).".to_string(),
        _ => format!("Could not open the microphone: {err}"),
    }
}

/// Open `mic_id` (a cpal device id; `None`, or a device that is gone, means the system default input).
pub fn open(mic_id: Option<&str>, generation: u64, tx: SyncSender<super::engine::Msg>) -> Result<Capture, String> {
    let host = cpal::default_host();
    let chosen = mic_id.and_then(|id| id.parse::<cpal::DeviceId>().ok()).and_then(|id| host.device_by_id(&id));
    let device = chosen.or_else(|| host.default_input_device()).ok_or("No microphone found.")?;
    let supported = device.default_input_config().map_err(|e| friendly(&e))?;
    let sample_rate = supported.sample_rate();
    let channels = usize::from(supported.channels().max(1));
    let config = supported.config();

    let stream = match supported.sample_format() {
        SampleFormat::F32 => build::<f32>(&device, config, channels, generation, tx),
        SampleFormat::I16 => build::<i16>(&device, config, channels, generation, tx),
        SampleFormat::I32 => build::<i32>(&device, config, channels, generation, tx),
        SampleFormat::U16 => build::<u16>(&device, config, channels, generation, tx),
        SampleFormat::U8 => build::<u8>(&device, config, channels, generation, tx),
        SampleFormat::I8 => build::<i8>(&device, config, channels, generation, tx),
        SampleFormat::F64 => build::<f64>(&device, config, channels, generation, tx),
        other => return Err(format!("The microphone uses an unsupported sample format ({other:?}).")),
    }
    .map_err(|e| friendly(&e))?;
    stream.play().map_err(|e| friendly(&e))?;
    Ok(Capture { _stream: stream, sample_rate })
}

fn build<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    channels: usize,
    generation: u64,
    tx: SyncSender<super::engine::Msg>,
) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample + Send + 'static,
    f32: FromSample<T>,
{
    let err_tx = tx.clone();
    device.build_input_stream::<T, _, _>(
        config,
        move |data: &[T], _: &cpal::InputCallbackInfo| {
            let samples = downmix(data, channels);
            // Never block the audio thread: if the worker is behind, this block is dropped.
            let _ = tx.try_send(super::engine::Msg::Capture(CaptureMsg::Audio { generation, samples }));
        },
        move |err: cpal::Error| match err.kind() {
            cpal::ErrorKind::Xrun | cpal::ErrorKind::DeviceChanged | cpal::ErrorKind::RealtimeDenied => {}
            _ => {
                let _ = err_tx
                    .try_send(super::engine::Msg::Capture(CaptureMsg::Failed { generation, message: friendly(&err) }));
            }
        },
        None,
    )
}

/// Interleaved frames -> mono f32 (the mean of the channels).
pub fn downmix<T>(data: &[T], channels: usize) -> Vec<f32>
where
    T: Sample,
    f32: FromSample<T>,
{
    let ch = channels.max(1);
    let scale = 1.0 / ch as f32;
    data.chunks(ch).map(|frame| frame.iter().map(|s| s.to_sample::<f32>()).sum::<f32>() * scale).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downmix_averages_channels_and_converts() {
        assert_eq!(downmix(&[0.5f32, -0.5, 1.0, 0.0], 2), vec![0.0, 0.5]);
        assert_eq!(downmix(&[0.25f32, 0.75], 1), vec![0.25, 0.75]);
        let m = downmix(&[i16::MAX, i16::MAX, i16::MIN, i16::MIN], 2);
        assert!((m[0] - 1.0).abs() < 1e-3 && (m[1] + 1.0).abs() < 1e-3, "{m:?}");
        assert_eq!(downmix::<f32>(&[], 2), Vec::<f32>::new());
    }
}
