//! Secret-safe preparation values for a future FFmpeg supervisor.
//!
//! This module creates a short-lived ffconcat manifest and builds an argv
//! vector that opens it through the concat demuxer. It deliberately does not
//! spawn a process, open a network connection, or own a supervisor task.

use std::ffi::OsString;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use crate::config::ValidatedIoTimeout;
use crate::{
    FfmpegError, FfmpegFileOperation, RtspCredentials, RtspEndpoint, RtspError, RtspTransport,
};

const REDACTED: &str = "[REDACTED]";
const PROTOCOL_WHITELIST: &str = "file,rtsp,rtsps,tcp,udp,rtp,tls";

/// A short-lived ffconcat manifest containing one complete RTSP input URL.
///
/// The file is created with `create_new`, is removed by this guard on drop,
/// and is intentionally not cloneable. Callers must keep this value alive for
/// at least as long as the future FFmpeg process may read the file.
pub struct FfmpegInputFile {
    path: PathBuf,
    remove_on_drop: bool,
}

impl FfmpegInputFile {
    /// Creates the input file at exactly `path` without overwriting an
    /// existing file.
    pub fn create_new(
        path: impl Into<PathBuf>,
        endpoint: &RtspEndpoint,
        credentials: Option<&RtspCredentials>,
        transport: RtspTransport,
        io_timeout: Duration,
    ) -> Result<Self, RtspError> {
        let io_timeout = ValidatedIoTimeout::new(io_timeout)?;
        Self::create_new_validated(path, endpoint, credentials, transport, io_timeout)
    }

    pub(crate) fn create_new_validated(
        path: impl Into<PathBuf>,
        endpoint: &RtspEndpoint,
        credentials: Option<&RtspCredentials>,
        transport: RtspTransport,
        io_timeout: ValidatedIoTimeout,
    ) -> Result<Self, RtspError> {
        let path = path.into();
        if path.as_os_str().is_empty() {
            return Err(FfmpegError::InputFileIo {
                operation: FfmpegFileOperation::Create,
                kind: io::ErrorKind::InvalidInput,
            }
            .into());
        }

        let mut file = OpenOptions::new();
        file.write(true).create_new(true);
        #[cfg(unix)]
        file.mode(0o600);
        let mut file = file.open(&path).map_err(|error| FfmpegError::InputFileIo {
            operation: FfmpegFileOperation::Create,
            kind: error.kind(),
        })?;

        let guard = Self {
            path,
            remove_on_drop: true,
        };

        let manifest = ffconcat_manifest(endpoint, credentials, transport, io_timeout);
        if let Err(error) = file.write_all(manifest.as_bytes()) {
            return Err(FfmpegError::InputFileIo {
                operation: FfmpegFileOperation::Write,
                kind: error.kind(),
            }
            .into());
        }

        Ok(guard)
    }

    /// Returns the exact path supplied to [`Self::create_new`].
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Removes the exact file now, retaining the drop guard on failure.
    pub fn cleanup(mut self) -> Result<(), RtspError> {
        match fs::remove_file(&self.path) {
            Ok(()) => {
                self.remove_on_drop = false;
                Ok(())
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                self.remove_on_drop = false;
                Ok(())
            }
            Err(error) => Err(FfmpegError::InputFileIo {
                operation: FfmpegFileOperation::Remove,
                kind: error.kind(),
            }
            .into()),
        }
    }
}

impl fmt::Debug for FfmpegInputFile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FfmpegInputFile")
            .field("path_present", &true)
            .field("remove_on_drop", &self.remove_on_drop)
            .finish()
    }
}

