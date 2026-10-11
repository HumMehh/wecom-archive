//! Optional external summarization for communication reports.
//!
//! This is the only place in the product that can send archived message text to
//! a third party, so it is off unless an operator explicitly turns it on and
//! supplies their own endpoint. The API key is stored DPAPI-protected and never
//! leaves this module in plaintext.

use std::path::Path;

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

/// Characters allowed in one request. Chinese runs roughly one token per
/// character, so this keeps a single call well inside a 32k-token context
/// while leaving room for the prompt and the answer.
pub const DEFAULT_MAX_INPUT_CHARS: usize = 12_000;

fn default_max_input_chars() -> usize {
    DEFAULT_MAX_INPUT_CHARS
}

/// Attach pictures (and model-supported audio) to the request. Default on: the
/// operator asked for media to be fed to the model, but it is a separate switch
/// so it can be turned off without disabling summaries altogether.
fn default_attach_media() -> bool {
    true
}

/// Stored form. The key is a hex dump of DPAPI-protected bytes, so the file is
/// useless on another machine or another account.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct StoredSummarySettings {
    pub enabled: bool,
    pub endpoint: String,
    pub model: String,
    /// The operator's summary instructions, sent as the request's system
    /// message ahead of one customer's conversation. Required: there is no
    /// built-in prompt. Stored in plain text — it is a template, not a secret.
    pub prompt: String,
    /// Per-request character budget; longer transcripts are summarized in
    /// chunks and folded together.
    #[serde(default = "default_max_input_chars")]
    pub max_input_chars: usize,
    /// Send image bytes along with the transcript. Missing from files written
    /// by older versions, so it must default to on rather than to false.
    #[serde(default = "default_attach_media")]
    pub attach_media: bool,
    pub protected_api_key: Option<String>,
}

impl Default for StoredSummarySettings {
    fn default() -> Self {
        Self {
            enabled: false,
            endpoint: String::new(),
            model: String::new(),
            // No built-in prompt: an install with nothing written here is not
            // usable, which is the point.
            prompt: String::new(),
            max_input_chars: DEFAULT_MAX_INPUT_CHARS,
            // A fresh install, and a settings file written before the switch
            // existed, both mean "media goes out": that is what was asked for.
            attach_media: true,
            protected_api_key: None,
        }
    }
}

/// What the UI is allowed to see: everything except the key itself.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicSummarySettings {
    pub enabled: bool,
    pub endpoint: String,
    pub model: String,
    /// The operator's instructions, echoed back so the form reopens with the
    /// wording that was actually saved. It is a template, never a secret — and
    /// without it the field loads blank, which the next save then persists as
    /// blank, discarding what was written last time.
    pub prompt: String,
    /// Whether pictures are attached; mirrored back so the form reopens with
    /// the switch in the state that was actually saved.
    pub attach_media: bool,
    pub has_api_key: bool,
}

impl PublicSummarySettings {
    pub fn from_stored(stored: &StoredSummarySettings) -> Self {
        Self {
            enabled: stored.enabled,
            endpoint: stored.endpoint.clone(),
            model: stored.model.clone(),
            prompt: stored.prompt.clone(),
            attach_media: stored.attach_media,
            has_api_key: stored
                .protected_api_key
                .as_deref()
                .is_some_and(|value| !value.is_empty()),
        }
    }
}

/// Runtime settings with the key decrypted. Never serialized.
#[derive(Clone)]
pub struct ActiveSummarySettings {
    pub endpoint: String,
    pub model: String,
    /// Empty when the built-in prompt should be used.
    pub prompt: String,
    pub max_input_chars: usize,
    pub attach_media: bool,
    pub api_key: Zeroizing<String>,
}

impl ActiveSummarySettings {
    /// The operator's instructions, which are the whole system message: the
    /// request carries them and then the conversation of a single customer.
    ///
    /// There is no built-in fallback. A blank prompt would leave the model
    /// without instructions, so empty settings are simply not usable
    /// (`is_configured`) rather than silently answered with a template nobody
    /// wrote.
    fn system_prompt(&self) -> &str {
        self.prompt.trim()
    }
}

impl StoredSummarySettings {
    pub fn to_active(&self) -> Result<ActiveSummarySettings, String> {
        let protected = self
            .protected_api_key
            .as_deref()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "尚未配置大模型密钥。".to_owned())?;
        let bytes = hex_decode(protected).ok_or_else(|| "密钥数据已损坏。".to_owned())?;
        let plaintext = crate::dpapi_protect::unprotect(&bytes)?;
        let api_key = String::from_utf8(plaintext)
            .map_err(|_| "密钥数据已损坏。".to_owned())
            // Also clean on read, so a key stored before this was introduced
            // gets repaired instead of continuing to fail.
            .map(|key| sanitize_key(&key))?;
        Ok(ActiveSummarySettings {
            endpoint: resolve_endpoint(&self.endpoint),
            model: self.model.trim().to_owned(),
            prompt: self.prompt.trim().to_owned(),
            max_input_chars: if self.max_input_chars == 0 {
                DEFAULT_MAX_INPUT_CHARS
            } else {
                self.max_input_chars.clamp(1_000, 200_000)
            },
            attach_media: self.attach_media,
            api_key: Zeroizing::new(api_key),
        })
    }

    pub fn public(&self) -> PublicSummarySettings {
        PublicSummarySettings::from_stored(self)
    }

    /// Everything a real request needs apart from the on/off switch.
    ///
    /// The switch is a product decision about sending archived text out; a
    /// probe is something the operator asks for directly and carries no
    /// archived content. Testing a configured-but-disabled endpoint must work,
    /// otherwise the first「测试连接」before「保存并开启」can only ever answer
    /// "尚未开启" however complete the form is.
    ///
    /// The instructions count as required: they are the system message, and
    /// there is no built-in one to fall back to.
    pub fn is_configured(&self) -> bool {
        !self.endpoint.trim().is_empty()
            && !self.model.trim().is_empty()
            && !self.prompt.trim().is_empty()
            && self
                .protected_api_key
                .as_deref()
                .is_some_and(|value| !value.is_empty())
    }

    /// True only when everything needed for a real request is present.
    pub fn is_usable(&self) -> bool {
        self.enabled && self.is_configured()
    }
}

pub fn settings_path(data_root: &Path) -> std::path::PathBuf {
    data_root.join("summary-settings.json")
}

pub fn load(path: &Path) -> StoredSummarySettings {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Write through a `.partial` rename so a crash cannot leave half a config.
pub fn save(path: &Path, settings: &StoredSummarySettings) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(settings).map_err(|_| "配置序列化失败".to_owned())?;
    let mut partial = path.as_os_str().to_os_string();
    partial.push(".partial");
    std::fs::write(&partial, bytes).map_err(|_| "配置写入失败".to_owned())?;
    std::fs::rename(&partial, path).map_err(|_| "配置写入失败".to_owned())
}

