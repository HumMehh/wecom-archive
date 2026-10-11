//! Markdown transcript and workbook rendering.

use std::path::{Path, PathBuf};

use archive_domain::MessageType;
use chrono::{DateTime, Duration, Utc};
use rust_xlsxwriter::{Color, Format, Image, ObjectMovement, Url, Workbook, Worksheet};

use super::media::MediaPaths;
use super::{ReportError, ReportMessage, ReportOptions};

pub const WORKBOOK_NAME: &str = "沟通记录汇总.xlsx";

const TYPE_LABELS: &[(MessageType, &str)] = &[
    (MessageType::Text, "文本"),
    (MessageType::Image, "图片"),
    (MessageType::Audio, "语音"),
    (MessageType::Video, "视频"),
    (MessageType::File, "文件"),
    (MessageType::Link, "链接"),
    (MessageType::Reply, "引用"),
    (MessageType::System, "系统"),
    (MessageType::Unsupported, "未知"),
];

/// How a media file is presented in the sheet: a picture is embedded as a
/// thumbnail inside the 聊天记录 cell, a voice or an attachment stays a file
/// on disk that the cell names and links to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Image,
    Voice,
    File,
}

impl MediaKind {
    pub fn label(self) -> &'static str {
        match self {
            MediaKind::Image => "图片",
            MediaKind::Voice => "语音",
            MediaKind::File => "附件",
        }
    }
}

/// One exported file that belongs to a row, with both the path Excel links to
/// and the absolute path rust_xlsxwriter needs to embed a thumbnail.
#[derive(Debug, Clone)]
pub struct RowMediaFile {
    /// Forward-slash path relative to the export directory, safe to embed.
    pub relative: String,
    pub absolute: PathBuf,
    pub kind: MediaKind,
    /// Display name: the original file name when the archive recorded one.
    pub label: String,
}

/// One conversation inside a person-month row. Each section carries its own
/// transcript file and its own media, so a summariser can be asked about this
/// conversation alone — never the other conversations that share the row.
#[derive(Debug, Clone)]
pub struct RowSection {
    /// The conversation's own name: the other party for a direct chat, the
    /// group name for a group chat.
    pub conversation: String,
    /// Markdown transcript of just this conversation, in the notes directory.
    pub note: PathBuf,
    /// Media referenced by this conversation's messages only.
    pub media: Vec<RowMediaFile>,
}

#[derive(Debug, Clone)]
pub struct PersonMonthRow {
    /// Stable identity used when a summary comes back for this row.
    pub key: String,
    /// The other person's WeCom display name, which is the remark when one exists.
    pub name: String,
    /// Calendar month the row covers, as `YYYY-MM`.
    pub month: String,
    /// Human readable span, for example `2026-09-01 ~ 2026-09-30`.
    pub period: String,
    pub message_count: usize,
    pub text_count: usize,
    pub image_count: usize,
    pub audio_count: usize,
    pub file_count: usize,
    pub voice_with_audio: usize,
    pub missing_media: usize,
    pub first_at: Option<DateTime<Utc>>,
    pub last_at: Option<DateTime<Utc>>,
    pub highlights: String,
    /// Every message of that person in that month, trimmed to the cell limit.
    pub transcript: String,
    pub summary: String,
    /// Conversations that make up this row, each with its own transcript file
    /// and media. Empty only for rows built by tests.
    pub sections: Vec<RowSection>,
}

impl PersonMonthRow {
    /// Every exported file of this row, in conversation order.
    pub fn media_files(&self) -> impl Iterator<Item = &RowMediaFile> {
        self.sections.iter().flat_map(|section| section.media.iter())
    }

    pub fn count_media(&self, kind: MediaKind) -> usize {
        self.media_files().filter(|file| file.kind == kind).count()
    }
}

fn type_label(message_type: MessageType) -> &'static str {
    TYPE_LABELS
        .iter()
        .find(|(candidate, _)| *candidate == message_type)
        .map(|(_, label)| *label)
        .unwrap_or("未知")
}

fn local(at: DateTime<Utc>, utc_offset_minutes: i32) -> DateTime<Utc> {
    at + Duration::minutes(i64::from(utc_offset_minutes))
}

fn format_local(at: DateTime<Utc>, utc_offset_minutes: i32) -> String {
    local(at, utc_offset_minutes).format("%Y-%m-%d %H:%M:%S").to_string()
}

/// One Markdown transcript per person per month, mirroring the sheet rows.
pub fn render_person_markdown(
    name: &str,
    period: &str,
    messages: &[ReportMessage],
    media_paths: &MediaPaths,
    options: ReportOptions,
) -> String {
    let span = format!(
        "{} 至 {}",
        messages
            .first()
            .map(|message| format_local(message.sent_at, options.utc_offset_minutes))
            .unwrap_or_else(|| "-".into()),
        messages
            .last()
            .map(|message| format_local(message.sent_at, options.utc_offset_minutes))
            .unwrap_or_else(|| "-".into())
    );
    let media = format!(
        "语音含音频 {} 条，未落盘 {} 项",
        messages
            .iter()
            .filter(|message| message.message_type == MessageType::Audio)
            .flat_map(|message| message.media.iter())
            .filter(|item| item.content_hash.is_some())
            .count(),
        messages
            .iter()
            .flat_map(|message| message.media.iter())
            .filter(|item| item.content_hash.is_none())
            .count()
    );

    let mut out = String::new();
    out.push_str(&format!("# 沟通记录：{name}（{period}）\n\n"));
    out.push_str(&summary_line(messages, "姓名", name));
    out.push_str(&summary_line(messages, "时间", period));
    out.push_str(&summary_line(messages, "消息数", &messages.len().to_string()));
    out.push_str(&summary_line(
        messages,
        "参与者",
        &participant_counts(messages).join("、"),
    ));
    out.push_str(&summary_line(messages, "实际时间跨度", &span));
    out.push_str(&summary_line(messages, "媒体", &media));
    out.push_str("\n## 对话全文\n\n");

    for message in messages {
        out.push_str(&render_message(message, media_paths, options));
        out.push('\n');
    }

    out
}

fn summary_line(messages: &[ReportMessage], label: &str, value: &str) -> String {
    if messages.is_empty() {
        return String::new();
    }
    format!("- {label}：{value}\n")
}

fn participant_counts(messages: &[ReportMessage]) -> Vec<String> {
    let mut counts: std::collections::BTreeMap<String, usize> = Default::default();
    for message in messages {
        *counts.entry(message.sender().to_owned()).or_default() += 1;
    }
    let mut entries: Vec<(String, usize)> = counts.into_iter().collect();
    entries.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    entries
        .into_iter()
        .map(|(name, count)| format!("{name}({count})"))
        .collect()
}