impl Drop for FfmpegInputFile {
    fn drop(&mut self) {
        if self.remove_on_drop {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// An argv-only FFmpeg invocation description. It never spawns a process.
pub struct FfmpegCommandSpec {
    executable: PathBuf,
    argv: Vec<OsString>,
}

impl FfmpegCommandSpec {
    /// Builds the minimal FFmpeg argv needed for an MJPEG image pipe.
    pub fn new(
        executable: impl Into<PathBuf>,
        input_file: &FfmpegInputFile,
    ) -> Result<Self, RtspError> {
        let executable = executable.into();
        if executable.as_os_str().is_empty() {
            return Err(FfmpegError::EmptyExecutablePath.into());
        }

        let argv = vec![
            OsString::from("-hide_banner"),
            OsString::from("-loglevel"),
            OsString::from("warning"),
            OsString::from("-nostdin"),
            OsString::from("-f"),
            OsString::from("concat"),
            OsString::from("-safe"),
            OsString::from("0"),
            OsString::from("-protocol_whitelist"),
            OsString::from(PROTOCOL_WHITELIST),
            OsString::from("-i"),
            input_file.path().as_os_str().to_owned(),
            OsString::from("-an"),
            OsString::from("-f"),
            OsString::from("image2pipe"),
            OsString::from("-vcodec"),
            OsString::from("mjpeg"),
            OsString::from("pipe:1"),
        ];

        Ok(Self { executable, argv })
    }

    pub fn executable(&self) -> &Path {
        &self.executable
    }

    /// Returns argv as separate OS arguments. No shell command line is made.
    pub fn argv(&self) -> &[OsString] {
        &self.argv
    }
}

impl fmt::Debug for FfmpegCommandSpec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FfmpegCommandSpec")
            .field("executable_present", &true)
            .field("argument_count", &self.argv.len())
            .finish()
    }
}

impl fmt::Display for FfmpegCommandSpec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("FfmpegCommandSpec(<redacted argv>)")
    }
}

/// Sanitizes FFmpeg stderr before it reaches logs, UI, or public status.
pub struct FfmpegLogSanitizer {
    patterns: Vec<String>,
}

impl FfmpegLogSanitizer {
    /// Builds redaction patterns for an endpoint and optional credentials.
    /// Empty credentials are never added as patterns.
    pub fn new(endpoint: &RtspEndpoint, credentials: Option<&RtspCredentials>) -> Self {
        let mut patterns = Vec::new();
        push_pattern(&mut patterns, endpoint.as_str().to_owned());
        push_pattern(&mut patterns, quote_ffconcat_token(endpoint.as_str()));
        push_pattern(&mut patterns, endpoint.host().to_owned());
        if let Some(resource) = endpoint_resource(endpoint) {
            push_pattern(&mut patterns, resource.to_owned());
        }
        let Some(credentials) = credentials else {
            patterns.sort_by_key(|pattern| std::cmp::Reverse(pattern.len()));
            return Self { patterns };
        };
        if credentials.username().is_empty() && credentials.password().is_empty() {
            patterns.sort_by_key(|pattern| std::cmp::Reverse(pattern.len()));
            return Self { patterns };
        }

        let raw_url = authenticated_url_with(endpoint, credentials, false);
        let encoded_url = authenticated_url_with(endpoint, credentials, true);
        let lowercase_encoded_url = authenticated_url_with_case(endpoint, credentials, true, true);
        push_pattern(&mut patterns, quote_ffconcat_token(&raw_url));
        push_pattern(&mut patterns, quote_ffconcat_token(&encoded_url));
        push_pattern(&mut patterns, quote_ffconcat_token(&lowercase_encoded_url));
        push_pattern(&mut patterns, raw_url);
        push_pattern(&mut patterns, encoded_url);
        push_pattern(&mut patterns, lowercase_encoded_url);
        push_pattern(&mut patterns, credentials.username().to_owned());
        push_pattern(&mut patterns, credentials.password().to_owned());
        push_pattern(&mut patterns, percent_encode(credentials.username()));
        push_pattern(&mut patterns, percent_encode(credentials.password()));
        push_pattern(
            &mut patterns,
            percent_encode_case(credentials.username(), true),
        );
        push_pattern(
            &mut patterns,
            percent_encode_case(credentials.password(), true),
        );

        patterns.sort_by_key(|pattern| std::cmp::Reverse(pattern.len()));
        Self { patterns }
    }

    pub(crate) fn add_sensitive_pattern(&mut self, pattern: impl Into<String>) {
        push_pattern(&mut self.patterns, pattern.into());
        self.patterns
            .sort_by_key(|pattern| std::cmp::Reverse(pattern.len()));
    }

    pub fn sanitize(&self, stderr: &str) -> String {
        self.patterns
            .iter()
            .fold(stderr.to_owned(), |sanitized, pattern| {
                sanitized.replace(pattern, REDACTED)
            })
    }

