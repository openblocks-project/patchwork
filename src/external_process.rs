//! P4.3.2B: argument hygiene for external programs started by nodes.
//!
//! Nodes start ffmpeg, yt-dlp, espeak-ng, lp/print and the OS "open"
//! handler with values that can come from a graph (paths, URLs, text). The
//! rules here keep such values from becoming options or shell syntax:
//!
//! * no shell: programs are started directly (`Command`), and opening a
//!   file or URL uses the OS handler (`open::that_detached`), never
//!   `cmd /C start`;
//! * a value that could be read as an option is refused, or placed after
//!   `--` where the program supports it;
//! * ffmpeg inputs carry an explicit `-protocol_whitelist`, so a "network"
//!   URL cannot read local files (`file:`, `concat:`, `subfile:`, HLS
//!   playlists pointing at files, …) and a local path cannot reach the
//!   network.

/// Protocols ffmpeg may use for a network source (no `file`).
pub const FFMPEG_NETWORK_PROTOCOLS: &str = "http,https,tls,tcp,udp,crypto,rtmp,rtmps,rtsp,rtp,srt";
/// Protocols ffmpeg may use for a local file.
pub const FFMPEG_FILE_PROTOCOLS: &str = "file";

/// URL schemes accepted as network sources.
const NETWORK_SCHEMES: &[&str] = &["http", "https", "rtmp", "rtmps", "rtsp", "srt", "udp"];

/// Plain text without control characters (newlines, NUL, …).
fn has_control(value: &str) -> bool {
    value.chars().any(char::is_control)
}

/// `scheme` of `scheme://…` or `scheme:…` (a single letter is a Windows
/// drive, not a scheme).
fn scheme_of(value: &str) -> Option<&str> {
    // Anything before the first ':' that is not a path (no separator) is
    // treated as a protocol, including ffmpeg's `subfile,,…:` form.
    let (scheme, _) = value.split_once(':')?;
    let looks_like_scheme = scheme.len() > 1 && !scheme.contains(['/', '\\']);
    looks_like_scheme.then_some(scheme)
}

/// A value passed as a positional argument: non-empty, no control
/// characters, and not readable as an option.
pub fn check_positional(value: &str, what: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("The {what} is empty."));
    }
    if has_control(value) {
        return Err(format!("The {what} contains control characters."));
    }
    if value.starts_with('-') {
        return Err(format!("The {what} must not start with '-'."));
    }
    Ok(())
}

/// An `http://` or `https://` URL for a web download.
pub fn check_web_url(url: &str) -> Result<(), String> {
    check_positional(url, "URL")?;
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return Err(format!("'{url}' is not an http(s) URL."));
    }
    if url.chars().any(char::is_whitespace) {
        return Err("The URL contains spaces.".to_string());
    }
    Ok(())
}

/// `-protocol_whitelist <list> -i <source>` for one ffmpeg input. Network
/// URLs may only use network protocols; anything else is a local file and
/// may only be read as a file. Other schemes (`file:`, `concat:`,
/// `subfile:`, `data:`, `pipe:`, …) are refused.
pub fn ffmpeg_input_args(source: &str) -> Result<Vec<String>, String> {
    check_positional(source, "media source")?;
    let whitelist = match scheme_of(source) {
        Some(scheme) => {
            let scheme = scheme.to_ascii_lowercase();
            if !NETWORK_SCHEMES.contains(&scheme.as_str()) {
                return Err(format!(
                    "Media source scheme '{scheme}:' is not supported (use a file path or an \
                     http(s), rtmp, rtsp, srt or udp URL)."
                ));
            }
            FFMPEG_NETWORK_PROTOCOLS
        }
        None => FFMPEG_FILE_PROTOCOLS,
    };
    Ok(vec![
        "-protocol_whitelist".to_string(),
        whitelist.to_string(),
        "-i".to_string(),
        source.to_string(),
    ])
}

/// Open a file or URL with the operating system's handler, without a
/// shell.
pub fn open_externally(target: impl AsRef<std::ffi::OsStr>) {
    if let Err(error) = open::that_detached(target) {
        crate::system_log::warn(format!("Could not open: {error}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ffmpeg_inputs_get_a_protocol_whitelist() {
        assert_eq!(
            ffmpeg_input_args("https://example.com/v.m3u8").unwrap(),
            vec![
                "-protocol_whitelist",
                FFMPEG_NETWORK_PROTOCOLS,
                "-i",
                "https://example.com/v.m3u8"
            ]
        );
        assert_eq!(
            ffmpeg_input_args("C:\\Videos\\clip.mp4").unwrap()[1],
            FFMPEG_FILE_PROTOCOLS,
            "a drive letter is a local path"
        );
        assert_eq!(ffmpeg_input_args("/home/a/clip.mp4").unwrap()[1], "file");
        assert_eq!(
            ffmpeg_input_args("rtsp://cam.local/live").unwrap()[1],
            FFMPEG_NETWORK_PROTOCOLS
        );
        assert!(!FFMPEG_NETWORK_PROTOCOLS.split(',').any(|p| p == "file"));
    }

    #[test]
    fn unsafe_ffmpeg_inputs_are_refused() {
        for bad in [
            "file:///etc/passwd",
            "concat:/etc/passwd|/etc/hosts",
            "subfile,,start,0,end,0,,:/etc/passwd",
            "data:text/plain,hello",
            "pipe:0",
            "-",
            "-f lavfi",
            "-i",
            "",
            "clip.mp4\n-y",
            "gopher://x/1",
        ] {
            assert!(ffmpeg_input_args(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn web_urls_and_positional_values() {
        assert!(check_web_url("https://youtu.be/abc").is_ok());
        for bad in [
            "--exec=calc youtube.com",
            "-o /tmp/x https://youtube.com",
            "file:///etc/passwd",
            "ftp://x/y",
            "https://youtube.com/watch?v=1 --exec calc",
            "https://x\n--exec",
        ] {
            assert!(check_web_url(bad).is_err(), "{bad:?}");
        }
        assert!(check_positional("en-us", "voice").is_ok());
        assert!(check_positional("-w", "voice").is_err());
        assert!(check_positional("a\0b", "voice").is_err());
    }
}
