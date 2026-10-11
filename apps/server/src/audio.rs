//! Decode exported voice messages into a container a chat gateway accepts.
//!
//! WeCom stores voice as Tencent SILK, which no OpenAI-style endpoint takes, so
//! a SILK file is decoded to 16-bit mono WAV before it is attached. The decoder
//! works on a path, so the bytes are spooled to a scratch file that is deleted
//! as soon as the PCM is back in memory: the archived audio must not outlive the
//! request that carries it.

use std::io::Write;

/// Voices are decoded at 16 kHz mono. Speech survives the downsample, and the
/// result stays small enough to travel with the transcript: about 32 kB of PCM,
/// 43 kB once base64-encoded, per second of speech.
pub const SAMPLE_RATE: u32 = 16_000;

/// Decode one SILK file into WAV bytes.
pub fn silk_to_wav(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let mut scratch = tempfile::Builder::new()
        .prefix("wecom-archive-voice-")
        .suffix(".silk")
        .tempfile()
        .map_err(|_| "无法创建语音解码所需的临时文件。".to_owned())?;
    scratch
        .write_all(bytes)
        .map_err(|_| "无法写入语音解码所需的临时文件。".to_owned())?;
    scratch
        .flush()
        .map_err(|_| "无法写入语音解码所需的临时文件。".to_owned())?;
    let path = scratch.path().to_string_lossy().into_owned();
    let decoded = silk_decoder_rs::silk_to_wav(SAMPLE_RATE, &path).map_err(describe);
    // Dropping the handle removes the file, so it is gone before the request is built.
    drop(scratch);
    let wav = decoded?;
    if wav.is_empty() {
        return Err("语音文件未解码出音频。".to_owned());
    }
    Ok(wav)
}

/// The decoder's own messages embed the scratch path, which is machine specific,
/// so a failure is described in the operator's language instead of echoing it.
fn describe(error: silk_decoder_rs::SilkError) -> String {
    match error {
        silk_decoder_rs::SilkError::InvalidSampleRate => "语音采样率不受支持。".to_owned(),
        silk_decoder_rs::SilkError::InvalidOrEmptySilkFile => {
            "语音文件不是有效的 SILK 数据。".to_owned()
        }
        silk_decoder_rs::SilkError::NoAudioDecoded => "语音文件未解码出音频。".to_owned(),
        silk_decoder_rs::SilkError::ReadFile(_) => "无法读取语音临时文件。".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A payload that is not SILK must fail with a readable reason instead of
    /// producing a WAV the gateway would reject. The success path needs real
    /// SILK, and real chat audio is never checked in, so it is exercised by the
    /// report flow against a real export rather than here.
    #[test]
    fn a_failed_decode_reports_the_reason_and_removes_the_scratch_file() {
        let error = silk_to_wav(b"not a silk file").expect_err("garbage must not decode");
        assert!(error.contains("SILK"), "got {error}");
        let leftovers = std::fs::read_dir(std::env::temp_dir())
            .expect("temp dir")
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("wecom-archive-voice-")
            })
            .count();
        assert_eq!(leftovers, 0, "the scratch file was left behind");
    }
}