    /// Creates a byte-oriented sanitizer for arbitrarily chunked stderr.
    pub fn stream(&self) -> FfmpegLogStreamSanitizer {
        FfmpegLogStreamSanitizer::new(
            self.patterns
                .iter()
                .map(|pattern| pattern.as_bytes().to_vec())
                .collect(),
        )
    }
}

impl fmt::Debug for FfmpegLogSanitizer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FfmpegLogSanitizer")
            .field("pattern_count", &self.patterns.len())
            .finish()
    }
}

/// Stateful byte-stream sanitizer for FFmpeg stderr.
///
/// It retains at most `max_pattern_len - 1` bytes between calls, which is the
/// minimum suffix needed to recognize a secret split across the next chunk.
/// Input is never decoded as UTF-8, so malformed FFmpeg output remains safe.
pub struct FfmpegLogStreamSanitizer {
    patterns: Vec<Vec<u8>>,
    max_pattern_len: usize,
    carry: Vec<u8>,
}

impl FfmpegLogStreamSanitizer {
    fn new(patterns: Vec<Vec<u8>>) -> Self {
        let max_pattern_len = patterns.iter().map(Vec::len).max().unwrap_or(0);
        Self {
            patterns,
            max_pattern_len,
            carry: Vec::new(),
        }
    }

    /// Sanitizes a byte chunk and returns only bytes that cannot participate
    /// in a secret pattern completed by a later chunk.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<u8> {
        self.carry.extend_from_slice(chunk);
        let retain = self.max_pattern_len.saturating_sub(1);
        let process_before = self.carry.len().saturating_sub(retain);
        let (sanitized, consumed) = sanitize_bytes(&self.carry, &self.patterns, process_before);
        self.carry.drain(..consumed);
        debug_assert!(self.carry.len() <= retain);
        sanitized
    }

    /// Sanitizes and returns the final retained suffix.
    pub fn finish(mut self) -> Vec<u8> {
        let process_before = self.carry.len();
        let (sanitized, consumed) = sanitize_bytes(&self.carry, &self.patterns, process_before);
        debug_assert_eq!(consumed, self.carry.len());
        self.carry.clear();
        sanitized
    }
}

impl fmt::Debug for FfmpegLogStreamSanitizer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FfmpegLogStreamSanitizer")
            .field("pattern_count", &self.patterns.len())
            .field("max_pattern_len", &self.max_pattern_len)
            .field("buffered_bytes", &self.carry.len())
            .finish()
    }
}

fn sanitize_bytes(input: &[u8], patterns: &[Vec<u8>], process_before: usize) -> (Vec<u8>, usize) {
    let mut sanitized = Vec::with_capacity(process_before);
    let mut cursor = 0;
    while cursor < process_before {
        if let Some(pattern) = patterns
            .iter()
            .find(|pattern| input[cursor..].starts_with(pattern))
        {
            sanitized.extend_from_slice(REDACTED.as_bytes());
            cursor += pattern.len();
        } else {
            sanitized.push(input[cursor]);
            cursor += 1;
        }
    }
    (sanitized, cursor)
}

fn push_pattern(patterns: &mut Vec<String>, pattern: String) {
    if !pattern.is_empty() && !patterns.iter().any(|existing| existing == &pattern) {
        patterns.push(pattern);
    }
}

fn endpoint_resource(endpoint: &RtspEndpoint) -> Option<&str> {
    let endpoint = endpoint.as_str();
    let authority_start = endpoint.find("://")? + 3;
    let resource_start = endpoint[authority_start..].find(['/', '?', '#'])? + authority_start;
    let resource = &endpoint[resource_start..];
    (resource.len() > 1).then_some(resource)
}

fn authenticated_url(endpoint: &RtspEndpoint, credentials: Option<&RtspCredentials>) -> String {
    match credentials {
        Some(credentials) => authenticated_url_with(endpoint, credentials, true),
        None => endpoint.as_str().to_owned(),
    }
}