fn render_message(
    message: &ReportMessage,
    media_paths: &MediaPaths,
    options: ReportOptions,
) -> String {
    let mut lines = vec![format!(
        "- **{} {}**（{}）",
        format_local(message.sent_at, options.utc_offset_minutes),
        message.sender(),
        type_label(message.message_type.clone())
    )];

    if let Some(body) = message
        .body_text
        .as_deref()
        .map(str::trim)
        .filter(|body| !body.is_empty())
    {
        for line in body.lines() {
            lines.push(format!("  {}", line.trim_end()));
        }
    }

    for media in &message.media {
        let name = media
            .original_name
            .as_deref()
            .unwrap_or("未命名附件")
            .to_owned();
        match media
            .content_hash
            .as_deref()
            .and_then(|hash| media_paths.get(hash))
        {
            Some(relative) if message.message_type == MessageType::Image => {
                lines.push(format!("  ![{name}]({relative})"));
            }
            Some(relative) => {
                lines.push(format!("  附件：{name} → `{relative}`"));
            }
            None => {
                let reason = media
                    .missing_reason
                    .as_deref()
                    .unwrap_or("文件未落盘");
                lines.push(format!("  附件：{name}（未采集：{reason}）"));
            }
        }
    }
    lines.join("\n")
}

/// The sheet the user asked for: one row per person per month, with the summary
/// right next to the name. Everything a row owns — the conversation, its
/// pictures and its media files — lives inside 聊天记录, and nothing gets a row
/// of its own.
const COLUMNS: &[(&str, f64)] = &[
    ("姓名", 22.0),
    ("总结记录", 60.0),
    ("时间", 26.0),
    ("消息数", 9.0),
    ("聊天记录", 100.0),
    ("要点摘录", 60.0),
];

/// Column that holds the conversation, its embedded thumbnails and the link
/// to the row's media directory.
const TRANSCRIPT_COLUMN: u16 = 4;
/// Column index of 要点摘录, which is written in green.
const HIGHLIGHT_COLUMN: usize = 5;

/// Thumbnail width in pixels, slightly narrower than the 聊天记录 column so a
/// picture never overlaps the next column.
const THUMBNAIL_WIDTH: f64 = 168.0;
/// Smallest thumbnail width: a picture is scaled up to this if it is tinier,
/// otherwise a small source would leave an invisible dot in the cell.
const MIN_THUMBNAIL_WIDTH: f64 = 48.0;

/// The box one picture is scaled into: width capped at THUMBNAIL_WIDTH,
/// aspect kept, never shrunk below the minimum. The planner and the insertion
/// step must agree on this number or the row would be sized for a picture the
/// drawing does not match.
pub(crate) fn fit_thumbnail(width: f64, height: f64) -> (f64, f64) {
    if width <= 0.0 || height <= 0.0 {
        return (width.max(0.0), height.max(0.0));
    }
    let target_w = width.clamp(MIN_THUMBNAIL_WIDTH, THUMBNAIL_WIDTH);
    (target_w, height * target_w / width)
}
/// Excel refuses to make a row taller than this, so thumbnails that do not fit
/// stay in the media directory instead of being clipped.
const MAX_ROW_HEIGHT_POINTS: f64 = 409.0;
/// One wrapped line of 11pt text: 15 points, i.e. 20 pixels.
const TEXT_LINE_POINTS: f64 = 15.0;
const TEXT_LINE_PIXELS: f64 = 20.0;

/// One wrapped line of conversation text that must stay readable below the
/// pictures, so the cell never looks like it only holds pictures.
const TRAILING_TEXT_LINES: f64 = 4.0;

/// Where thumbnails start inside the 聊天记录 cell and how tall the row has to
/// be.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct MediaLayout {
    pub shown: usize,
    pub row_height_points: f64,
    /// Pixels used by the hint text above the pictures, i.e. where the first
    /// thumbnail starts.
    pub text_height_pixels: f64,
    /// Pixels the shown thumbnails occupy below the hint.
    pub shown_pixels: f64,
}

impl MediaLayout {
    /// First line index the conversation may use: the hint text and the pictures
    /// above it are drawn from the top of the cell, and Excel paints cell text
    /// from the same origin, so pushing the text down is the only way to keep
    /// both readable.
    pub fn transcript_start_line(&self) -> usize {
        ((self.text_height_pixels + self.shown_pixels) / TEXT_LINE_PIXELS).ceil() as usize
    }

    /// Newlines to append after `media_lines` lines of hint text so the
    /// conversation begins exactly on `transcript_start_line`.
    ///
    /// After m lines of hint, appending n newlines puts the next line at index
    /// m + n - 1, so n is `start + 1 - m`. At least one newline always stays,
    /// which keeps the first message off the hint line.
    pub fn text_spacer_lines(&self, media_lines: usize) -> usize {
        (self.transcript_start_line() + 1)
            .saturating_sub(media_lines)
            .max(1)
    }
}

/// Plan the media block that heads the 聊天记录 cell: the hint text on top, then
/// as many thumbnails as Excel's row height limit allows, then room for a few
/// lines of the conversation. Pure so a test can pin the 409-point ceiling.
pub(crate) fn plan_media_layout(text_lines: usize, image_sizes: &[(f64, f64)]) -> MediaLayout {
    let text_height_pixels = text_lines as f64 * TEXT_LINE_PIXELS;
    let mut used_pixels = text_height_pixels;
    let mut shown_pixels = 0.0;
    let mut shown = 0usize;
    // The trailing lines are reserved while the pictures are fitted, so the row
    // is never so tall that the conversation below them is cut off.
    for (width, height) in image_sizes {
        let (_, scaled) = fit_thumbnail(*width, *height);
        if (used_pixels + scaled + TRAILING_TEXT_LINES * TEXT_LINE_PIXELS) * 0.75
            > MAX_ROW_HEIGHT_POINTS
        {
            break;
        }
        used_pixels += scaled;
        shown_pixels += scaled;
        shown += 1;
    }
    MediaLayout {
        shown,
        row_height_points: (used_pixels * 0.75 + TRAILING_TEXT_LINES * TEXT_LINE_POINTS)
            .clamp(TEXT_LINE_POINTS, MAX_ROW_HEIGHT_POINTS),
        text_height_pixels,
        shown_pixels,
    }
}