/// Encrypt a plaintext key into the stored hex form.
pub fn protect_key(plaintext: &str) -> Result<String, String> {
    let cleaned = sanitize_key(plaintext);
    let protected = crate::dpapi_protect::protect(cleaned.as_bytes())?;
    Ok(hex_encode(&protected))
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn hex_decode(value: &str) -> Option<Vec<u8>> {
    if !value.len().is_multiple_of(2) {
        return None;
    }
    (0..value.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&value[index..index + 2], 16).ok())
        .collect()
}

/// Ask the configured endpoint to summarize one transcript.
///
/// What actually came back. `Ok` only means curl ran; the HTTP status may still
/// be an error, and curl's own diagnostics are preserved so a failure can be
/// explained instead of guessed at.
#[derive(Debug, Default)]
pub struct EndpointOutcome {
    pub http_status: Option<i32>,
    pub body: String,
    pub curl_error: String,
    /// The request headers curl actually put on the wire, credential redacted.
    /// Empty unless a trace was captured.
    pub sent_headers: Vec<String>,
}

impl EndpointOutcome {
    pub fn is_success(&self) -> bool {
        self.http_status.is_some_and(|status| (200..300).contains(&status))
    }

    /// A short human-readable explanation, safe to show in the UI.
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        parts.push(match self.http_status {
            Some(status) => format!("HTTP {status}"),
            None => "未取得 HTTP 状态码".to_owned(),
        });
        if !self.body.is_empty() {
            parts.push(format!("响应：{}", truncate(&self.body, 500)));
        }
        if let Some(hint) = hint_for(self.http_status, &self.body) {
            parts.push(hint.to_owned());
        }
        if !self.curl_error.is_empty() {
            parts.push(format!("curl：{}", truncate(&self.curl_error, 300)));
        }
        if !self.sent_headers.is_empty() {
            parts.push(format!("实际发出的请求头：\n{}", self.sent_headers.join("\n")));
        }
        parts.join("\n")
    }
}

/// Show that an Authorization header really went out without ever printing the
/// secret: the scheme and length stay visible, the token does not.
fn redact_header(line: &str) -> Option<String> {
    let (name, value) = line.split_once(':')?;
    if !name.trim().eq_ignore_ascii_case("authorization") {
        return Some(line.to_owned());
    }
    let value = value.trim();
    let token_len = value
        .split_once(' ')
        .map(|(_, token)| token.trim().chars().count())
        .unwrap_or(value.chars().count());
    let scheme = value
        .split_once(' ')
        .map(|(scheme, _)| format!("{} ", scheme.trim()))
        .unwrap_or_default();
    Some(format!("{}: {}[已打码，{token_len} 字符]", name.trim(), scheme))
}

/// Turn the service's own wording into a concrete next step. A bare 404 from a
/// chat-completions gateway is almost always a truncated address, and without
/// this the operator has to already know the right URL shape.
fn hint_for(status: Option<i32>, body: &str) -> Option<&'static str> {
    let lowered = body.to_ascii_lowercase();
    if lowered.contains("invalid url")
        || lowered.contains("unknown path")
        || lowered.contains("unsupported url")
        || lowered.contains("not_found_error")
    {
        return Some(
            "建议：接口地址要填完整的 chat completions 路径，例如 https://<域名>/v1/chat/completions",
        );
    }
    if lowered.contains("model")
        && (status == Some(404) || lowered.contains("does not exist"))
    {
        return Some("建议：模型名称不被该接口识别，请核对，通常需要带厂商前缀。");
    }
    if status == Some(401)
        || lowered.contains("unauthorized")
        || lowered.contains("incorrect api key")
        || lowered.contains("invalid api key")
        || lowered.contains("authentication")
    {
        return Some("建议：密钥无效或已过期，请重新填写 API 密钥。");
    }
    if status == Some(403) || lowered.contains("forbidden") {
        return Some("建议：密钥有效但没有该模型或该接口的权限。");
    }
    if status == Some(429) {
        return Some("建议：请求过于频繁或额度已用尽。");
    }
    None
}

/// Marks a value that was cut short for display.
pub const ELLIPSIS: &str = "…";

fn truncate(value: &str, limit: usize) -> String {
    let trimmed = value.trim();
    if trimmed.chars().count() <= limit {
        return trimmed.to_owned();
    }
    format!("{}{ELLIPSIS}", trimmed.chars().take(limit).collect::<String>())
}

/// Complete the chat-completions path from a base URL, the way config switchers
/// do. A truncated address is the single most common setup mistake and the
/// service only answers `Invalid URL (POST /v1)` with no hint, so fill it in here
/// instead of pushing the detail onto the operator.
pub fn resolve_endpoint(raw: &str) -> String {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return String::new();
    }
    let lowered = trimmed.to_ascii_lowercase();
    if lowered.contains("/chat/completions")
        || lowered.ends_with("/completions")
        || lowered.ends_with("/messages")
    {
        return trimmed.to_owned();
    }
    // A base that already ends in a version segment names the revision itself
    // (Zhipu serves chat completions under .../api/paas/v4), so only the path
    // is missing. Supplying /v1 there would build .../v4/v1/chat/completions
    // and the gateway would answer 404 for a perfectly good key.
    if path_last_segment(&lowered).is_some_and(is_version_segment)
        || lowered.ends_with("/openai")
    {
        return format!("{trimmed}/chat/completions");
    }
    format!("{trimmed}/v1/chat/completions")
}

/// The final segment of the URL path, or None when the URL is only an origin.
/// Looking at the path rather than the whole string keeps a host that merely
/// starts with a v (say https://v1.example.com) from being mistaken for a
/// version and left without its /v1.
fn path_last_segment(url: &str) -> Option<&str> {
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    let path = after_scheme.split_once('/')?.1;
    let segment = path.rsplit('/').next().unwrap_or(path);
    (!segment.is_empty()).then_some(segment)
}

/// v1, v4, v10, v1beta, v1.0 — a token that starts with v, then a digit, and
/// nothing that would make it read as a word.
fn is_version_segment(segment: &str) -> bool {
    let Some(rest) = segment.strip_prefix('v') else {
        return false;
    };
    rest.starts_with(|character: char| character.is_ascii_digit())
        && rest.chars().all(|character| {
            character.is_ascii_digit() || character.is_ascii_alphabetic() || character == '.'
        })
}

/// A pasted API key often carries a line break or a stray space from wherever
/// it was copied. Those never belong in a credential and a newline would break
/// the request header line, so everything whitespace is dropped on the way in.
pub fn sanitize_key(raw: &str) -> String {
    raw.chars().filter(|character| !character.is_whitespace()).collect()
}

pub fn summarize_key_shape(key: &str) -> String {
    if key.is_empty() {
        return "空".to_owned();
    }
    let has_non_ascii = !key.is_ascii();
    let mut kinds = Vec::new();
    if key.chars().any(|character| character.is_ascii_digit()) {
        kinds.push("数字");
    }
    if key.chars().any(|character| character.is_ascii_alphabetic()) {
        kinds.push("字母");
    }
    if key
        .chars()
        .any(|character| !character.is_ascii_alphanumeric())
    {
        kinds.push("符号");
    }
    if has_non_ascii {
        kinds.push("非 ASCII");
    }
    format!("{} 字符（{}）", key.chars().count(), kinds.join("+"))
}