fn ffconcat_manifest(
    endpoint: &RtspEndpoint,
    credentials: Option<&RtspCredentials>,
    transport: RtspTransport,
    io_timeout: ValidatedIoTimeout,
) -> String {
    let input_url = authenticated_url(endpoint, credentials);
    let transport = match transport {
        RtspTransport::Tcp => "tcp",
        RtspTransport::Udp => "udp",
    };
    format!(
        "ffconcat version 1.0\nfile {}\noption rtsp_transport {transport}\noption timeout {}\n",
        quote_ffconcat_token(&input_url),
        io_timeout.microseconds()
    )
}

/// Quotes one ffconcat token according to FFmpeg's shared quoting rules.
/// Backslashes remain literal inside quotes; an apostrophe closes the quoted
/// section, is backslash-escaped, and then reopens it.
fn quote_ffconcat_token(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('\'');
    for character in value.chars() {
        if character == '\'' {
            quoted.push_str("'\\''");
        } else {
            quoted.push(character);
        }
    }
    quoted.push('\'');
    quoted
}

fn authenticated_url_with(
    endpoint: &RtspEndpoint,
    credentials: &RtspCredentials,
    encode: bool,
) -> String {
    authenticated_url_with_case(endpoint, credentials, encode, false)
}

fn authenticated_url_with_case(
    endpoint: &RtspEndpoint,
    credentials: &RtspCredentials,
    encode: bool,
    lowercase_hex: bool,
) -> String {
    let endpoint_text = endpoint.as_str();
    let separator = endpoint.scheme_separator();
    let username = if encode {
        percent_encode_case(credentials.username(), lowercase_hex)
    } else {
        credentials.username().to_owned()
    };
    let password = if encode {
        percent_encode_case(credentials.password(), lowercase_hex)
    } else {
        credentials.password().to_owned()
    };
    format!(
        "{}://{}:{}@{}",
        &endpoint_text[..separator],
        username,
        password,
        &endpoint_text[separator + 3..]
    )
}

fn percent_encode(value: &str) -> String {
    percent_encode_case(value, false)
}