/// The hint that heads the 聊天记录 cell: what the row owns, and where to click
/// it. It sits above the thumbnails and above the conversation.
///
/// `embedded_images` is how many pictures the row really shows as thumbnails;
/// a row whose pictures all stayed on disk says so instead of pointing at a
/// thumbnail that is not there.
pub(crate) fn media_cell_text(row: &PersonMonthRow, embedded_images: usize) -> String {
    let images = row.count_media(MediaKind::Image);
    let voices = row.count_media(MediaKind::Voice);
    let files = row.count_media(MediaKind::File);
    if images + voices + files == 0 {
        return String::new();
    }
    let mut lines = Vec::new();
    if images > 0 {
        lines.push(if embedded_images > 0 {
            format!("图片 {images} 张（缩略图见下，点缩略图看原图）")
        } else {
            format!("图片 {images} 张（原图在媒体目录）")
        });
    }
    if voices + files > 0 {
        lines.push(format!(
            "{}（{}）",
            media_counts(voices, files),
            media_location_hint(row)
        ));
    }
    lines.join("\n")
}

/// 「语音 2 条、附件 1 个」 — only the kinds the row actually has.
fn media_counts(voices: usize, files: usize) -> String {
    let mut counts = Vec::new();
    if voices > 0 {
        counts.push(format!("语音 {voices} 条"));
    }
    if files > 0 {
        counts.push(format!("附件 {files} 个"));
    }
    counts.join("、")
}

/// Where the originals are, in the words the cell can keep. The folder is only
/// promised as clickable when the cell really does carry that one link.
fn media_location_hint(row: &PersonMonthRow) -> String {
    let directories = row_media_directories(row);
    match directories.as_slice() {
        [only] => format!("原文件在 {only}/，点本单元格打开文件夹"),
        [] => "原文件在导出目录的媒体文件夹".to_owned(),
        many => format!("原文件在 {} 下", many.join("、")),
    }
}

/// The directories a row's media files live in, without duplicates and in a
/// stable order.
pub(crate) fn row_media_directories(row: &PersonMonthRow) -> Vec<String> {
    let mut directories: Vec<String> = row
        .sections
        .iter()
        .flat_map(|section| section.media.iter())
        .filter_map(|file| file.relative.rsplit_once('/'))
        .map(|(parent, _)| parent.to_owned())
        .collect();
    directories.sort();
    directories.dedup();
    directories
}

/// The single directory a row's media lives in. A cell carries one hyperlink at
/// most, so a row spread over several directories is described in words instead
/// of being linked to one of them.
pub(crate) fn row_media_directory(row: &PersonMonthRow) -> Option<String> {
    match row_media_directories(row).as_slice() {
        [only] => Some(only.clone()),
        _ => None,
    }
}

/// The hint text and the picture layout of a row, resolved together: the wording
/// says how many pictures are embedded, the layout says how many fit, and the
/// line that names the leftovers can push a picture out of the row.
pub(crate) fn plan_media_hint(
    row: &PersonMonthRow,
    image_sizes: &[(f64, f64)],
) -> (String, MediaLayout) {
    let images = row.count_media(MediaKind::Image);
    // The hint always occupies the same number of lines, whatever it says, so
    // the layout can be planned before the wording is final.
    let base_lines = media_cell_text(row, 0).lines().count();
    let mut lines = base_lines;
    let mut layout = plan_media_layout(lines, image_sizes);
    let mut hidden = images.saturating_sub(layout.shown);
    if hidden > 0 {
        lines = base_lines + 1;
        layout = plan_media_layout(lines, image_sizes);
        hidden = images.saturating_sub(layout.shown);
    }
    let mut text = media_cell_text(row, layout.shown);
    if hidden > 0 {
        text = format!("{text}\n另有 {hidden} 张图片未嵌入，原图在媒体目录");
    }
    (text, layout)
}

/// The 聊天记录 cell: the media hint first, then the conversation itself, with
/// the pictures' own height left blank so no message hides behind them.
///
/// Excel cannot put a picture inside a run of text, so "the media sits at the
/// position of its message" is realised as: the hint line names what this row
/// owns, the thumbnails are anchored right below that hint inside this very
/// cell, and the conversation text starts below the pictures. A voice or an
/// attachment cannot be embedded at all, so it stays a file on disk and the
/// cell reaches it in words — the per-message `[名称 -> 路径]` markers that
/// `render_transcript` writes, plus the folder the hint names.
pub(crate) fn conversation_cell(row: &PersonMonthRow, media_text: &str, spacer_lines: usize) -> String {
    if media_text.is_empty() {
        return row.transcript.clone();
    }
    format!(
        "{media_text}{}{}",
        "\n".repeat(spacer_lines),
        row.transcript
    )
}

/// Excel refuses to store more than this in a single cell.
pub const EXCEL_CELL_LIMIT: usize = 32_767;