/// The credential file curl reads. The whole `Name: value` line matters:
/// curl keeps only header lines that contain a colon, so a line holding just
/// `Bearer …` is dropped and the request would go out with no Authorization
/// header at all — which gateways report as "未提供令牌" instead of a bad key.
fn curl_config_contents(authorization: &str) -> String {
    format!("header = \"Authorization: {authorization}\"\n")
}

/// The command line for one call, minus the payload (that goes through stdin).
///
/// `--config` takes the very next token as its file name, so the path has to
/// sit directly behind the flag. When another option followed it instead, curl
/// tried to open `--verbose` as a file, failed with "cannot read config from
/// '--verbose'" and never sent the request — the call could not succeed no
/// matter what was configured. Kept as a function so a test can pin the order.
fn curl_arguments(config_path: &Path, endpoint: &str) -> Vec<std::ffi::OsString> {
    let mut arguments: Vec<std::ffi::OsString> = [
        // -q must stay first: it stops curl from reading a user .curlrc, so a
        // stray file can never alter what this product sends.
        "-q",
        // -s hides the progress meter and --show-error keeps failures visible;
        // --verbose restores the request headers on stderr, which is how the
        // outgoing headers are reported back to the operator.
        "--silent",
        "--show-error",
        "--connect-timeout",
        "15",
        "--max-time",
        "180",
        "-X",
        "POST",
    ]
    .iter()
    .copied()
    .map(std::ffi::OsString::from)
    .collect();
    arguments.push(std::ffi::OsString::from("--config"));
    arguments.push(config_path.as_os_str().to_os_string());
    arguments.extend(
        [
            "--verbose",
            "-H",
            "Content-Type: application/json",
            "--data-binary",
            "@-",
            // curl appends the status after the body, so an HTTP error still
            // shows exactly what the service replied.
            "--write-out",
            "\n%{http_code}",
        ]
        .iter()
        .copied()
        .map(std::ffi::OsString::from),
    );
    arguments.push(std::ffi::OsString::from(endpoint));
    arguments
}

fn call_endpoint(
    settings: &ActiveSummarySettings,
    payload: &[u8],
) -> Result<EndpointOutcome, String> {
    if !(settings.endpoint.starts_with("http://") || settings.endpoint.starts_with("https://")) {
        return Err(format!("接口地址无效：{}", settings.endpoint));
    }
    // An empty key would send a bare `Authorization: Bearer ` header, which most
    // gateways report as "no token provided" rather than "bad token" — refuse
    // locally so the cause is unambiguous.
    if settings.api_key.trim().is_empty() {
        return Err(
            "API 密钥解密后为空，请在配置里清空并重新填写（留空不修改不会覆盖已存密钥）"
                .to_owned(),
        );
    }
    // The header goes through a curl config file rather than argv. Two reasons:
    // Rust's Windows argument quoting mangles a value containing spaces before
    // curl's runtime splits it again, which silently dropped the header; and an
    // argv-passed secret is readable from any local process's command line.
    let authorization = format!("Bearer {}", settings.api_key.trim());
    if authorization.contains(['"', '\n', '\r']) {
        return Err("API 密钥含有非法字符，请重新复制。".to_owned());
    }
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_nanos())
        .unwrap_or_default();
    let config_path = std::env::temp_dir().join(format!(
        "wecom-archive-curl-{}-{unique}.cfg",
        std::process::id()
    ));
    std::fs::write(&config_path, curl_config_contents(&authorization))
        .map_err(|_| "无法写入临时请求配置。".to_owned())?;
    let discard_temp = || {
        let _ = std::fs::remove_file(&config_path);
    };

    let mut command = std::process::Command::new("curl.exe");
    command
        .args(curl_arguments(&config_path, &settings.endpoint))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    // Hide the console window curl would otherwise flash on a GUI host.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(_) => {
            discard_temp();
            return Err("系统未找到 curl.exe，无法调用大模型接口。".to_owned());
        }
    };
    // `--data-binary @-` reads the request body from stdin, which also keeps the
    // transcript out of the process command line.
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        if stdin.write_all(payload).is_err() {
            discard_temp();
            let _ = child.kill();
            return Err("发送大模型请求失败。".to_owned());
        }
    }
    let output = match child.wait_with_output() {
        Ok(output) => output,
        Err(_) => {
            discard_temp();
            return Err("未收到大模型响应。".to_owned());
        }
    };
    // The config file held the credential; it must not outlive the call.
    discard_temp();

    let mut stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let mut http_status = None;
    if let Some((body, tail)) = stdout.rsplit_once('\n')
        && let Ok(status) = tail.trim().parse::<i32>()
    {
        http_status = Some(status);
        stdout = body.to_owned();
    }
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let (sent_headers, curl_error) = split_curl_trace(&stderr);
    discard_temp();
    Ok(EndpointOutcome {
        http_status,
        body: stdout,
        curl_error,
        sent_headers,
    })
}

/// Turn curl's verbose stderr into the two facts worth reporting: the request
/// headers that actually went out (credential redacted) and curl's own error
/// lines. Everything else — the "*" transport transcript, the "<" response
/// headers, the "-X is unnecessary" note — is noise that would otherwise bury
/// the one line that explains the failure.
fn split_curl_trace(stderr: &str) -> (Vec<String>, String) {
    let mut sent_headers = Vec::new();
    let mut errors = Vec::new();
    for line in stderr.lines() {
        match line.strip_prefix("> ") {
            // A line with no colon is the request line itself; it carries no
            // credential, so it is safe to show verbatim. The request body is
            // excluded because it is the archived content itself.
            Some(header) if header.trim() != "{" => {
                sent_headers.push(
                    redact_header(header.trim()).unwrap_or_else(|| header.trim().to_owned()),
                );
            }
            // With --silent --show-error, curl prefixes every real failure
            // with "curl: (N) ..."; nothing else on stderr is diagnostic.
            _ if line.starts_with("curl:") => errors.push(line.trim().to_owned()),
            _ => {}
        }
    }
    (sent_headers, errors.join(" ").trim().to_owned())
}

/// Returns the raw assistant text. The transcript is the only thing that
/// leaves the machine.
/// One request against the endpoint. The transcript is the only thing that
/// leaves the machine.
/// The user message: plain text unless media is actually attached, because
/// gateways that never learned about content arrays must keep working for
/// text-only summaries.
fn build_user_content(
    settings: &ActiveSummarySettings,
    label: &str,
    body_text: &str,
    media: &[MediaPart],
) -> serde_json::Value {
    let text = format!("以下是会话「{label}」的沟通记录：\n\n{body_text}");
    if !settings.attach_media || media.is_empty() {
        return serde_json::json!(text);
    }
    let (attachments, note) = attachment_parts(media);
    if attachments.is_empty() && note.is_none() {
        return serde_json::json!(text);
    }
    let mut parts = vec![serde_json::json!({ "type": "text", "text": text })];
    parts.extend(attachments);
    if let Some(note) = note {
        parts.push(serde_json::json!({ "type": "text", "text": note }));
    }
    serde_json::json!(parts)
}

