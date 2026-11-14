//! yt-dlp / ffprobe / ffmpeg helpers shared by both bots (`/ytdlp`).
//!
//! Pure functions only: argument building and output parsing. Spawning processes and
//! delivery stay in each bot.

use std::path::Path;

use serde_json::Value;

/// Target video codec for delivered files (`YTDLP_VIDEO_CODEC`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoCodec {
    /// H.264 8-bit yuv420p + AAC in MP4: plays inline in Discord, Telegram and browsers.
    H264,
    /// AV1 8-bit (SVT-AV1) + AAC: smaller files.
    Av1,
    /// Keep whatever the source provides (only remux for fast start).
    Source,
}

impl VideoCodec {
    /// Parses `YTDLP_VIDEO_CODEC`; unknown or missing values give `default`
    /// (each bot keeps its own default).
    pub fn parse(raw: Option<&str>, default: Self) -> Self {
        match raw.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
            Some("h264" | "avc") => Self::H264,
            Some("av1" | "av01") => Self::Av1,
            Some("source" | "copy" | "original") => Self::Source,
            _ => default,
        }
    }
}

/// Codecs of the first video and audio stream (`ffprobe -print_format json -show_streams`).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct StreamInfo {
    pub video: Option<String>,
    pub pix_fmt: Option<String>,
    pub audio: Option<String>,
}

pub fn parse_probe(output: &str) -> StreamInfo {
    let value: Value = serde_json::from_str(output).unwrap_or(Value::Null);
    let mut info = StreamInfo::default();
    for stream in value["streams"].as_array().into_iter().flatten() {
        let name = stream["codec_name"].as_str().map(str::to_owned);
        match stream["codec_type"].as_str() {
            Some("video") if info.video.is_none() => {
                info.video = name;
                info.pix_fmt = stream["pix_fmt"].as_str().map(str::to_owned);
            }
            Some("audio") if info.audio.is_none() => info.audio = name,
            _ => {}
        }
    }
    info
}

/// Encoder knobs for [`transcode_args`].
#[derive(Debug, Clone, Copy)]
pub struct Encode {
    pub codec: VideoCodec,
    /// `None` = codec default (x264: 18, SVT-AV1: 30).
    pub crf: Option<u32>,
    pub av1_preset: u8,
}

/// ffmpeg arguments that turn `input` into an inline-playable MP4. Streams that already match
/// are copied; everything gets `+faststart`. Returns `(args, video_is_reencoded)`.
pub fn transcode_args(
    info: &StreamInfo,
    encode: Encode,
    input: &Path,
    output: &Path,
) -> (Vec<String>, bool) {
    let codec = encode.codec;
    let eight_bit = matches!(info.pix_fmt.as_deref(), Some("yuv420p" | "yuvj420p"));
    let video_ok = match codec {
        VideoCodec::H264 => info.video.as_deref() == Some("h264") && eight_bit,
        VideoCodec::Av1 => info.video.as_deref() == Some("av1") && eight_bit,
        VideoCodec::Source => true,
    };
    let audio_ok = codec == VideoCodec::Source
        || info.audio.is_none()
        || matches!(info.audio.as_deref(), Some("aac" | "mp3"));

    let mut args: Vec<String> = Vec::with_capacity(24);
    let push = |args: &mut Vec<String>, items: &[&str]| {
        args.extend(items.iter().map(|s| (*s).to_owned()));
    };
    push(&mut args, &["-hide_banner", "-nostdin", "-y", "-i"]);
    args.push(input.to_string_lossy().into_owned());
    push(&mut args, &["-map", "0:v:0", "-map", "0:a:0?"]);
    if video_ok {
        push(&mut args, &["-c:v", "copy"]);
    } else if codec == VideoCodec::Av1 {
        let crf = encode.crf.unwrap_or(30).to_string();
        let preset = encode.av1_preset.to_string();
        push(
            &mut args,
            &["-c:v", "libsvtav1", "-preset", &preset, "-crf", &crf],
        );
        push(&mut args, &["-pix_fmt", "yuv420p"]);
    } else {
        let crf = encode.crf.unwrap_or(18).to_string();
        push(
            &mut args,
            &["-c:v", "libx264", "-preset", "slow", "-crf", &crf],
        );
        push(&mut args, &["-profile:v", "high", "-pix_fmt", "yuv420p"]);
    }
    if audio_ok {
        push(&mut args, &["-c:a", "copy"]);
    } else {
        push(&mut args, &["-c:a", "aac", "-b:a", "192k"]);
    }
    push(&mut args, &["-movflags", "+faststart"]);
    args.push(output.to_string_lossy().into_owned());
    (args, !video_ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEVC: &str = r#"{"streams":[{"codec_type":"video","codec_name":"hevc","pix_fmt":"yuv420p10le"},{"codec_type":"audio","codec_name":"opus"}]}"#;
    const H264: &str = r#"{"streams":[{"codec_type":"video","codec_name":"h264","pix_fmt":"yuv420p"},{"codec_type":"audio","codec_name":"aac"}]}"#;

    fn enc(codec: VideoCodec, crf: Option<u32>, av1_preset: u8) -> Encode {
        Encode {
            codec,
            crf,
            av1_preset,
        }
    }

    #[test]
    fn codec_parse_keeps_bot_default() {
        assert_eq!(VideoCodec::parse(None, VideoCodec::Av1), VideoCodec::Av1);
        assert_eq!(
            VideoCodec::parse(Some(" AVC "), VideoCodec::Av1),
            VideoCodec::H264
        );
        assert_eq!(
            VideoCodec::parse(Some("av01"), VideoCodec::H264),
            VideoCodec::Av1
        );
        assert_eq!(
            VideoCodec::parse(Some("copy"), VideoCodec::H264),
            VideoCodec::Source
        );
    }

    #[test]
    fn transcode_decisions() {
        let (p, o) = (Path::new("/t/video.mp4"), Path::new("/t/delivery.mp4"));
        let hevc = parse_probe(HEVC);
        assert_eq!(hevc.video.as_deref(), Some("hevc"));
        let (args, reencode) = transcode_args(&hevc, enc(VideoCodec::H264, None, 10), p, o);
        assert!(reencode);
        let joined = args.join(" ");
        assert!(
            joined.contains("-c:v libx264 -preset slow -crf 18 -profile:v high -pix_fmt yuv420p")
        );
        assert!(joined.contains("-c:a aac -b:a 192k"));
        assert!(joined.ends_with("-movflags +faststart /t/delivery.mp4"));

        let h264 = parse_probe(H264);
        let (args, reencode) = transcode_args(&h264, enc(VideoCodec::H264, None, 10), p, o);
        assert!(!reencode);
        assert!(args.join(" ").contains("-c:v copy -c:a copy"));

        let (args, _) = transcode_args(&h264, enc(VideoCodec::Av1, None, 10), p, o);
        assert!(args.join(" ").contains("-c:v libsvtav1 -preset 10 -crf 30"));
        let (args, _) = transcode_args(&hevc, enc(VideoCodec::Av1, Some(28), 6), p, o);
        assert!(args.join(" ").contains("-c:v libsvtav1 -preset 6 -crf 28"));
        assert!(!transcode_args(&hevc, enc(VideoCodec::Source, None, 6), p, o).1);
    }
}