/// The conversation as plain text, one message per line, so the sheet is usable
/// even when no summarizer is configured.
pub fn render_transcript(
    messages: &[ReportMessage],
    media_paths: &MediaPaths,
    options: ReportOptions,
) -> String {
    messages
        .iter()
        .map(|message| {
            let time = local(message.sent_at, options.utc_offset_minutes)
                .format("%m-%d %H:%M")
                .to_string();
            let body = message
                .body_text
                .as_deref()
                .map(str::trim)
                .filter(|body| !body.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| format!("[{}]", type_label(message.message_type.clone())));
            let attachments = message
                .media
                .iter()
                .map(|media| {
                    let name = media
                        .original_name
                        .as_deref()
                        .unwrap_or("附件")
                        .to_owned();
                    let suffix = media
                        .content_hash
                        .as_deref()
                        .and_then(|hash| media_paths.get(hash))
                        .map(|relative| format!(" -> {relative}"))
                        .unwrap_or_else(|| {
                            media
                                .missing_reason
                                .as_deref()
                                .map(|reason| format!("（{reason}）"))
                                .unwrap_or_default()
                        });
                    format!("[{name}{suffix}]")
                })
                .collect::<Vec<_>>()
                .join("");
            format!("{time} {}：{body}{attachments}", message.sender())
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Cut the transcript on a line boundary so a cell never starts mid-sentence,
/// and say how much was dropped instead of silently losing the tail.
pub fn truncate_for_cell(text: &str) -> String {
    if text.chars().count() <= EXCEL_CELL_LIMIT {
        return text.to_owned();
    }
    let mut kept: String = text.chars().take(EXCEL_CELL_LIMIT - 40).collect();
    while kept.ends_with('\n') || kept.ends_with(' ') {
        kept.pop();
    }
    let total = text.chars().count();
    kept.push_str(&format!("\n……（已截断，共 {total} 字，完整全文见沟通记录目录）"));
    kept
}

pub fn write_workbook(path: &Path, rows: &[PersonMonthRow]) -> Result<(), ReportError> {
    let mut workbook = Workbook::new();
    let worksheet = workbook.add_worksheet();
    write_sheet(worksheet, rows)?;

    let mut partial = path.as_os_str().to_os_string();
    partial.push(".partial");
    workbook
        .save(&partial)
        .map_err(|error| ReportError::Workbook(error.to_string()))?;
    std::fs::rename(&partial, path)?;
    Ok(())
}

/// Writes the header and then one row per person-month, and returns how many
/// Excel rows that took.
///
/// Media never gets a row of its own: the pictures are anchored inside the
/// 聊天记录 cell and everything else is named in that same cell, so the count is
/// always `1 + rows.len()` no matter how much media a row carries.
fn write_sheet(worksheet: &mut Worksheet, rows: &[PersonMonthRow]) -> Result<u32, ReportError> {
    worksheet
        .set_freeze_panes(1, 0)
        .map_err(workbook_error)?;

    let header = Format::new().set_bold().set_text_wrap();
    let body = Format::new().set_text_wrap();
    let highlight = Format::new()
        .set_text_wrap()
        .set_font_color(Color::Green);

    for (column, (title, width)) in COLUMNS.iter().enumerate() {
        let column = column as u16;
        worksheet
            .write_string_with_format(0, column, *title, &header)
            .map_err(workbook_error)?;
        worksheet
            .set_column_width(column, *width)
            .map_err(workbook_error)?;
    }
    worksheet.set_row_format(0, &header).map_err(workbook_error)?;

    let mut excel_row: u32 = 1;
    for row in rows {
        // Pictures: keep the files Excel can embed, then plan how many of them
        // fit below the 409-point row ceiling. The rest stays in the media
        // directory and is named in the cell instead of being clipped away.
        let images: Vec<(&RowSection, &RowMediaFile)> = row
            .sections
            .iter()
            .flat_map(|section| section.media.iter().map(move |file| (section, file)))
            .filter(|(_, file)| file.kind == MediaKind::Image)
            .collect();
        let mut embeddable: Vec<(&RowSection, &RowMediaFile, Image)> = Vec::new();
        for (section, file) in &images {
            if let Ok(image) = Image::new(&file.absolute) {
                embeddable.push((section, file, image));
            }
        }
        let sizes: Vec<(f64, f64)> = embeddable
            .iter()
            .map(|(_, _, image)| (image.width(), image.height()))
            .collect();

        let (media_text, layout) = plan_media_hint(row, &sizes);

        // The media hint sits above the conversation text and the pictures hang
        // between the two, so the row owns them where its own messages are.
        // The hint is added here, after the transcript was trimmed to the cell
        // limit, so trim once more.
        let spacer_lines = layout.text_spacer_lines(media_text.lines().count());
        let transcript =
            truncate_for_cell(&conversation_cell(row, &media_text, spacer_lines));

        // Everything the row owns stays in this one cell. Excel allows a single
        // hyperlink per cell, so a picture is opened by clicking its own
        // thumbnail, and the cell itself links to the directory that holds what a
        // picture cannot carry: the voices, the attachments and any picture that
        // did not fit.
        let embedded: Vec<&str> = embeddable
            .iter()
            .take(layout.shown)
            .map(|(_, file, _)| file.relative.as_str())
            .collect();
        let unlinked = row
            .sections
            .iter()
            .flat_map(|section| section.media.iter())
            .any(|file| {
                file.kind != MediaKind::Image || !embedded.contains(&file.relative.as_str())
            });
        let media_directory = if unlinked { row_media_directory(row) } else { None };

        let cells: Vec<String> = vec![
            row.name.clone(),
            row.summary.clone(),
            row.period.clone(),
            row.message_count.to_string(),
            transcript,
            row.highlights.clone(),
        ];
        for (column, value) in cells.iter().enumerate() {
            let column = column as u16;
            let style = if column as usize == HIGHLIGHT_COLUMN {
                &highlight
            } else {
                &body
            };
            if column == TRANSCRIPT_COLUMN && let Some(directory) = &media_directory {
                let tip = format!("打开 {directory}/：本行的语音、附件和未嵌入的图片都在这里");
                worksheet
                    .write_url_with_options(
                        excel_row,
                        column,
                        Url::new(format!("file://{directory}/")),
                        value.clone(),
                        tip,
                        Some(style),
                    )
                    .map_err(workbook_error)?;
                continue;
            }
            // Chat text can start with '=' or '+', so always go through the
            // string writer instead of the generic one.
            worksheet
                .write_string_with_format(excel_row, column, value.clone(), style)
                .map_err(workbook_error)?;
        }

        // A row with media gets an explicit height: the blank lines that hold the
        // pictures back would otherwise push the conversation out of a default
        // 15-point row. Rows without media keep Excel's own auto-fit.
        if !media_text.is_empty() {
            let mut y_offset = layout.text_height_pixels.round() as u32;
            for (section, file, image) in embeddable.iter().take(layout.shown) {
                let (fitted_width, fitted_height) = fit_thumbnail(image.width(), image.height());
                let image = image
                    .clone()
                    .set_scale_to_size(fitted_width, fitted_height, true)
                    .set_alt_text(format!("{} · {}", section.conversation, file.label))
                    .set_object_movement(ObjectMovement::MoveAndSizeWithCells);
                // Clicking the thumbnail opens the original file at full size.
                let linked = image
                    .clone()
                    .set_url(Url::new(format!("file://{}", file.relative)));
                let image = linked.unwrap_or(image);
                worksheet
                    .insert_image_with_offset(excel_row, TRANSCRIPT_COLUMN, &image, 0, y_offset)
                    .map_err(workbook_error)?;
                y_offset += (fitted_height.ceil() as u32).max(1);
            }
            worksheet
                .set_row_height(excel_row, layout.row_height_points)
                .map_err(workbook_error)?;
        }

        excel_row += 1;
    }

    Ok(excel_row)
}

fn workbook_error(error: rust_xlsxwriter::XlsxError) -> ReportError {
    ReportError::Workbook(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ReportMessage;
    use archive_domain::{MediaIntegrity, MediaRefV1, MessageDirection};
    use chrono::DateTime;

    fn text(body: &str) -> ReportMessage {
        ReportMessage {
            sent_at: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            sender_name: Some("张三".into()),
            sender_id: Some("peer".into()),
            direction: MessageDirection::Incoming,
            message_type: MessageType::Text,
            body_text: Some(body.into()),
            media: Vec::new(),
        }
    }

    fn row(name: &str) -> PersonMonthRow {
        PersonMonthRow {
            key: format!("2026-09|{name}"),
            name: name.into(),
            month: "2026-09".into(),
            period: "2026-09-01 ~ 2026-09-30".into(),
            message_count: 2,
            text_count: 2,
            image_count: 0,
            audio_count: 0,
            file_count: 0,
            voice_with_audio: 0,
            missing_media: 0,
            first_at: Some(DateTime::from_timestamp(1_700_000_000, 0).unwrap()),
            last_at: Some(DateTime::from_timestamp(1_700_000_060, 0).unwrap()),
            highlights: "高频词：上线(3)".into(),
            transcript: String::new(),
            summary: String::new(),
            sections: Vec::new(),
        }
    }

    #[test]
    fn person_markdown_carries_name_period_transcript_and_prompt() {
        let mut image = text("");
        image.message_type = MessageType::Image;
        image.media = vec![MediaRefV1 {
            content_hash: Some("a".repeat(64)),
            original_name: Some("图.png".into()),
            mime_type: None,
            size_bytes: Some(3),
            source_locator: "sha256:aa".into(),
            integrity: MediaIntegrity::Verified,
            missing_reason: None,
        }];
        let mut paths = MediaPaths::new();
        paths.insert("a".repeat(64), "media/项目群/01.png".into());
        let messages = vec![text("上线时间确认了吗"), image];

        let markdown = render_person_markdown(
            "李四",
            "2026-09-01 ~ 2026-09-30",
            &messages,
            &paths,
            ReportOptions::default(),
        );
        assert!(markdown.contains("# 沟通记录：李四（2026-09-01 ~ 2026-09-30）"), "got {markdown}");
        assert!(markdown.contains("- 姓名：李四"), "got {markdown}");
        assert!(markdown.contains("- 时间：2026-09-01 ~ 2026-09-30"), "got {markdown}");
        assert!(markdown.contains("上线时间确认了吗"), "got {markdown}");
        assert!(markdown.contains("![图.png](media/项目群/01.png)"), "got {markdown}");
        // The note is the conversation and nothing else: the instructions are the
        // operator's, and they travel as the request's system message instead.
        assert!(!markdown.contains("请按以下结构总结"), "got {markdown}");
        assert!(!markdown.contains("核心议题"), "got {markdown}");
    }

    #[test]
    fn workbook_writes_one_row_per_person_month_with_the_summary_left_blank() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(WORKBOOK_NAME);
        write_workbook(&path, &[row("张三"), row("李四")]).unwrap();

        assert!(path.is_file());
        assert!(!path.with_extension("xlsx.partial").exists());
        // The workbook is a zip container, so its magic bytes prove real output.
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..2], b"PK");
    }

    #[test]
    fn long_transcripts_are_trimmed_to_the_excel_cell_limit() {
        let short = "x".repeat(100);
        assert_eq!(truncate_for_cell(&short), short);

        let long = "行内容\n".repeat(10_000);
        let trimmed = truncate_for_cell(&long);
        assert!(trimmed.chars().count() <= EXCEL_CELL_LIMIT);
        assert!(trimmed.contains("已截断"));
        assert!(trimmed.contains("沟通记录目录"));
    }

    #[test]
    fn transcript_marks_media_and_falls_back_to_the_message_type() {
        let mut image = text("");
        image.message_type = MessageType::Image;
        image.media = vec![MediaRefV1 {
            content_hash: Some("a".repeat(64)),
            original_name: Some("图.png".into()),
            mime_type: None,
            size_bytes: None,
            source_locator: "sha256:aa".into(),
            integrity: MediaIntegrity::Verified,
            missing_reason: None,
        }];
        let mut paths = MediaPaths::new();
        paths.insert("a".repeat(64), "media/会话/01.png".into());
        let messages = vec![text("正文"), image];

        let transcript = render_transcript(&messages, &paths, ReportOptions::default());
        assert!(transcript.contains("正文"), "got {transcript}");
        assert!(transcript.contains("[图.png -> media/会话/01.png]"), "got {transcript}");
        // No body text on a media message: fall back to the localized type label.
        assert!(transcript.contains("[图片]"), "got {transcript}");
    }

    /// A real 1x1 RGBA PNG. Synthetic header bytes would fail to parse and the
    /// embed path would never run.
    const TINY_PNG: [u8; 70] = [137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 218, 99, 252, 207, 192, 80, 15, 0, 4, 133, 1, 128, 132, 169, 140, 33, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130];

    fn media_file(relative: &str, kind: MediaKind) -> RowMediaFile {
        RowMediaFile {
            relative: relative.to_owned(),
            absolute: PathBuf::from(relative),
            kind,
            label: relative.rsplit('/').next().unwrap().to_owned(),
        }
    }

    /// The user asked for the summary right next to the name, and for the media
    /// to live inside 聊天记录 instead of a column of its own.
    #[test]
    fn the_summary_column_sits_next_to_the_name_and_media_lives_in_the_transcript() {
        let titles: Vec<&str> = COLUMNS.iter().map(|(title, _)| *title).collect();
        assert_eq!(
            titles,
            vec!["姓名", "总结记录", "时间", "消息数", "聊天记录", "要点摘录"]
        );
        assert_eq!(COLUMNS[HIGHLIGHT_COLUMN].0, "要点摘录");
        assert_eq!(COLUMNS[TRANSCRIPT_COLUMN as usize].0, "聊天记录");
    }

    #[test]
    fn conversation_cell_puts_the_media_hint_above_the_transcript() {
        let mut with_media = row("张三");
        with_media.transcript = "10:00 张三：你好".into();

        assert_eq!(conversation_cell(&with_media, "", 0), "10:00 张三：你好");

        let cell = conversation_cell(&with_media, "图片 1 张（缩略图见下，点缩略图看原图）", 2);
        assert!(cell.starts_with("图片 1 张"), "got {cell}");
        assert!(cell.ends_with("10:00 张三：你好"), "got {cell}");

        // Pictures are drawn from the top of the cell, so the conversation is
        // pushed down by the blank lines that cover them.
        let spaced = conversation_cell(&with_media, "图片 1 张", 9);
        assert!(spaced.contains("图片 1 张\n\n\n\n\n\n\n\n\n10:00 张三：你好"), "got {spaced}");
    }

    #[test]
    fn media_cell_text_reports_counts_and_where_to_click() {
        let mut with_media = row("张三");
        with_media.sections = vec![RowSection {
            conversation: "张三".into(),
            note: PathBuf::from("note.md"),
            media: vec![
                media_file("media/张三/01.png", MediaKind::Image),
                media_file("media/张三/01.silk", MediaKind::Voice),
                media_file("media/张三/02.pdf", MediaKind::File),
            ],
        }];
        // With the picture embedded the cell points at the thumbnail, and it
        // promises the folder click only when the cell really carries that link.
        let text = media_cell_text(&with_media, 1);
        assert!(text.contains("图片 1 张（缩略图见下，点缩略图看原图）"), "got {text}");
        assert!(text.contains("语音 1 条"), "got {text}");
        assert!(text.contains("附件 1 个"), "got {text}");
        assert!(text.contains("原文件在 media/张三/，点本单元格打开文件夹"), "got {text}");

        // A picture that stayed on disk must not promise a thumbnail.
        let kept = media_cell_text(&with_media, 0);
        assert!(kept.contains("图片 1 张（原图在媒体目录）"), "got {kept}");
        assert!(!kept.contains("缩略图"), "got {kept}");

        // A row whose media lives in two folders cannot be opened by the one
        // hyperlink a cell allows, so it is described in words instead.
        let mut spread = with_media.clone();
        spread.sections[0]
            .media
            .push(media_file("media/李四/03.pdf", MediaKind::File));
        assert_eq!(row_media_directory(&spread), None);
        let many = media_cell_text(&spread, 0);
        assert!(many.contains("原文件在 media/张三、media/李四 下"), "got {many}");
        assert!(!many.contains("点本单元格"), "got {many}");

        assert_eq!(media_cell_text(&row("张三"), 0), "");
    }

    /// A thumbnail is anchored from the top of the cell, so the conversation
    /// text must start below it: the spacer has to cover the pictures exactly,
    /// and a one-line hint must never swallow the first message.
    #[test]
    fn the_conversation_starts_below_the_pictures() {
        // 1 line of hint (20px) + one 168px picture = 188px -> line 10.
        let one_picture = plan_media_layout(1, &[(400.0, 400.0)]);
        assert_eq!(one_picture.shown, 1);
        assert_eq!(one_picture.shown_pixels, 168.0);
        assert_eq!(one_picture.transcript_start_line(), 10);

        let mut person = row("张三");
        person.transcript = "10:00 张三：你好".into();
        let cell = conversation_cell(&person, "图片 1 张", one_picture.text_spacer_lines(1));
        // Line 0 is the hint, lines 1..=9 are covered by the picture, so the
        // first message lands on line 10 and nothing is hidden behind it.
        assert_eq!(cell.lines().nth(10), Some("10:00 张三：你好"), "got {cell}");
        assert!(one_picture.row_height_points > one_picture.shown_pixels * 0.75);

        // No media: the conversation keeps the whole cell and starts at line 0.
        let none = plan_media_layout(0, &[]);
        assert_eq!(none.shown, 0);
        assert_eq!(none.shown_pixels, 0.0);
        assert_eq!(none.transcript_start_line(), 0);
        assert_eq!(conversation_cell(&person, "", none.text_spacer_lines(0)), person.transcript);

        // A voice-only row has no picture to clear, so one blank line is enough
        // to keep the conversation off the hint.
        let voice_only = plan_media_layout(1, &[]);
        assert_eq!(voice_only.text_spacer_lines(1), 1);
        let cell = conversation_cell(&person, "语音 1 条", voice_only.text_spacer_lines(1));
        assert_eq!(cell.lines().nth(1), Some("10:00 张三：你好"), "got {cell}");
    }

    /// Excel caps a row at 409 points; pictures that do not fit must be left
    /// out of the layout rather than silently clipped into the next row.
    #[test]
    fn thumbnails_stop_before_the_excel_row_limit() {
        let empty = plan_media_layout(2, &[]);
        assert_eq!(empty.shown, 0);
        assert!(empty.row_height_points <= MAX_ROW_HEIGHT_POINTS);

        // 168px wide thumbs, square: ~168px each, one point per 1.333 pixels.
        let sizes = vec![(400.0, 400.0); 8];
        let layout = plan_media_layout(3, &sizes);
        assert!(layout.shown >= 1, "at least one picture must fit");
        assert!(layout.shown < sizes.len(), "the limit must bite");
        assert!(layout.row_height_points <= MAX_ROW_HEIGHT_POINTS);

        // Small pictures all fit.
        let small = vec![(100.0, 60.0); 3];
        let layout = plan_media_layout(2, &small);
        assert_eq!(layout.shown, small.len());
        assert!(layout.row_height_points <= MAX_ROW_HEIGHT_POINTS);
    }

    /// Media never gets a row of its own: the pictures are anchored inside the
    /// 聊天记录 cell and the voices and attachments are named in that same cell,
    /// so a row that carries several files still occupies exactly one sheet row.
    #[test]
    fn media_never_gets_a_row_of_its_own() {
        let mut workbook = Workbook::new();
        let worksheet = workbook.add_worksheet();

        let mut with_media = row("张三");
        with_media.transcript = "10:00 张三：你好".into();
        with_media.sections = vec![RowSection {
            conversation: "张三".into(),
            note: PathBuf::from("note.md"),
            media: vec![
                media_file("media/张三/01.png", MediaKind::Image),
                media_file("media/张三/01.silk", MediaKind::Voice),
                media_file("media/张三/02.pdf", MediaKind::File),
            ],
        }];

        let written = write_sheet(worksheet, &[with_media.clone(), row("李四")]).unwrap();
        assert_eq!(
            written, 3,
            "one header row plus one row per person-month, never one per medium"
        );

        // The hint names all three kinds and the one folder the cell links to,
        // and the conversation follows below the pictures inside the same cell.
        let (media_text, layout) = plan_media_hint(&with_media, &[(400.0, 300.0)]);
        assert_eq!(layout.shown, 1, "the only picture fits below the hint");
        let spacer = layout.text_spacer_lines(media_text.lines().count());
        let cell = conversation_cell(&with_media, &media_text, spacer);
        assert!(
            cell.starts_with(
                "图片 1 张（缩略图见下，点缩略图看原图）\n语音 1 条、附件 1 个（原文件在 media/张三/，点本单元格打开文件夹）"
            ),
            "got {cell}"
        );
        assert_eq!(
            cell.lines().nth(layout.transcript_start_line()),
            Some("10:00 张三：你好"),
            "the conversation must start below the pictures, got {cell}"
        );
        assert!(layout.row_height_points > TEXT_LINE_POINTS, "the row must grow");
    }

    /// A row with a real picture and a real voice file must still write, with
    /// the picture embedded in the cell and the voice named in that same cell.
    #[test]
    fn a_real_picture_is_embedded_in_the_conversation_cell() {
        let directory = tempfile::tempdir().unwrap();
        let media_dir = directory.path().join("media").join("张三");
        std::fs::create_dir_all(&media_dir).unwrap();
        // A genuine PNG: only a picture Excel can parse gets embedded.
        std::fs::write(media_dir.join("01.png"), TINY_PNG).unwrap();
        std::fs::write(media_dir.join("01.silk"), b"#!SILK_V3abc").unwrap();

        let mut with_media = row("张三");
        with_media.sections = vec![RowSection {
            conversation: "张三".into(),
            note: PathBuf::from("note.md"),
            media: vec![
                RowMediaFile {
                    relative: "media/张三/01.png".into(),
                    absolute: media_dir.join("01.png"),
                    kind: MediaKind::Image,
                    label: "01.png".into(),
                },
                RowMediaFile {
                    relative: "media/张三/01.silk".into(),
                    absolute: media_dir.join("01.silk"),
                    kind: MediaKind::Voice,
                    label: "01.silk".into(),
                },
            ],
        }];
        let path = directory.path().join(WORKBOOK_NAME);
        write_workbook(&path, &[with_media.clone()]).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..2], b"PK");

        // The picture really parses, so it is embedded rather than merely named.
        let image = Image::new(media_dir.join("01.png")).unwrap();
        let sizes = [(image.width(), image.height())];
        let (media_text, layout) = plan_media_hint(&with_media, &sizes);
        assert_eq!(layout.shown, 1);
        assert!(media_text.contains("缩略图见下"), "got {media_text}");
        assert!(media_text.contains("语音 1 条"), "got {media_text}");
    }

    /// A canonical Huffman table, as (code, bit length, symbol) triples.
    type Huffman = Vec<(u16, u32, u16)>;

    /// Reads a deflate stream bit by bit, least significant bit first, which is
    /// the order the format packs its fields in.
    struct BitReader<'a> {
        bytes: &'a [u8],
        bit: usize,
    }

    impl BitReader<'_> {
        fn bits(&mut self, count: u32) -> u32 {
            let mut value = 0u32;
            for index in 0..count {
                let byte = self.bytes[self.bit / 8];
                let bit = (byte >> (self.bit % 8)) & 1;
                value |= u32::from(bit) << index;
                self.bit += 1;
            }
            value
        }

        fn align(&mut self) {
            self.bit = self.bit.div_ceil(8) * 8;
        }

        fn byte(&mut self) -> u8 {
            self.align();
            let byte = self.bytes[self.bit / 8];
            self.bit += 8;
            byte
        }

        fn u16(&mut self) -> u16 {
            u16::from_le_bytes([self.byte(), self.byte()])
        }
    }

    /// Canonical Huffman codes, shortest length first, as deflate defines them.
    fn huffman(lengths: &[u32]) -> Huffman {
        let max_bits = lengths.iter().copied().max().unwrap_or(0);
        let mut counts = vec![0u16; max_bits as usize + 2];
        for length in lengths {
            if *length > 0 {
                counts[*length as usize] += 1;
            }
        }
        let mut next = vec![0u16; max_bits as usize + 2];
        let mut code = 0u16;
        for bits in 1..=max_bits as usize {
            code = (code + counts[bits - 1]) << 1;
            next[bits] = code;
        }
        let mut table = Huffman::new();
        for (symbol, length) in lengths.iter().enumerate() {
            if *length == 0 {
                continue;
            }
            let length = *length as usize;
            table.push((next[length], length as u32, symbol as u16));
            next[length] += 1;
        }
        table
    }

    fn next_symbol(reader: &mut BitReader, table: &Huffman) -> u16 {
        let mut code = 0u16;
        for length in 1..=15u32 {
            code = (code << 1) | reader.bits(1) as u16;
            if let Some((_, _, symbol)) = table
                .iter()
                .find(|(candidate, bits, _)| *bits == length && *candidate == code)
            {
                return *symbol;
            }
        }
        panic!("no Huffman code matched the stream");
    }

    /// The literal and distance tables of a fixed-Huffman block.
    fn fixed_tables() -> (Huffman, Huffman) {
        let mut literals = vec![8u32; 144];
        literals.extend(std::iter::repeat_n(9u32, 112));
        literals.extend(std::iter::repeat_n(7u32, 24));
        literals.extend(std::iter::repeat_n(8u32, 8));
        (huffman(&literals), huffman(&[5u32; 32]))
    }

    /// The literal and distance tables a dynamic block carries in its header.
    fn dynamic_tables(reader: &mut BitReader) -> (Huffman, Huffman) {
        const CODE_LENGTH_ORDER: [usize; 19] =
            [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];
        let literals = reader.bits(5) as usize + 257;
        let distances = reader.bits(5) as usize + 1;
        let code_lengths = reader.bits(4) as usize + 4;

        let mut lengths = [0u32; 19];
        for symbol in CODE_LENGTH_ORDER.iter().take(code_lengths) {
            lengths[*symbol] = reader.bits(3);
        }
        let table = huffman(&lengths);

        let mut all: Vec<u32> = Vec::with_capacity(literals + distances);
        while all.len() < literals + distances {
            match next_symbol(reader, &table) {
                16 => {
                    let last = *all.last().expect("a repeat must follow a length");
                    let repeat = reader.bits(2) + 3;
                    all.extend(std::iter::repeat_n(last, repeat as usize));
                }
                17 => {
                    let repeat = reader.bits(3) + 3;
                    all.extend(std::iter::repeat_n(0, repeat as usize));
                }
                18 => {
                    let repeat = reader.bits(7) + 11;
                    all.extend(std::iter::repeat_n(0, repeat as usize));
                }
                length => all.push(u32::from(length)),
            }
        }
        all.truncate(literals + distances);
        (
            huffman(&all[..literals]),
            huffman(&all[literals..]),
        )
    }

    fn inflate_block(
        reader: &mut BitReader,
        literals: &Huffman,
        distances: &Huffman,
        out: &mut Vec<u8>,
    ) {
        const LENGTH_BASE: [usize; 29] = [
            3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99,
            115, 131, 163, 195, 227, 258,
        ];
        const LENGTH_EXTRA: [u32; 29] = [
            0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
        ];
        const DISTANCE_BASE: [usize; 30] = [
            1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025,
            1537, 2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
        ];
        const DISTANCE_EXTRA: [u32; 30] = [
            0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12,
            12, 13, 13,
        ];

        loop {
            match next_symbol(reader, literals) {
                literal @ 0..=255 => out.push(literal as u8),
                256 => return,
                code @ 257..=285 => {
                    let index = code as usize - 257;
                    let length = LENGTH_BASE[index] + reader.bits(LENGTH_EXTRA[index]) as usize;
                    let index = next_symbol(reader, distances) as usize;
                    let distance = DISTANCE_BASE[index] + reader.bits(DISTANCE_EXTRA[index]) as usize;
                    assert!(distance > 0 && distance <= out.len(), "back reference out of range");
                    let start = out.len() - distance;
                    let mut offset = 0;
                    while offset < length {
                        let byte = out[start + offset];
                        out.push(byte);
                        offset += 1;
                    }
                }
                other => panic!("reserved deflate length code {other}"),
            }
        }
    }

    /// A minimal inflate for the raw deflate streams rust_xlsxwriter writes: the
    /// sheet and drawing parts are Deflated, and this crate deliberately has no
    /// compression dependency to read them back with.
    fn inflate(raw: &[u8]) -> Vec<u8> {
        let mut reader = BitReader { bytes: raw, bit: 0 };
        let mut out = Vec::new();
        loop {
            let last_block = reader.bits(1) == 1;
            match reader.bits(2) {
                0 => {
                    reader.align();
                    let length = reader.u16() as usize;
                    let complement = reader.u16();
                    assert_eq!(length as u16, !complement, "stored block length");
                    for _ in 0..length {
                        let byte = reader.byte();
                        out.push(byte);
                    }
                }
                1 => {
                    let (literals, distances) = fixed_tables();
                    inflate_block(&mut reader, &literals, &distances, &mut out);
                }
                2 => {
                    let (literals, distances) = dynamic_tables(&mut reader);
                    inflate_block(&mut reader, &literals, &distances, &mut out);
                }
                _ => panic!("reserved deflate block type"),
            }
            if last_block {
                return out;
            }
        }
    }

    /// One member of the saved workbook, straight out of its zip container.
    /// rust_xlsxwriter exposes no row count, so the only way to prove what the
    /// sheet really holds is to read the file it wrote.
    fn zip_member(archive: &[u8], wanted: &str) -> Vec<u8> {
        let eocd = (0..archive.len().saturating_sub(21))
            .rev()
            .find(|start| archive[*start..*start + 4] == [0x50, 0x4b, 0x05, 0x06])
            .expect("the workbook must end with a zip end-of-central-directory record");
        let entries = u16::from_le_bytes([archive[eocd + 10], archive[eocd + 11]]) as usize;
        let mut cursor = u32::from_le_bytes([
            archive[eocd + 16],
            archive[eocd + 17],
            archive[eocd + 18],
            archive[eocd + 19],
        ]) as usize;

        for _ in 0..entries {
            assert_eq!(
                archive[cursor..cursor + 4],
                [0x50, 0x4b, 0x01, 0x02],
                "central directory header"
            );
            let method = u16::from_le_bytes([archive[cursor + 10], archive[cursor + 11]]);
            let compressed = u32::from_le_bytes([
                archive[cursor + 20],
                archive[cursor + 21],
                archive[cursor + 22],
                archive[cursor + 23],
            ]) as usize;
            let name_length =
                u16::from_le_bytes([archive[cursor + 28], archive[cursor + 29]]) as usize;
            let extra_length =
                u16::from_le_bytes([archive[cursor + 30], archive[cursor + 31]]) as usize;
            let comment_length =
                u16::from_le_bytes([archive[cursor + 32], archive[cursor + 33]]) as usize;
            let local = u32::from_le_bytes([
                archive[cursor + 42],
                archive[cursor + 43],
                archive[cursor + 44],
                archive[cursor + 45],
            ]) as usize;
            let name =
                String::from_utf8_lossy(&archive[cursor + 46..cursor + 46 + name_length]).into_owned();

            if name == wanted {
                // The local header repeats the name and adds its own extra field,
                // which is not the same length as the one in the directory.
                let local_name =
                    u16::from_le_bytes([archive[local + 26], archive[local + 27]]) as usize;
                let local_extra =
                    u16::from_le_bytes([archive[local + 28], archive[local + 29]]) as usize;
                let body = local + 30 + local_name + local_extra;
                let raw = &archive[body..body + compressed];
                return match method {
                    0 => raw.to_vec(),
                    8 => inflate(raw),
                    other => panic!("unexpected compression method {other}"),
                };
            }
            cursor += 46 + name_length + extra_length + comment_length;
        }
        panic!("{wanted} is missing from the workbook");
    }

    /// The user's complaint was that media got rows of its own below the
    /// conversation. Excel cannot be asked for its row count, so the saved sheet
    /// is read back: one header row plus exactly one row per person-month,
    /// however much media those rows carry.
    #[test]
    fn the_saved_sheet_has_one_row_per_person_month_and_no_media_rows() {
        let directory = tempfile::tempdir().unwrap();
        let media_dir = directory.path().join("media").join("张三");
        std::fs::create_dir_all(&media_dir).unwrap();
        std::fs::write(media_dir.join("01.png"), TINY_PNG).unwrap();
        std::fs::write(media_dir.join("01.silk"), b"#!SILK_V3abc").unwrap();
        std::fs::write(media_dir.join("02.pdf"), b"%PDF-1.4\n").unwrap();

        let mut with_media = row("张三");
        with_media.transcript = "10:00 张三：你好".into();
        with_media.sections = vec![RowSection {
            conversation: "张三".into(),
            note: PathBuf::from("note.md"),
            media: vec![
                RowMediaFile {
                    relative: "media/张三/01.png".into(),
                    absolute: media_dir.join("01.png"),
                    kind: MediaKind::Image,
                    label: "01.png".into(),
                },
                RowMediaFile {
                    relative: "media/张三/01.silk".into(),
                    absolute: media_dir.join("01.silk"),
                    kind: MediaKind::Voice,
                    label: "01.silk".into(),
                },
                RowMediaFile {
                    relative: "media/张三/02.pdf".into(),
                    absolute: media_dir.join("02.pdf"),
                    kind: MediaKind::File,
                    label: "02.pdf".into(),
                },
            ],
        }];

        let path = directory.path().join(WORKBOOK_NAME);
        write_workbook(&path, &[with_media, row("李四")]).unwrap();
        let archive = std::fs::read(&path).unwrap();

        let sheet = String::from_utf8(zip_member(&archive, "xl/worksheets/sheet1.xml")).unwrap();
        assert_eq!(
            sheet.matches("<row r=\"").count(),
            3,
            "one header row and two person-month rows, got {sheet}"
        );
        assert!(sheet.contains("<row r=\"1\""), "got {sheet}");
        assert!(sheet.contains("<row r=\"3\""), "got {sheet}");
        assert!(
            !sheet.contains("<row r=\"4\""),
            "nothing may follow the last person-month: {sheet}"
        );

        // The cell carries the one hyperlink Excel allows: the folder holding
        // the files a thumbnail cannot show.
        assert_eq!(sheet.matches("<hyperlink ").count(), 1, "got {sheet}");
        assert!(sheet.contains("media/张三/"), "got {sheet}");

        // The picture is anchored inside the row's own cell, not in a row below.
        let drawing = zip_member(&archive, "xl/drawings/drawing1.xml");
        let drawing = String::from_utf8(drawing).unwrap();
        assert_eq!(drawing.matches("<xdr:from>").count(), 1, "got {drawing}");
        assert!(drawing.contains("<xdr:row>1</xdr:row>"), "got {drawing}");
        assert!(drawing.contains("<xdr:col>4</xdr:col>"), "got {drawing}");
    }

    #[test]
    fn cells_starting_with_formula_characters_do_not_break_writing() {
        let directory = tempfile::tempdir().unwrap();
        let mut hostile = row("=cmd()");
        hostile.highlights = "+1234".into();
        hostile.transcript = "@handle".into();
        hostile.summary = "-1+1".into();
        let path = directory.path().join(WORKBOOK_NAME);
        write_workbook(&path, &[hostile]).unwrap();
        assert!(path.is_file());
    }
}