fn call_once(
    settings: &ActiveSummarySettings,
    label: &str,
    body_text: &str,
    media: &[MediaPart],
) -> Result<String, String> {
    let payload = serde_json::json!({
        "model": settings.model,
        "messages": [
            { "role": "system", "content": settings.system_prompt() },
            { "role": "user", "content": build_user_content(settings, label, body_text, media) },
        ],
        "temperature": 0.2,
    });
    let body = serde_json::to_vec(&payload).map_err(|_| "请求构造失败".to_owned())?;

    let outcome = call_endpoint(settings, &body)?;
    if !outcome.is_success() {
        return Err(outcome.describe());
    }
    extract_choice(outcome.body.as_bytes())
}

/// Split on line boundaries so a chunk never cuts a message in half, and split
/// the rare single line longer than the whole budget on character boundaries —
/// byte slicing would tear multi-byte UTF-8 apart and corrupt the transcript.
fn split_transcript(transcript: &str, budget: usize) -> Vec<String> {
    let budget = budget.max(1);
    let mut chunks = Vec::new();
    let mut current = String::new();

    fn flush(current: &mut String, chunks: &mut Vec<String>) {
        if !current.trim().is_empty() {
            chunks.push(std::mem::take(current));
        }
    }

    for line in transcript.lines() {
        if !current.is_empty() && current.len() + line.len() + 1 > budget {
            flush(&mut current, &mut chunks);
        }
        if line.len() > budget {
            for character in line.chars() {
                if !current.is_empty() && current.len() + character.len_utf8() > budget {
                    flush(&mut current, &mut chunks);
                }
                current.push(character);
            }
            current.push('\n');
            flush(&mut current, &mut chunks);
            continue;
        }
        current.push_str(line);
        current.push('\n');
    }
    flush(&mut current, &mut chunks);
    chunks
}

/// What a media attachment is. Pictures travel as a data URL; voices travel as
/// decoded wav/mp3 audio, the only containers an OpenAI-style gateway names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Image,
    Voice,
}

/// One file from the report's media folder, ready to be attached.
#[derive(Debug, Clone)]
pub struct MediaPart {
    pub kind: MediaKind,
    /// MIME type of `bytes`, e.g. `image/png`.
    pub mime: String,
    /// File name shown to the model so it can refer to the picture by name.
    pub label: String,
    pub bytes: Vec<u8>,
}

/// Per-request attachment ceilings. A month of screenshots and voice notes can
/// otherwise push tens of megabytes of base64 at the gateway and get the whole
/// call rejected for no useful gain, so the surplus stays in the local media
/// folder and the model is told which files it did not receive.
///
/// Only pictures are counted. A decoded voice costs about 43 kB of base64 per
/// second of speech, so the shared byte budget is what keeps the request small;
/// a fixed voice count would drop a run of short notes while letting three long
/// recordings through anyway.
///
/// Files are weighed in the order they were sent, so what survives is the start
/// of the conversation: the note tells the model which later files were held
/// back instead of letting it read their absence as a gap.
pub const MAX_ATTACHED_IMAGES: usize = 8;
pub const MAX_ATTACHED_BASE64_CHARS: usize = 8_000_000;

const BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// RFC 4648 base64, written out here so the request path needs no extra
/// dependency (the lockfile may not change without an explicit go-ahead).
pub fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let triple =
            (u32::from(chunk[0]) << 16) | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8) | u32::from(*chunk.get(2).unwrap_or(&0));
        out.push(BASE64_ALPHABET[(triple >> 18 & 63) as usize] as char);
        out.push(BASE64_ALPHABET[(triple >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            BASE64_ALPHABET[(triple >> 6 & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            BASE64_ALPHABET[(triple & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// The audio container an OpenAI-style `input_audio` part can name. WeCom
/// voices arrive as SILK and are decoded to wav before they get here; anything
/// still in another container is reported instead of sent, never silently
/// dropped.
fn audio_format(mime: &str) -> Option<&'static str> {
    match mime {
        "audio/wav" | "audio/x-wav" | "audio/wave" => Some("wav"),
        "audio/mpeg" | "audio/mp3" => Some("mp3"),
        _ => None,
    }
}

/// Split the attachments that fit from the ones that have to stay local, and
/// return the sentence telling the model which files were held back — a picture
/// that silently disappears would otherwise read as a conversation with no
/// screenshots at all.
fn attachment_parts(media: &[MediaPart]) -> (Vec<serde_json::Value>, Option<String>) {
    let mut parts = Vec::new();
    let mut held: Vec<String> = Vec::new();
    let mut used_chars = 0usize;
    let mut images = 0usize;

    for part in media {
        if part.bytes.is_empty() {
            held.push(format!("{}（本地文件为空）", part.label));
            continue;
        }
        if part.kind == MediaKind::Image && images >= MAX_ATTACHED_IMAGES {
            held.push(format!("{}（超过单次附带上限）", part.label));
            continue;
        }
        let format = match part.kind {
            MediaKind::Image => None,
            MediaKind::Voice => match audio_format(&part.mime) {
                Some(format) => Some(format),
                None => {
                    held.push(format!("{}（该音频格式无法随请求发送）", part.label));
                    continue;
                }
            },
        };
        if used_chars + part.bytes.len().div_ceil(3) * 4 > MAX_ATTACHED_BASE64_CHARS {
            held.push(format!("{}（超出本次体积上限）", part.label));
            continue;
        }
        let encoded = base64_encode(&part.bytes);
        used_chars += encoded.len();
        match format {
            None => parts.push(serde_json::json!({
                "type": "image_url",
                "image_url": { "url": format!("data:{};base64,{encoded}", part.mime) }
            })),
            Some(format) => parts.push(serde_json::json!({
                "type": "input_audio",
                "input_audio": { "data": encoded, "format": format }
            })),
        }
        if part.kind == MediaKind::Image {
            images += 1;
        }
    }

    let note = if held.is_empty() {
        None
    } else {
        Some(format!(
            "另有 {} 个文件未随本次请求发送：{}。它们仍保存在报告的媒体目录中。",
            held.len(),
            held.join("、")
        ))
    };
    (parts, note)
}

/// Map-reduce over a long transcript: summarise each chunk on its own, then fold
/// the partial summaries into one. A transcript that already fits is sent once,
/// so this costs nothing in the common case.
// The text-only entry point; the report flow calls `summarize_with_media`.
#[cfg_attr(not(test), allow(dead_code))]
pub fn summarize(
    settings: &ActiveSummarySettings,
    conversation_name: &str,
    transcript: &str,
    max_input_chars: usize,
) -> Result<String, String> {
    summarize_with_media(settings, conversation_name, transcript, &[], max_input_chars)
}

/// Summarize one conversation, attaching its own media to the map-stage calls.
///
/// The caller passes exactly one conversation's files: every request is built
/// from scratch with only that conversation's text and pictures, so nothing
/// said in another chat can reach this call — HTTP has no memory and the system
/// prompt forbids guessing outside the given record. Fold/reduce calls get no
/// media, they only merge summaries that already describe it.
pub fn summarize_with_media(
    settings: &ActiveSummarySettings,
    conversation_name: &str,
    transcript: &str,
    media: &[MediaPart],
    max_input_chars: usize,
) -> Result<String, String> {
    let chunks = split_transcript(transcript, max_input_chars);
    if chunks.len() <= 1 {
        return call_once(settings, conversation_name, transcript, media);
    }

    let mut partials = Vec::with_capacity(chunks.len());
    for (index, chunk) in chunks.iter().enumerate() {
        let label = format!("{conversation_name}（第 {}/{} 段）", index + 1, chunks.len());
        partials.push(call_once(settings, &label, chunk, media)?);
    }

    let combined = partials
        .iter()
        .enumerate()
        .map(|(index, part)| format!("【第 {} 段总结】\n{part}", index + 1))
        .collect::<Vec<_>>()
        .join("\n\n");
    // The reduce stage gets a wider budget than a single chunk: these are short
    // summaries, not raw messages, and losing context here loses conclusions.
    let reduce_budget = max_input_chars.max(1) * 4;
    let parts = split_transcript(&combined, reduce_budget);
    let mut merged = Vec::with_capacity(parts.len());
    for part in &parts {
        let label = if parts.len() == 1 {
            format!("{conversation_name}（合并 {} 段分段总结）", partials.len())
        } else {
            format!("{conversation_name}（分段总结合并，共 {parts_len} 份）", parts_len = parts.len())
        };
        merged.push(call_once(settings, &label, part, &[])?);
    }
    if merged.len() <= 1 {
        return Ok(merged.remove(0));
    }
    let final_join = merged.join("\n\n");
    call_once(
        settings,
        &format!("{conversation_name}（{} 份分段总结的最终汇总）", merged.len()),
        &final_join,
        &[],
    )
}

/// Send a deliberately tiny probe so the operator can see exactly what the
/// configured endpoint answers. It carries no archived content.
pub fn diagnose(settings: &ActiveSummarySettings) -> Result<String, String> {
    let payload = serde_json::json!({
        "model": settings.model,
        "messages": [{ "role": "user", "content": "请只回复两个字：正常" }],
        // Room enough for a reasoning model to think before it answers: with
        // 16 the thinking budget ate the whole reply and it came back empty.
        "max_tokens": 256,
        "temperature": 0,
    });
    let body = serde_json::to_vec(&payload).map_err(|_| "请求构造失败".to_owned())?;
    // State exactly what was sent, minus the secret, so "no token provided"
    // from a gateway can be told apart from a rejected key.
    let sent = format!(
        "实际请求地址：{}\n认证头：Authorization: Bearer\n密钥：{}",
        settings.endpoint,
        summarize_key_shape(settings.api_key.trim())
    );
    // A refusal before curl ever runs (bad address, empty key) still deserves
    // the context above it, so the operator sees what was attempted either way.
    let outcome = call_endpoint(settings, &body).map_err(|error| format!("{sent}\n{error}"))?;
    if !outcome.is_success() {
        return Err(format!("{sent}\n{}", outcome.describe()));
    }
    match extract_choice(outcome.body.as_bytes()) {
        Ok(reply) => Ok(format!("{sent}\n连接成功，模型回复：{}", truncate(&reply, 120))),
        Err(error) => Err(format!(
            "{sent}\n连接成功但无法取用回复（{error}）；原始响应：{}",
            truncate(&outcome.body, 400)
        )),
    }
}

fn extract_choice(body: &[u8]) -> Result<String, String> {
    let value: serde_json::Value = serde_json::from_slice(body)
        .map_err(|_| "大模型响应不是合法 JSON".to_owned())?;
    value["choices"][0]["message"]["content"]
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| "大模型响应中没有 choices[0].message.content".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_are_unusable_until_fully_configured() {
        let mut settings = StoredSummarySettings::default();
        assert!(!settings.is_usable());
        settings.enabled = true;
        settings.endpoint = "https://example.invalid/v1/chat/completions".into();
        settings.model = "model".into();
        assert!(!settings.is_usable(), "still missing the key");
        settings.protected_api_key = Some("00ff".into());
        // The instructions are the system message; without them nothing usable
        // would be sent, so a saved key alone is not enough.
        assert!(!settings.is_usable(), "still missing the instructions");
        settings.prompt = "只输出结论和待办".into();
        assert!(settings.is_usable());
    }

    #[test]
    fn public_view_never_exposes_the_key() {
        let settings = StoredSummarySettings {
            enabled: true,
            endpoint: "https://example.invalid".into(),
            model: "m".into(),
            prompt: String::new(),
            max_input_chars: DEFAULT_MAX_INPUT_CHARS,
            attach_media: true,
            protected_api_key: Some("aabb".into()),
        };
        let public = settings.public();
        assert!(public.has_api_key);
        let encoded = serde_json::to_string(&public).unwrap();
        assert!(!encoded.contains("aabb"));
    }

    #[test]
    fn hex_round_trips_and_rejects_odd_length() {
        assert_eq!(hex_encode(&[0x00, 0xff, 0x10]), "00ff10");
        assert_eq!(hex_decode("00ff10"), Some(vec![0x00, 0xff, 0x10]));
        assert_eq!(hex_decode("0"), None);
        assert_eq!(hex_decode("zz"), None);
    }

    #[test]
    fn missing_key_cannot_be_activated() {
        let settings = StoredSummarySettings {
            enabled: true,
            endpoint: "https://example.invalid".into(),
            model: "m".into(),
            prompt: String::new(),
            max_input_chars: DEFAULT_MAX_INPUT_CHARS,
            attach_media: true,
            protected_api_key: None,
        };
        assert!(settings.to_active().is_err());
    }

    #[test]
    fn corrupt_protected_key_is_rejected() {
        let settings = StoredSummarySettings {
            enabled: true,
            endpoint: "https://example.invalid".into(),
            model: "m".into(),
            prompt: String::new(),
            max_input_chars: DEFAULT_MAX_INPUT_CHARS,
            attach_media: true,
            protected_api_key: Some("not-hex".into()),
        };
        assert!(settings.to_active().is_err());
    }

    #[test]
    fn choice_content_is_extracted_from_openai_shape() {
        let body = r#"{"choices":[{"message":{"content":"  结论是继续推进  "}}]}"#;
        assert_eq!(extract_choice(body.as_bytes()).unwrap(), "结论是继续推进");
        assert!(extract_choice(br#"{"choices":[]}"#).is_err());
        assert!(extract_choice(b"not json").is_err());
    }

    #[test]
    fn non_http_endpoint_is_rejected_before_spawning() {
        let settings = ActiveSummarySettings {
            endpoint: "file:///etc/passwd".into(),
            model: "m".into(),
            prompt: String::new(),
            max_input_chars: DEFAULT_MAX_INPUT_CHARS,
            attach_media: true,
            api_key: Zeroizing::new("k".into()),
        };
        assert!(summarize(&settings, "会话", "内容", DEFAULT_MAX_INPUT_CHARS).is_err());
    }

    #[test]
    fn long_transcripts_split_on_message_boundaries() {
        let transcript = (0..40)
            .map(|index| format!("09-08 14:{:02} 张三：第{index}条消息内容\n", index % 60))
            .collect::<String>();
        let chunks = split_transcript(&transcript, 120);
        assert!(chunks.len() > 1, "should need chunking");
        for chunk in &chunks {
            // No message may be cut in half: every line stays intact.
            assert!(chunk.lines().all(|line| line.ends_with("条消息内容")), "got {chunk}");
        }
        // Nothing may be lost across the split.
        assert_eq!(chunks.concat(), transcript);
    }

    #[test]
    fn short_transcripts_stay_in_one_piece_and_blank_input_is_safe() {
        let short = "09-08 14:00 张三：你好\n";
        assert_eq!(split_transcript(short, 1000), vec![short.to_owned()]);
        assert!(split_transcript("   \n\n", 1000).is_empty());
        // A zero budget is clamped rather than looping forever; production
        // never reaches it because the settings clamp to 1000+.
        let clamped = split_transcript(short, 0);
        assert!(!clamped.is_empty());
        assert_eq!(clamped.concat(), short);
    }

    #[test]
    fn a_single_enormous_line_is_still_chunked() {
        let huge = "x".repeat(5_000);
        let chunks = split_transcript(&huge, 1_000);
        assert!(chunks.len() >= 5);
        // Every character survives; only the trailing newline is normalized.
        assert_eq!(chunks.concat(), format!("{huge}\n"));
    }

    #[test]
    fn sent_headers_are_reported_without_leaking_the_secret() {
        assert_eq!(
            redact_header("Authorization: Bearer sk-secret-value").as_deref(),
            Some("Authorization: Bearer [已打码，15 字符]")
        );
        assert_eq!(
            redact_header("Authorization: sk-secret").as_deref(),
            Some("Authorization: [已打码，9 字符]")
        );
        // Anything that is not the credential passes through untouched.
        assert_eq!(
            redact_header("Content-Type: application/json").as_deref(),
            Some("Content-Type: application/json")
        );
        assert!(redact_header("no colon here").is_none());
    }

    #[test]
    fn the_saved_instructions_come_back_with_the_settings() {
        let stored = StoredSummarySettings {
            enabled: true,
            endpoint: "https://api.example.com/v1".into(),
            model: "m".into(),
            prompt: "总结50-100的沟通记录".into(),
            max_input_chars: DEFAULT_MAX_INPUT_CHARS,
            attach_media: true,
            protected_api_key: Some("00ff".into()),
        };
        let view = PublicSummarySettings::from_stored(&stored);
        // The form reopens with what was last saved, so the next save cannot
        // silently replace it with an empty field.
        assert_eq!(view.prompt, "总结50-100的沟通记录");
        // ...while the credential still never leaves the server.
        assert!(!format!("{:?}", view).contains("00ff"));
        assert!(view.has_api_key);
    }

    #[test]
    fn verbose_transcript_does_not_bury_the_failure() {
        let stderr = [
            "* Host api.example.com:443 was resolved",
            "> POST /v1/chat/completions HTTP/1.1",
            "> Authorization: Bearer sk-secret",
            "> {",
            "< HTTP/1.1 401 Unauthorized",
            "Note: Unnecessary use of -X or --request, POST is already inferred.",
            "curl: (6) Could not resolve host: api.example.com",
        ]
        .join("\n");
        let (sent_headers, curl_error) = split_curl_trace(&stderr);
        assert_eq!(
            sent_headers,
            vec![
                "POST /v1/chat/completions HTTP/1.1".to_owned(),
                "Authorization: Bearer [已打码，9 字符]".to_owned(),
            ]
        );
        // Only curl's own diagnostics survive; the transport transcript and the
        // response headers used to fill 300 characters before any real cause.
        assert_eq!(
            curl_error,
            "curl: (6) Could not resolve host: api.example.com"
        );

        let outcome = EndpointOutcome {
            http_status: Some(401),
            body: "无效的令牌".into(),
            curl_error,
            sent_headers,
        };
        let text = outcome.describe();
        let status = text.find("HTTP 401").expect("status first");
        let response = text.find("响应：").expect("service reply next");
        let hint = text.find("建议：").expect("then the advice");
        let noise = text.find("curl：").expect("diagnostics last");
        assert!(status < response && response < hint && hint < noise, "got {text}");
    }

    #[test]
    fn keys_are_stripped_of_whitespace_from_being_copied() {
        assert_eq!(sanitize_key("sk-abc123"), "sk-abc123");
        assert_eq!(sanitize_key("sk-abc\n123"), "sk-abc123");
        assert_eq!(sanitize_key("  sk-abc\r\n123  "), "sk-abc123");
        assert_eq!(sanitize_key("sk abc\t123"), "skabc123");
        assert_eq!(sanitize_key(""), "");
    }

    #[test]
    fn key_shape_is_reported_without_revealing_it() {
        assert_eq!(summarize_key_shape(""), "空");
        assert_eq!(summarize_key_shape("sk-abc123"), "9 字符（数字+字母+符号）");
        assert_eq!(summarize_key_shape("sk_abc"), "6 字符（字母+符号）");
        assert!(
            summarize_key_shape("sk密钥").contains("非 ASCII"),
            "got {}",
            summarize_key_shape("sk密钥")
        );
    }

    #[test]
    fn base_url_is_completed_the_way_config_switchers_do() {
        // The mistake that produced `Invalid URL (POST /v1)`.
        assert_eq!(
            resolve_endpoint("https://api.example.com/v1"),
            "https://api.example.com/v1/chat/completions"
        );
        assert_eq!(
            resolve_endpoint("https://api.example.com/v1/"),
            "https://api.example.com/v1/chat/completions"
        );
        // No /v1 at all: the conventional path is supplied.
        assert_eq!(
            resolve_endpoint("https://api.example.com"),
            "https://api.example.com/v1/chat/completions"
        );
        // Already complete: left exactly as given.
        assert_eq!(
            resolve_endpoint("https://api.example.com/v1/chat/completions"),
            "https://api.example.com/v1/chat/completions"
        );
        assert_eq!(
            resolve_endpoint("https://gw.example.com/openai/v1/chat/completions"),
            "https://gw.example.com/openai/v1/chat/completions"
        );
        assert_eq!(resolve_endpoint("   "), "");
    }

    #[test]
    fn config_file_is_the_argument_right_behind_the_config_flag() {
        let arguments = curl_arguments(
            Path::new("C:\\temp\\probe.cfg"),
            "https://example.invalid/v1/chat/completions",
        );
        let position = arguments
            .iter()
            .position(|value| value.to_string_lossy() == "--config")
            .expect("--config must be passed");
        // curl reads whatever follows --config as a file name. Let any other
        // option sit there and the call dies with "cannot read config from
        // '--verbose'" before a single byte leaves the machine.
        assert_eq!(arguments[position + 1].to_str(), Some("C:\\temp\\probe.cfg"));
        assert!(arguments
            .iter()
            .any(|value| value.to_string_lossy() == "--verbose"));
        // -q has to come first, or a user .curlrc is read before it applies.
        assert_eq!(
            arguments.first().map(|value| value.to_string_lossy().into_owned()),
            Some("-q".to_owned())
        );
        // The URL stays last and the config path never becomes the URL.
        assert_eq!(
            arguments.last().map(|value| value.to_string_lossy().into_owned()),
            Some("https://example.invalid/v1/chat/completions".to_owned())
        );
    }

    #[test]
    fn credential_header_is_written_with_its_name() {
        // curl keeps only header lines that contain a colon. Without the name
        // the line is dropped and the request goes out unauthenticated, which
        // the gateway reports as "no token provided" instead of a bad key.
        assert_eq!(
            curl_config_contents("Bearer sk-test"),
            "header = \"Authorization: Bearer sk-test\"\n"
        );
    }

    #[test]
    fn a_versioned_base_path_does_not_gain_a_second_version() {
        // Zhipu serves chat completions straight under .../api/paas/v4.
        assert_eq!(
            resolve_endpoint("https://open.bigmodel.cn/api/paas/v4"),
            "https://open.bigmodel.cn/api/paas/v4/chat/completions"
        );
        assert_eq!(
            resolve_endpoint("https://gw.example.com/api/v10/"),
            "https://gw.example.com/api/v10/chat/completions"
        );
        // An origin on its own still gets the conventional /v1.
        assert_eq!(
            resolve_endpoint("https://api.example.com"),
            "https://api.example.com/v1/chat/completions"
        );
        // A host that merely starts with a version is not a versioned path.
        assert_eq!(
            resolve_endpoint("https://v1.example.com"),
            "https://v1.example.com/v1/chat/completions"
        );
        // .../v1 behaves exactly as it always did.
        assert_eq!(
            resolve_endpoint("https://api.example.com/v1"),
            "https://api.example.com/v1/chat/completions"
        );
    }

    #[test]
    fn a_complete_form_can_be_probed_before_the_switch_is_armed() {
        let mut settings = StoredSummarySettings {
            enabled: false,
            endpoint: "https://example.invalid/v1".into(),
            model: "m".into(),
            prompt: "按五段结构总结".into(),
            max_input_chars: DEFAULT_MAX_INPUT_CHARS,
            attach_media: true,
            protected_api_key: Some("00ff".into()),
        };
        assert!(settings.is_configured());
        assert!(!settings.is_usable(), "the switch is still off");
        settings.enabled = true;
        assert!(settings.is_usable());
        // Without a key neither holds: nothing could be sent.
        settings.protected_api_key = None;
        assert!(!settings.is_configured());
        assert!(!settings.is_usable());
        // Nor without instructions: they are the system message, and there is
        // no built-in one to fall back to.
        settings.protected_api_key = Some("00ff".into());
        settings.prompt = "   ".into();
        assert!(!settings.is_configured());
        assert!(!settings.is_usable());
    }

    #[test]
    fn truncated_endpoint_is_explained_instead_of_bare_404() {
        // The wording WeChat-style gateways return when the address stops at /v1.
        let outcome = EndpointOutcome {
            http_status: Some(404),
            body: "{\"error\":{\"message\":\"Invalid URL (POST /v1)\",\"type\":\"invalid_request_error\"}}"
                .into(),
            curl_error: String::new(),
            sent_headers: Vec::new(),
        };
        let text = outcome.describe();
        assert!(text.contains("HTTP 404"), "got {text}");
        assert!(text.contains("/v1/chat/completions"), "got {text}");
    }

    #[test]
    fn common_failures_map_to_a_concrete_next_step() {
        assert!(hint_for(Some(401), "Incorrect API key provided").is_some());
        assert!(hint_for(Some(403), "forbidden").is_some());
        assert!(hint_for(Some(429), "rate limited").is_some());
        assert!(hint_for(Some(404), "model gpt-x does not exist").is_some());
        // A success carries no hint.
        assert!(hint_for(Some(200), "").is_none());
        assert!(hint_for(Some(500), "internal error").is_none());
    }

    /// The operator's wording is the system message, and it is the only one:
    /// nothing is appended after it, and there is no built-in template to
    /// fall back to.
    #[test]
    fn the_operator_prompt_is_the_whole_system_message() {
        let custom = ActiveSummarySettings {
            endpoint: "https://example.invalid".into(),
            model: "m".into(),
            prompt: "只输出三行：结论、待办、风险".into(),
            max_input_chars: DEFAULT_MAX_INPUT_CHARS,
            attach_media: true,
            api_key: Zeroizing::new("k".into()),
        };
        assert_eq!(custom.system_prompt(), "只输出三行：结论、待办、风险");

        // Surrounding whitespace is trimmed, and a blank prompt stays blank
        // rather than becoming some template.
        let padded = ActiveSummarySettings {
            prompt: "  只输出结论  ".into(),
            ..custom
        };
        assert_eq!(padded.system_prompt(), "只输出结论");

        // The user message is the conversation of one customer, and the
        // instructions never travel with it.
        let content = build_user_content(
            &padded,
            "张三（2026-09-01 ~ 2026-09-30）",
            "10:00 张三：你好",
            &[],
        );
        assert_eq!(
            content.as_str().unwrap(),
            "以下是会话「张三（2026-09-01 ~ 2026-09-30）」的沟通记录：\n\n10:00 张三：你好"
        );
    }

    #[test]
    fn outcome_explains_status_curl_and_body() {
        let outcome = EndpointOutcome {
            http_status: Some(401),
            body: "invalid api key".into(),
            curl_error: String::new(),
            sent_headers: Vec::new(),
        };
        assert!(!outcome.is_success());
        let text = outcome.describe();
        assert!(text.contains("HTTP 401"), "got {text}");
        assert!(text.contains("invalid api key"), "got {text}");

        let ok = EndpointOutcome {
            http_status: Some(200),
            ..Default::default()
        };
        assert!(ok.is_success());
        assert_eq!(ok.describe(), "HTTP 200");

        // No status at all means curl never reached the service.
        let offline = EndpointOutcome {
            curl_error: "Could not resolve host".into(),
            ..Default::default()
        };
        assert!(!offline.is_success());
        assert!(offline.describe().contains("Could not resolve host"));
    }

    #[test]
    fn long_values_are_trimmed_for_display() {
        assert_eq!(truncate("abc", 10), "abc");
        let long = "x".repeat(50);
        let trimmed = truncate(&long, 10);
        assert!(trimmed.chars().count() <= 11);
        assert!(trimmed.ends_with(ELLIPSIS));
    }

    #[test]
    fn base64_matches_the_canonical_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    /// A settings file from before the switch existed must come back with
    /// media attached: that is the behaviour the feature was asked for.
    #[test]
    fn attach_media_defaults_on_when_the_field_is_missing() {
        let stored: StoredSummarySettings = serde_json::from_str(r#"{"enabled":true}"#).unwrap();
        assert!(stored.attach_media);
        assert!(StoredSummarySettings::default().attach_media);
        // And a saved "off" survives a load/save round trip.
        let stored: StoredSummarySettings =
            serde_json::from_str(r#"{"enabled":true,"attachMedia":false}"#).unwrap();
        assert!(!stored.attach_media);
    }

    fn picture(label: &str, byte: u8) -> MediaPart {
        MediaPart {
            kind: MediaKind::Image,
            mime: "image/png".into(),
            label: label.into(),
            bytes: vec![byte],
        }
    }

    #[test]
    fn images_travel_as_data_urls_and_voices_carry_their_format() {
        let media = vec![
            picture("01.png", 1),
            MediaPart {
                kind: MediaKind::Voice,
                mime: "audio/wav".into(),
                label: "02.wav".into(),
                bytes: b"RIFF".to_vec(),
            },
        ];
        let (parts, note) = attachment_parts(&media);
        assert_eq!(parts.len(), 2);
        let url = parts[0]["image_url"]["url"].as_str().unwrap();
        assert_eq!(url, format!("data:image/png;base64,{}", base64_encode(&[1])));
        assert_eq!(parts[1]["type"], "input_audio");
        assert_eq!(parts[1]["input_audio"]["format"], "wav");
        assert!(note.is_none(), "everything fit, nothing to report");
    }

    /// A voice in a container no gateway names cannot be sent as-is; the model
    /// must still be told it exists so a missing audio is never read as a missing
    /// conversation.
    #[test]
    fn unsupported_voices_are_reported_not_silently_dropped() {
        let media = vec![MediaPart {
            kind: MediaKind::Voice,
            mime: "audio/amr".into(),
            label: "语音01.amr".into(),
            bytes: b"#!AMR".to_vec(),
        }];
        let (parts, note) = attachment_parts(&media);
        assert!(parts.is_empty());
        let note = note.expect("the held file must be named");
        assert!(note.contains("语音01.amr"), "got {note}");
        assert!(note.contains("无法随请求发送"), "got {note}");
    }

    /// Every voice goes out when it fits: only the byte budget may hold audio
    /// back, never a count, because a conversation of short notes must not lose
    /// the later ones.
    #[test]
    fn voices_are_limited_by_size_and_not_by_count() {
        let media = (0..12)
            .map(|index| MediaPart {
                kind: MediaKind::Voice,
                mime: "audio/wav".into(),
                label: format!("语音{index:02}.wav"),
                bytes: vec![index as u8; 1_024],
            })
            .collect::<Vec<_>>();
        let (parts, note) = attachment_parts(&media);
        assert_eq!(parts.len(), media.len());
        assert!(note.is_none(), "12 short voices fit in the budget");
        assert!(parts.iter().all(|part| part["type"] == "input_audio"));
    }

    /// The byte budget is what stops a long recording from crowding out
    /// everything behind it.
    #[test]
    fn oversized_voices_are_held_back_with_a_reason() {
        let media = vec![MediaPart {
            kind: MediaKind::Voice,
            mime: "audio/wav".into(),
            label: "语音01.wav".into(),
            bytes: vec![0u8; MAX_ATTACHED_BASE64_CHARS / 4 * 3 + 3],
        }];
        let (parts, note) = attachment_parts(&media);
        assert!(parts.is_empty());
        let note = note.expect("the held file must be named");
        assert!(note.contains("语音01.wav"), "got {note}");
        assert!(note.contains("体积上限"), "got {note}");
    }

    #[test]
    fn attachment_counts_are_capped_per_request() {
        let mut media = Vec::new();
        for index in 0..MAX_ATTACHED_IMAGES + 3 {
            media.push(picture(&format!("{index:02}.png"), index as u8));
        }
        let (parts, note) = attachment_parts(&media);
        assert_eq!(parts.len(), MAX_ATTACHED_IMAGES);
        let note = note.expect("the surplus must be reported");
        assert!(note.contains("上限"), "got {note}");
        assert!(note.contains("媒体目录"), "got {note}");
    }

    /// Text-only requests keep the exact payload shape that always worked, so
    /// a gateway without multimodal support is never broken by this feature.
    #[test]
    fn user_content_stays_a_plain_string_without_media() {
        let settings = ActiveSummarySettings {
            endpoint: "https://example.invalid".into(),
            model: "m".into(),
            prompt: String::new(),
            max_input_chars: DEFAULT_MAX_INPUT_CHARS,
            attach_media: true,
            api_key: Zeroizing::new("k".into()),
        };
        let content = build_user_content(&settings, "张三", "正文", &[]);
        assert!(content.is_string());

        // The switch off must also keep the old shape even when media exists.
        let mut off = settings.clone();
        off.attach_media = false;
        let content = build_user_content(&off, "张三", "正文", &[picture("01.png", 1)]);
        assert!(content.is_string());
    }

    #[test]
    fn user_content_arrays_carry_text_then_attachments() {
        let settings = ActiveSummarySettings {
            endpoint: "https://example.invalid".into(),
            model: "m".into(),
            prompt: String::new(),
            max_input_chars: DEFAULT_MAX_INPUT_CHARS,
            attach_media: true,
            api_key: Zeroizing::new("k".into()),
        };
        let content = build_user_content(&settings, "张三", "正文", &[picture("01.png", 1)]);
        let parts = content.as_array().expect("array when media attached");
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0]["type"], "text");
        assert!(parts[0]["text"].as_str().unwrap().contains("正文"));
        assert_eq!(parts[1]["type"], "image_url");
    }

    /// One request carries one customer's conversation and nothing else, so
    /// isolation no longer depends on any wording: there is no built-in
    /// prompt left to promise it, and the payload simply cannot mention
    /// another conversation because it never contains one.
    #[test]
    fn a_request_carries_one_conversation_and_no_prompt_template() {
        let settings = ActiveSummarySettings {
            endpoint: "https://example.invalid".into(),
            model: "m".into(),
            prompt: "按五段结构总结".into(),
            max_input_chars: DEFAULT_MAX_INPUT_CHARS,
            attach_media: true,
            api_key: Zeroizing::new("k".into()),
        };
        let content = build_user_content(&settings, "张三（2026-09）", "10:00 张三：你好", &[]);
        let text = content.as_str().unwrap();
        assert!(text.starts_with("以下是会话「张三（2026-09）」的沟通记录："), "got {text}");
        assert!(text.ends_with("10:00 张三：你好"), "got {text}");
        // Whatever the operator wrote lives in the system message only.
        assert!(!text.contains("按五段结构总结"), "got {text}");
    }
}