fn percent_encode_case(value: &str, lowercase_hex: bool) -> String {
    const UPPER_HEX: &[u8; 16] = b"0123456789ABCDEF";
    const LOWER_HEX: &[u8; 16] = b"0123456789abcdef";
    let hex = if lowercase_hex { LOWER_HEX } else { UPPER_HEX };
    let mut encoded = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(*byte as char);
        } else {
            encoded.push('%');
            encoded.push(hex[(byte >> 4) as usize] as char);
            encoded.push(hex[(byte & 0x0f) as usize] as char);
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::{RtspErrorCategory, RtspErrorCode};

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);
    const TEST_IO_TIMEOUT: Duration = Duration::from_secs(5);

    fn test_path(label: &str) -> PathBuf {
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("banto-rtsp-{label}-{}-{id}", std::process::id()))
    }

    fn endpoint() -> RtspEndpoint {
        RtspEndpoint::new("rtsp://camera.example/live").unwrap()
    }

    #[test]
    fn input_file_keeps_encoded_credentials_out_of_argv_and_debug() {
        let path = test_path("argv");
        let credentials = RtspCredentials::new("operator", "p@ss:word");
        let input = FfmpegInputFile::create_new(
            &path,
            &endpoint(),
            Some(&credentials),
            RtspTransport::Tcp,
            TEST_IO_TIMEOUT,
        )
        .unwrap();
        let spec = FfmpegCommandSpec::new("ffmpeg", &input).unwrap();
        let args = spec
            .argv()
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let argv_text = args.join(" ");
        let debug = format!("{spec:?} {spec}");

        let unsupported_input_arg = ["-/", "i"].concat();
        assert!(!args.contains(&unsupported_input_arg));
        assert!(args.contains(&"-i".to_owned()));
        assert!(args.contains(&"concat".to_owned()));
        assert!(args.contains(&PROTOCOL_WHITELIST.to_owned()));
        assert!(!argv_text.contains("camera.example"));
        assert!(!argv_text.contains("/live"));
        assert!(!argv_text.contains("rtsp://camera.example/live"));
        assert!(!argv_text.contains("rtsp://operator:p@ss:word@camera.example/live"));
        assert!(!argv_text.contains("operator"));
        assert!(!argv_text.contains("p@ss"));
        assert!(!argv_text.contains("%40"));
        assert!(!debug.contains("camera.example"));
        assert!(!debug.contains("/live"));
        assert!(!debug.contains("rtsp://camera.example/live"));
        assert!(!debug.contains("operator"));
        assert!(!debug.contains("p@ss"));
        assert!(!debug.contains("p%40ss"));
    }

    #[test]
    fn input_file_contains_exact_single_url_tcp_manifest() {
        let path = test_path("content");
        let credentials = RtspCredentials::new("operator", "p@ss:word");
        let input = FfmpegInputFile::create_new(
            &path,
            &endpoint(),
            Some(&credentials),
            RtspTransport::Tcp,
            TEST_IO_TIMEOUT,
        )
        .unwrap();

        assert_eq!(
            fs::read_to_string(input.path()).unwrap(),
            concat!(
                "ffconcat version 1.0\n",
                "file 'rtsp://operator:p%40ss%3Aword@camera.example/live'\n",
                "option rtsp_transport tcp\n",
                "option timeout 5000000\n"
            )
        );
    }

    #[test]
    fn reserved_and_unicode_credentials_are_percent_encoded_by_utf8_byte() {
        let path = test_path("encoding");
        let credentials = RtspCredentials::new("ユーザー:/@", "p a?%");
        let input = FfmpegInputFile::create_new(
            &path,
            &endpoint(),
            Some(&credentials),
            RtspTransport::Udp,
            TEST_IO_TIMEOUT,
        )
        .unwrap();

        assert_eq!(
            fs::read_to_string(input.path()).unwrap(),
            concat!(
                "ffconcat version 1.0\n",
                "file 'rtsp://%E3%83%A6%E3%83%BC%E3%82%B6%E3%83%BC%3A%2F%40:p%20a%3F%25@camera.example/live'\n",
                "option rtsp_transport udp\n",
                "option timeout 5000000\n"
            )
        );
    }

    #[test]
    fn manifest_quotes_apostrophe_and_preserves_backslash_spaces_and_query_delimiters() {
        let path = test_path("quoting");
        let endpoint = RtspEndpoint::new(
            "rtsps://camera.example/folder's\\stream name?mode=a&value=b=c#fragment",
        )
        .unwrap();
        let input = FfmpegInputFile::create_new(
            &path,
            &endpoint,
            None,
            RtspTransport::Tcp,
            TEST_IO_TIMEOUT,
        )
        .unwrap();

        assert_eq!(
            fs::read_to_string(input.path()).unwrap(),
            concat!(
                "ffconcat version 1.0\n",
                "file 'rtsps://camera.example/folder'\\''s\\stream name?mode=a&value=b=c#fragment'\n",
                "option rtsp_transport tcp\n",
                "option timeout 5000000\n"
            )
        );
    }

    #[test]
    fn existing_file_is_not_overwritten() {
        let path = test_path("existing");
        fs::write(&path, "sentinel").unwrap();

        let error = FfmpegInputFile::create_new(
            &path,
            &endpoint(),
            None,
            RtspTransport::Tcp,
            TEST_IO_TIMEOUT,
        )
        .unwrap_err();
        assert_eq!(
            error.public_info(),
            crate::RtspErrorInfo {
                category: RtspErrorCategory::Launch,
                code: RtspErrorCode::InputFileCreateFailed,
            }
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "sentinel");
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn drop_removes_only_the_created_file() {
        let path = test_path("drop");
        let sibling = path.with_extension("sibling");
        fs::write(&sibling, "keep").unwrap();
        {
            let _input = FfmpegInputFile::create_new(
                &path,
                &endpoint(),
                None,
                RtspTransport::Tcp,
                TEST_IO_TIMEOUT,
            )
            .unwrap();
            assert!(path.exists());
        }
        assert!(!path.exists());
        assert_eq!(fs::read_to_string(&sibling).unwrap(), "keep");
        fs::remove_file(sibling).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn input_file_is_created_with_owner_only_mode() {
        use std::os::unix::fs::PermissionsExt;

        let path = test_path("mode");
        let input = FfmpegInputFile::create_new(
            &path,
            &endpoint(),
            None,
            RtspTransport::Tcp,
            TEST_IO_TIMEOUT,
        )
        .unwrap();
        let mode = fs::metadata(input.path()).unwrap().permissions().mode() & 0o777;

        assert_eq!(mode, 0o600);
    }

    #[test]
    fn sanitizer_removes_raw_and_encoded_url_and_secret_values() {
        let credentials = RtspCredentials::new("alice@corp", "p/a:ss%");
        let endpoint = endpoint();
        let sanitizer = FfmpegLogSanitizer::new(&endpoint, Some(&credentials));
        let stderr = concat!(
            "raw rtsp://alice@corp:p/a:ss%@camera.example/live ",
            "encoded rtsp://alice%40corp:p%2Fa%3Ass%25@camera.example/live ",
            "lowercase rtsp://alice%40corp:p%2fa%3ass%25@camera.example/live ",
            "parts alice@corp p/a:ss% alice%40corp p%2Fa%3Ass%25 alice%40corp p%2fa%3ass%25"
        );
        let sanitized = sanitizer.sanitize(stderr);

        assert!(!sanitized.contains("alice@corp"));
        assert!(!sanitized.contains("p/a:ss%"));
        assert!(!sanitized.contains("alice%40corp"));
        assert!(!sanitized.contains("p%2Fa%3Ass%25"));
        assert!(!sanitized.contains("p%2fa%3ass%25"));
        assert!(sanitized.contains(REDACTED));
        let debug = format!("{sanitizer:?}");
        assert!(!debug.contains("alice"));
        assert!(!debug.contains("p/a"));
    }

    #[test]
    fn empty_credentials_do_not_create_a_dangerous_empty_pattern() {
        let endpoint = endpoint();
        let sanitizer = FfmpegLogSanitizer::new(&endpoint, Some(&RtspCredentials::new("", "")));
        assert_eq!(sanitizer.sanitize("camera stderr"), "camera stderr");
    }

    #[test]
    fn credentials_none_keeps_endpoint_without_authentication() {
        let path = test_path("none");
        let input = FfmpegInputFile::create_new(
            &path,
            &endpoint(),
            None,
            RtspTransport::Tcp,
            TEST_IO_TIMEOUT,
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(input.path()).unwrap(),
            concat!(
                "ffconcat version 1.0\n",
                "file 'rtsp://camera.example/live'\n",
                "option rtsp_transport tcp\n",
                "option timeout 5000000\n"
            )
        );
        let sanitizer = FfmpegLogSanitizer::new(&endpoint(), None);
        assert_eq!(sanitizer.sanitize("rtsp://camera.example/live"), REDACTED);
    }

    #[test]
    fn streaming_sanitizer_redacts_all_forms_from_one_byte_chunks() {
        let credentials = RtspCredentials::new("alice@corp", "p/a:ss%");
        let endpoint = endpoint();
        let sanitizer = FfmpegLogSanitizer::new(&endpoint, Some(&credentials));
        let input = concat!(
            "endpoint rtsp://camera.example/live ",
            "raw rtsp://alice@corp:p/a:ss%@camera.example/live ",
            "upper rtsp://alice%40corp:p%2Fa%3Ass%25@camera.example/live ",
            "lower rtsp://alice%40corp:p%2fa%3ass%25@camera.example/live ",
            "parts alice@corp p/a:ss% alice%40corp p%2Fa%3Ass%25 ",
            "alice%40corp p%2fa%3ass%25"
        );
        let mut stream = sanitizer.stream();
        let mut output = Vec::new();
        for byte in input.as_bytes() {
            output.extend(stream.push(std::slice::from_ref(byte)));
        }
        output.extend(stream.finish());

        let output = String::from_utf8(output).unwrap();
        for secret in [
            "rtsp://camera.example/live",
            "rtsp://alice@corp:p/a:ss%@camera.example/live",
            "rtsp://alice%40corp:p%2Fa%3Ass%25@camera.example/live",
            "rtsp://alice%40corp:p%2fa%3ass%25@camera.example/live",
            "alice@corp",
            "p/a:ss%",
            "alice%40corp",
            "p%2Fa%3Ass%25",
            "p%2fa%3ass%25",
        ] {
            assert!(!output.contains(secret));
        }
        assert!(output.contains(REDACTED));
    }

    #[test]
    fn streaming_sanitizer_redacts_endpoint_without_credentials() {
        let sanitizer = FfmpegLogSanitizer::new(&endpoint(), None);
        let mut stream = sanitizer.stream();
        let mut output = stream.push(b"open rtsp://camera.");
        output.extend(stream.push(b"example/live failed"));
        output.extend(stream.finish());

        assert_eq!(String::from_utf8(output).unwrap(), "open [REDACTED] failed");
    }

    #[test]
    fn streaming_sanitizer_preserves_invalid_utf8_without_exposing_secret() {
        let sanitizer = FfmpegLogSanitizer::new(&endpoint(), None);
        let mut stream = sanitizer.stream();
        let mut output = stream.push(b"\xffrtsp://camera.example/live\xfe");
        output.extend(stream.finish());

        assert_eq!(output, b"\xff[REDACTED]\xfe");
    }

    #[test]
    fn streaming_sanitizer_bounds_carry_and_hides_debug_values() {
        let credentials = RtspCredentials::new("alice@corp", "p/a:ss%");
        let sanitizer = FfmpegLogSanitizer::new(&endpoint(), Some(&credentials));
        let mut stream = sanitizer.stream();

        for byte in b"ordinary ffmpeg output without a complete secret" {
            let _ = stream.push(std::slice::from_ref(byte));
            assert!(stream.carry.len() < stream.max_pattern_len);
        }

        let debug = format!("{stream:?}");
        assert!(!debug.contains("camera.example"));
        assert!(!debug.contains("alice"));
        assert!(!debug.contains("p/a"));
    }

    #[test]
    fn command_spec_uses_concat_manifest_and_never_uses_slash_i() {
        let path = test_path("concat-argv").with_extension("ffconcat");
        let credentials = RtspCredentials::new("operator", "private-pass");
        let input = FfmpegInputFile::create_new(
            &path,
            &endpoint(),
            Some(&credentials),
            RtspTransport::Udp,
            TEST_IO_TIMEOUT,
        )
        .unwrap();
        let spec = FfmpegCommandSpec::new(OsString::from("ffmpeg"), &input).unwrap();
        let args = spec
            .argv()
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            args,
            [
                "-hide_banner",
                "-loglevel",
                "warning",
                "-nostdin",
                "-f",
                "concat",
                "-safe",
                "0",
                "-protocol_whitelist",
                PROTOCOL_WHITELIST,
                "-i",
                path.to_string_lossy().as_ref(),
                "-an",
                "-f",
                "image2pipe",
                "-vcodec",
                "mjpeg",
                "pipe:1",
            ]
        );
        let unsupported_input_arg = ["-/", "i"].concat();
        assert!(!args.iter().any(|arg| arg == &unsupported_input_arg));
        let argv = args.join(" ");
        assert!(!argv.contains("camera.example"));
        assert!(!argv.contains("operator"));
        assert!(!argv.contains("private-pass"));
    }

    #[test]
    fn empty_executable_path_is_rejected_structurally() {
        let path = test_path("empty-executable");
        let input = FfmpegInputFile::create_new(
            &path,
            &endpoint(),
            None,
            RtspTransport::Tcp,
            TEST_IO_TIMEOUT,
        )
        .unwrap();
        let error = FfmpegCommandSpec::new(PathBuf::new(), &input).unwrap_err();
        assert_eq!(error.public_info().code, RtspErrorCode::EmptyExecutablePath);
    }

    #[test]
    fn input_file_rejects_invalid_timeout_before_creating_a_file() {
        for (label, timeout) in [
            ("zero-timeout", Duration::ZERO),
            ("sub-microsecond-timeout", Duration::from_nanos(999)),
        ] {
            let path = test_path(label);
            let error =
                FfmpegInputFile::create_new(&path, &endpoint(), None, RtspTransport::Tcp, timeout)
                    .unwrap_err();

            assert_eq!(error.public_info().code, RtspErrorCode::InvalidIoTimeout);
            assert!(!path.exists());
        }
    }

    #[test]
    #[ignore = "requires BANTO_RTSP_TEST_FFMPEG pointing to a local FFmpeg binary"]
    fn env_ffmpeg_accepts_one_file_concat_manifest_offline() {
        let executable = std::env::var_os("BANTO_RTSP_TEST_FFMPEG")
            .expect("set BANTO_RTSP_TEST_FFMPEG to opt in");
        let directory = test_path("concat-capability");
        fs::create_dir(&directory).unwrap();
        let media = directory.join("one frame's source.ppm");
        let manifest = directory.join("one frame.ffconcat");
        let mut ppm = b"P6\n1 1\n255\n".to_vec();
        ppm.extend_from_slice(&[0xff, 0x00, 0x00]);
        fs::write(&media, ppm).unwrap();
        fs::write(
            &manifest,
            format!(
                "ffconcat version 1.0\nfile {}\n",
                quote_ffconcat_token(&media.to_string_lossy())
            ),
        )
        .unwrap();

        let output = std::process::Command::new(executable)
            .args([
                OsString::from("-hide_banner"),
                OsString::from("-loglevel"),
                OsString::from("error"),
                OsString::from("-f"),
                OsString::from("concat"),
                OsString::from("-safe"),
                OsString::from("0"),
                OsString::from("-protocol_whitelist"),
                OsString::from("file"),
                OsString::from("-i"),
                manifest.as_os_str().to_owned(),
                OsString::from("-frames:v"),
                OsString::from("1"),
                OsString::from("-f"),
                OsString::from("image2pipe"),
                OsString::from("-vcodec"),
                OsString::from("mjpeg"),
                OsString::from("pipe:1"),
            ])
            .output()
            .unwrap();

        let _ = fs::remove_dir_all(&directory);
        assert!(output.status.success());
        assert!(output.stdout.starts_with(&[0xff, 0xd8]));
        assert!(output.stdout.ends_with(&[0xff, 0xd9]));
    }

    #[test]
    #[ignore = "requires BANTO_RTSP_TEST_FFMPEG pointing to a local FFmpeg binary"]
    fn env_ffmpeg_rtsp_timeout_exits_before_outer_deadline() {
        use std::io::Read;
        use std::net::TcpListener;
        use std::process::Stdio;
        use std::sync::mpsc;
        use std::thread;
        use std::time::Instant;

        let executable = std::env::var_os("BANTO_RTSP_TEST_FFMPEG")
            .expect("set BANTO_RTSP_TEST_FFMPEG to opt in");
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let (accepted_tx, accepted_rx) = mpsc::channel();
        let (stop_tx, stop_rx) = mpsc::channel();
        let server = thread::spawn(move || loop {
            if stop_rx.try_recv().is_ok() {
                return;
            }
            match listener.accept() {
                Ok((_stream, _)) => {
                    accepted_tx.send(()).unwrap();
                    let _ = stop_rx.recv_timeout(Duration::from_secs(5));
                    return;
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("local listener failed: {error:?}"),
            }
        });

        let endpoint = RtspEndpoint::new(format!("rtsp://{address}/silent")).unwrap();
        let credentials = RtspCredentials::new("timeout-user", "timeout-password");
        let path = test_path("rtsp-timeout").with_extension("ffconcat");
        let input = FfmpegInputFile::create_new(
            &path,
            &endpoint,
            Some(&credentials),
            RtspTransport::Tcp,
            Duration::from_millis(200),
        )
        .unwrap();
        let spec = FfmpegCommandSpec::new(executable, &input).unwrap();
        let mut child = std::process::Command::new(spec.executable())
            .args(spec.argv())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        let status = loop {
            match child.try_wait().unwrap() {
                Some(status) => break Some(status),
                None if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
                None => break None,
            }
        };
        if status.is_none() {
            child.kill().unwrap();
            let _ = child.wait();
        }
        let mut stderr = Vec::new();
        child
            .stderr
            .take()
            .unwrap()
            .read_to_end(&mut stderr)
            .unwrap();
        let _ = stop_tx.send(());
        server.join().unwrap();

        assert!(accepted_rx.try_recv().is_ok());
        let status = status.expect("FFmpeg ignored the configured RTSP timeout");
        assert!(!status.success());
        let sanitizer = FfmpegLogSanitizer::new(&endpoint, Some(&credentials));
        let sanitized_stderr = sanitizer.sanitize(&String::from_utf8_lossy(&stderr));
        assert!(!sanitized_stderr.contains("timeout-user"));
        assert!(!sanitized_stderr.contains("timeout-password"));
    }
}
