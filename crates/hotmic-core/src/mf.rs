//! Windows Media Foundation fallback decoder.
//!
//! symphonia covers MP3, WAV, FLAC, OGG and AAC-LC. Files it refuses (HE-AAC "mp3"s from
//! video rips, WMA, anything exotic) are handed to the OS decoder, which is what Windows
//! Media Player and Edge use. Output is interleaved f32 at the file's native rate.

use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use windows::core::PCWSTR;
use windows::Win32::Media::MediaFoundation::{
    IMFMediaBuffer, IMFSample, IMFSourceReader, MFAudioFormat_Float, MFCreateMediaType,
    MFCreateSourceReaderFromURL, MFMediaType_Audio, MFStartup, MFSTARTUP_NOSOCKET, MF_API_VERSION,
    MF_MT_AUDIO_NUM_CHANNELS, MF_MT_AUDIO_SAMPLES_PER_SECOND, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE,
    MF_SDK_VERSION, MF_SOURCE_READERF_ENDOFSTREAM, MF_SOURCE_READER_FIRST_AUDIO_STREAM,
};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

/// Returns (interleaved f32 samples, sample rate, channel count).
pub fn decode(path: &Path) -> Result<(Vec<f32>, u32, usize)> {
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    unsafe {
        // COM may already be initialised on this thread (Tauri does it); that is fine.
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        MFStartup((MF_SDK_VERSION << 16) | MF_API_VERSION, MFSTARTUP_NOSOCKET)
            .context("MFStartup")?;

        let reader: IMFSourceReader = MFCreateSourceReaderFromURL(PCWSTR(wide.as_ptr()), None)
            .context("Media Foundation could not open the file")?;
        let stream = MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32;

        // Ask for float PCM; the reader inserts the decoder and converter for us.
        let wanted = MFCreateMediaType().context("MFCreateMediaType")?;
        wanted
            .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)
            .context("set major type")?;
        wanted
            .SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_Float)
            .context("set subtype")?;
        reader
            .SetCurrentMediaType(stream, None, &wanted)
            .context("Media Foundation has no decoder for this file")?;

        let actual = reader.GetCurrentMediaType(stream).context("get media type")?;
        let rate = actual
            .GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND)
            .context("sample rate")?;
        let channels = actual
            .GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS)
            .context("channel count")? as usize;
        if rate == 0 || channels == 0 {
            return Err(anyhow!("Media Foundation reported an empty audio format"));
        }

        let mut out: Vec<f32> = Vec::new();
        loop {
            let mut flags: u32 = 0;
            let mut sample: Option<IMFSample> = None;
            reader
                .ReadSample(stream, 0, None, Some(&mut flags), None, Some(&mut sample))
                .context("ReadSample")?;
            if flags & (MF_SOURCE_READERF_ENDOFSTREAM.0 as u32) != 0 {
                break;
            }
            let Some(sample) = sample else { continue };
            let buffer: IMFMediaBuffer = sample
                .ConvertToContiguousBuffer()
                .context("ConvertToContiguousBuffer")?;
            let mut ptr: *mut u8 = std::ptr::null_mut();
            let mut len: u32 = 0;
            buffer
                .Lock(&mut ptr, None, Some(&mut len))
                .context("buffer lock")?;
            let floats = std::slice::from_raw_parts(ptr as *const f32, len as usize / 4);
            out.extend_from_slice(floats);
            let _ = buffer.Unlock();
        }
        Ok((out, rate, channels))
    }
}
