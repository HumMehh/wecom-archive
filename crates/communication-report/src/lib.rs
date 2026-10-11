//! Communication report generation.
//!
//! Turns archived conversations into a per-conversation workbook plus one
//! Markdown transcript per conversation. The workbook keeps a summary column
//! empty so a caller can either fill it by hand or fill it from an external
//! summarizer without regenerating anything else.
//!
//! The crate is deliberately free of Windows APIs and network access: it only
//! reads media that the archive already stored, and every file it writes is
//! created under a single output directory through a `.partial` rename.

mod highlight;
mod media;
mod render;

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use archive_domain::{MediaRefV1, MessageType};
use chrono::{DateTime, Datelike, Duration, Utc};

pub use highlight::{extract_highlights, Highlights};
pub use render::{MediaKind, PersonMonthRow, RowMediaFile, RowSection, WORKBOOK_NAME};
pub use media::MediaPaths;

const NOTES_DIR_NAME: &str = "沟通记录";
const MEDIA_DIR_NAME: &str = "media";

#[derive(Debug, thiserror::Error)]
pub enum ReportError {
    #[error("输出目录不可写：{0}")]
    Io(#[from] io::Error),
    #[error("无法创建工作簿：{0}")]
    Workbook(String),
    #[error("输出目录已存在同名文件")]
    TargetExists,
}

/// Which local month to keep. `None` keeps every message.
pub type LocalMonth = (u32, u32);

#[derive(Debug, Clone, Copy)]
pub struct ReportOptions {
    /// Local month filter, evaluated against `utc_offset_minutes`.
    pub month: Option<LocalMonth>,
    /// Local offset from UTC in minutes. China Standard Time is 480.
    pub utc_offset_minutes: i32,
    /// Copy resolved media out of the media store into the report directory.
    pub include_media: bool,
    /// Drop group conversations and keep direct (one-to-one) ones only.
    pub only_direct: bool,
}

impl Default for ReportOptions {
    fn default() -> Self {
        Self {
            month: None,
            utc_offset_minutes: 480,
            include_media: true,
            only_direct: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ReportMessage {
    pub sent_at: DateTime<Utc>,
    pub sender_name: Option<String>,
    pub sender_id: Option<String>,
    /// Needed to tell the archive owner apart from the person on the other side.
    pub direction: archive_domain::MessageDirection,
    pub message_type: MessageType,
    pub body_text: Option<String>,
    pub media: Vec<MediaRefV1>,
}

impl ReportMessage {
    /// True when the archive owner sent this message, which is what identifies
    /// "the other person" in a direct conversation.
    pub fn is_from_owner(&self) -> bool {
        self.direction == archive_domain::MessageDirection::Outgoing
    }

    fn sender(&self) -> &str {
        self.sender_name
            .as_deref()
            .filter(|name| !name.trim().is_empty())
            .or(self.sender_id.as_deref())
            .unwrap_or("未知发送人")
    }
}

#[derive(Debug, Clone)]
pub struct ReportConversation {
    pub conversation_id: String,
    pub name: String,
    pub conversation_type: Option<String>,
    pub messages: Vec<ReportMessage>,
}

#[derive(Debug, Clone)]
pub struct GeneratedReport {
    pub directory: PathBuf,
    pub workbook_path: PathBuf,
    /// Markdown transcript per person-month, in the same order as `rows`.
    pub notes: Vec<PathBuf>,
    pub rows: Vec<PersonMonthRow>,
}

/// Shift a UTC instant into the report's local wall clock. Everything downstream
/// reads UTC fields off this value, so a fixed offset keeps the code free of
/// time-zone database lookups.
fn to_local(at: DateTime<Utc>, utc_offset_minutes: i32) -> DateTime<Utc> {
    at + Duration::minutes(i64::from(utc_offset_minutes))
}

fn local_month_of(at: DateTime<Utc>, utc_offset_minutes: i32) -> LocalMonth {
    let local = to_local(at, utc_offset_minutes);
    (local.format("%Y").to_string().parse().unwrap_or(1970), local.month())
}

/// A conversation counts as a group unless the archive explicitly labels it
/// direct. An absent label is treated as direct so an unknown conversation is
/// never silently dropped.
fn is_group(conversation_type: Option<&str>) -> bool {
    conversation_type.is_some_and(|value| !value.eq_ignore_ascii_case("direct"))
}

/// The person a report row is about: the side of the conversation that is not
/// the archive owner. Outgoing messages identify the owner, so the first other
/// sender is the counterpart and their WeCom display name — the remark, when one
/// was resolved at collection time — becomes the row's 姓名.
///
/// When no direction was recorded the two sides cannot be told apart, so the
/// conversation's own stored name is used instead of guessing.
fn counterpart_name(conversation: &ReportConversation) -> String {
    // 群聊挂群名：按「第一个非本人发送人」归档会把整段群聊记到某位群成员
    // 名下，他那行的原文里就掺进了别人的话，总结自然带着除他以外的人。
    if is_group(conversation.conversation_type.as_deref())
        && !conversation.name.trim().is_empty()
    {
        return conversation.name.clone();
    }
    let name_of = |message: &ReportMessage| {
        message
            .sender_name
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned)
            .or_else(|| message.sender_id.clone())
    };

    let owner_recorded = conversation
        .messages
        .iter()
        .any(ReportMessage::is_from_owner);
    if owner_recorded {
        if let Some(message) = conversation.messages.iter().find(|message| !message.is_from_owner())
            && let Some(name) = name_of(message)
        {
            return name;
        }
    } else {
        // Nothing outgoing in this slice, so the other side opened the chat.
        // When exactly one person is involved that person is the counterpart;
        // with several, the two sides cannot be told apart from direction alone.
        let mut senders: Vec<String> = Vec::new();
        for message in &conversation.messages {
            if message.direction == archive_domain::MessageDirection::System {
                continue;
            }
            if let Some(name) = name_of(message)
                && !senders.contains(&name)
            {
                senders.push(name);
            }
        }
        if senders.len() == 1 {
            return senders.remove(0);
        }
    }
    conversation.name.clone()
}

/// `2026-09-01 ~ 2026-09-30`, honouring the real length of the month.
fn month_period(month: LocalMonth) -> String {
    let (year, month) = (i32::try_from(month.0).unwrap_or(1970), month.1);
    let Some(first) = chrono::NaiveDate::from_ymd_opt(year, month, 1) else {
        return String::new();
    };
    let next = if month == 12 {
        chrono::NaiveDate::from_ymd_opt(year + 1, 1, 1)
    } else {
        chrono::NaiveDate::from_ymd_opt(year, month + 1, 1)
    };
    let last = next.and_then(|next| next.pred_opt()).unwrap_or(first);
    format!("{first} ~ {last}")
}

fn in_scope(messages: &[ReportMessage], options: ReportOptions) -> Vec<ReportMessage> {
    messages
        .iter()
        .filter(|message| match options.month {
            Some(month) => local_month_of(message.sent_at, options.utc_offset_minutes) == month,
            None => true,
        })
        .cloned()
        .collect()
}

/// Write the report into `directory`, which must already exist.
///
/// The workbook is written even when the archive holds nothing in scope, so a
/// caller always has a file to open and explain.
pub fn generate(
    directory: &Path,
    conversations: &[ReportConversation],
    media_root: Option<&Path>,
    options: ReportOptions,
) -> Result<GeneratedReport, ReportError> {
    let notes_dir = directory.join(NOTES_DIR_NAME);
    let media_dir = directory.join(MEDIA_DIR_NAME);
    std::fs::create_dir_all(&notes_dir)?;

    let mut prepared = Vec::new();
    let mut used_note_names: BTreeMap<String, usize> = BTreeMap::new();

    for conversation in conversations {
        // A conversation counts as a group unless the archive says it is direct;
        // an unknown type is kept so a missing label cannot silently drop data.
        if options.only_direct && is_group(conversation.conversation_type.as_deref()) {
            continue;
        }
        let messages = in_scope(&conversation.messages, options);
        if messages.is_empty() {
            continue;
        }
        let conversation_media_dir = media_dir.join(sanitize_component(&conversation.name));
        let resolved = if options.include_media {
            media::export_media(
                &conversation_media_dir,
                &sanitize_component(&conversation.name),
                media_root,
                &messages,
            )?
        } else {
            BTreeMap::new()
        };
        prepared.push((conversation, messages, resolved));
    }

    // Newest conversation first, mirroring how the workbench lists archives.
    prepared.sort_by(|left, right| {
        right
            .1
            .first()
            .map(|message| message.sent_at)
            .cmp(&left.1.first().map(|message| message.sent_at))
            .then_with(|| left.0.conversation_id.cmp(&right.0.conversation_id))
    });

    // One row per person per calendar month: somebody who chatted with the
    // archive owner across several conversations in September becomes a single
    // September row. Inside a row every conversation keeps its own section —
    // its own messages, its own transcript file and its own media — so the
    // summariser can be asked about that one conversation alone.
    struct Section {
        conversation: String,
        messages: Vec<ReportMessage>,
        media_paths: MediaPaths,
    }

    struct Bucket {
        name: String,
        month: LocalMonth,
        sections: Vec<Section>,
    }

    let mut buckets: Vec<Bucket> = Vec::new();
    let mut bucket_index: BTreeMap<(String, LocalMonth), usize> = BTreeMap::new();
    for (conversation, messages, media_paths) in &prepared {
        let person = counterpart_name(conversation);
        // A conversation can span two months. Split it per month first: the
        // messages inside a slice stay contiguous, which is what lets one slice
        // become one request later on.
        let mut by_month: BTreeMap<LocalMonth, Vec<ReportMessage>> = BTreeMap::new();
        for message in messages {
            let month = local_month_of(message.sent_at, options.utc_offset_minutes);
            by_month.entry(month).or_default().push(message.clone());
        }
        for (month, slice) in by_month {
            let slot = *bucket_index
                .entry((person.clone(), month))
                .or_insert_with(|| {
                    buckets.push(Bucket {
                        name: person.clone(),
                        month,
                        sections: Vec::new(),
                    });
                    buckets.len() - 1
                });
            // Only the media this slice actually references travels with it, so
            // a request never carries a picture from another conversation.
            let mut section_media = MediaPaths::new();
            for message in &slice {
                for item in &message.media {
                    if let Some(hash) = item.content_hash.as_deref()
                        && let Some(relative) = media_paths.get(hash)
                    {
                        section_media
                            .entry(hash.to_owned())
                            .or_insert_with(|| relative.clone());
                    }
                }
            }
            buckets[slot].sections.push(Section {
                conversation: conversation.name.clone(),
                messages: slice,
                media_paths: section_media,
            });
        }
    }
    buckets.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.month.cmp(&right.month))
    });

    let mut rows = Vec::with_capacity(buckets.len());
    let mut notes = Vec::with_capacity(buckets.len());
    for bucket in &buckets {
        let month_label = format!("{:04}-{:02}", bucket.month.0, bucket.month.1);
        let period = month_period(bucket.month);
        // The row-level note still shows the whole month together — it is what
        // the workbench lists for a person. Summaries use the per-conversation
        // notes written below instead.
        let messages: Vec<ReportMessage> = bucket
            .sections
            .iter()
            .flat_map(|section| section.messages.iter().cloned())
            .collect();
        let mut media_paths = MediaPaths::new();
        for section in &bucket.sections {
            for (hash, relative) in &section.media_paths {
                media_paths
                    .entry(hash.clone())
                    .or_insert_with(|| relative.clone());
            }
        }

        let base_name = unique_note_name(
            &format!("{}_{month_label}", bucket.name),
            &mut used_note_names,
        );
        let notes_path = notes_dir.join(format!("{base_name}.md"));
        std::fs::write(
            format!("{}.partial", notes_path.display()),
            render::render_person_markdown(&bucket.name, &period, &messages, &media_paths, options),
        )?;
        rename_partial(&notes_path)?;

        // One file per conversation. Each request reads exactly one of them, so
        // a conversation can never inherit wording — or pictures — from another.
        let mut sections = Vec::with_capacity(bucket.sections.len());
        for (index, section) in bucket.sections.iter().enumerate() {
            let section_name = unique_note_name(
                &format!("{base_name}__{:02}_{}", index + 1, section.conversation),
                &mut used_note_names,
            );
            let section_path = notes_dir.join(format!("{section_name}.md"));
            std::fs::write(
                format!("{}.partial", section_path.display()),
                render::render_person_markdown(
                    &section.conversation,
                    &period,
                    &section.messages,
                    &section.media_paths,
                    options,
                ),
            )?;
            rename_partial(&section_path)?;
            sections.push(RowSection {
                conversation: section.conversation.clone(),
                note: section_path,
                media: section_media_files(directory, &section.messages, &section.media_paths),
            });
        }

        rows.push(PersonMonthRow {
            key: format!("{month_label}|{}", bucket.name),
            name: bucket.name.clone(),
            month: month_label.clone(),
            period,
            message_count: messages.len(),
            text_count: count_type(&messages, MessageType::Text),
            image_count: count_type(&messages, MessageType::Image),
            audio_count: count_type(&messages, MessageType::Audio),
            file_count: count_type(&messages, MessageType::File),
            voice_with_audio: count_voice_with_audio(&messages),
            missing_media: messages
                .iter()
                .flat_map(|message| message.media.iter())
                .filter(|item| item.content_hash.is_none())
                .count(),
            first_at: messages.first().map(|message| message.sent_at),
            last_at: messages.last().map(|message| message.sent_at),
            highlights: extract_highlights(&messages).to_summary(),
            transcript: render::truncate_for_cell(&render::render_transcript(
                &messages,
                &media_paths,
                options,
            )),
            summary: String::new(),
            sections,
        });
        notes.push(notes_path);
    }

    let workbook_path = directory.join(WORKBOOK_NAME);
    render::write_workbook(&workbook_path, &rows)?;

    Ok(GeneratedReport {
        directory: directory.to_path_buf(),
        workbook_path,
        notes,
        rows,
    })
}

/// Rewrite only the workbook, so summaries produced after generation can be
/// merged in without touching the transcripts or copied media.
pub fn write_workbook(directory: &Path, rows: &[PersonMonthRow]) -> Result<PathBuf, ReportError> {
    let workbook_path = directory.join(WORKBOOK_NAME);
    render::write_workbook(&workbook_path, rows)?;
    Ok(workbook_path)
}

/// Replace the summary cell of one person-month row and rewrite the workbook.
/// Unknown keys are ignored so a stale model reply cannot corrupt the table.
pub fn apply_summary(
    directory: &Path,
    key: &str,
    summary: &str,
    rows: &mut [PersonMonthRow],
) -> Result<PathBuf, ReportError> {
    if let Some(row) = rows.iter_mut().find(|row| row.key == key) {
        row.summary = summary.trim().to_owned();
    }
    write_workbook(directory, rows)
}

/// Resolve one section's media into the files the workbook links and embeds.
/// Only media referenced by this section's messages is returned, so a request
/// never sends a picture that belongs to a different conversation.
fn section_media_files(
    directory: &Path,
    messages: &[ReportMessage],
    media_paths: &MediaPaths,
) -> Vec<RowMediaFile> {
    let mut files: Vec<RowMediaFile> = Vec::new();
    for message in messages {
        for item in &message.media {
            let Some(hash) = item.content_hash.as_deref() else {
                continue;
            };
            let Some(relative) = media_paths.get(hash) else {
                continue;
            };
            if files.iter().any(|file| file.relative == *relative) {
                continue;
            }
            let kind = match message.message_type {
                MessageType::Image => MediaKind::Image,
                MessageType::Audio => MediaKind::Voice,
                _ => MediaKind::File,
            };
            let label = item
                .original_name
                .as_deref()
                .filter(|name| !name.trim().is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    relative
                        .rsplit('/')
                        .next()
                        .unwrap_or(relative.as_str())
                        .to_owned()
                });
            files.push(RowMediaFile {
                relative: relative.clone(),
                absolute: directory.join(relative),
                kind,
                label,
            });
        }
    }
    files
}

fn count_type(messages: &[ReportMessage], message_type: MessageType) -> usize {
    messages
        .iter()
        .filter(|message| message.message_type == message_type)
        .count()
}

fn count_voice_with_audio(messages: &[ReportMessage]) -> usize {
    messages
        .iter()
        .filter(|message| message.message_type == MessageType::Audio)
        .flat_map(|message| message.media.iter())
        .filter(|item| item.content_hash.is_some())
        .count()
}

/// Make one name unique inside the notes directory without letting a crafted
/// conversation name escape it or collide with a sibling.
fn unique_note_name(name: &str, used: &mut BTreeMap<String, usize>) -> String {
    let base = sanitize_component(name);
    let counter = used.entry(base.clone()).or_default();
    let candidate = if *counter == 0 {
        base
    } else {
        format!("{base}-{}", *counter + 1)
    };
    *counter += 1;
    candidate
}

/// Reduce a conversation name to a single safe path component: no separators,
/// no Windows-reserved characters, no leading dots, bounded length.
pub fn sanitize_component(value: &str) -> String {
    let mut cleaned = String::with_capacity(value.len());
    for character in value.chars() {
        if character.is_control()
            || matches!(
                character,
                '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
            )
        {
            cleaned.push('_');
        } else {
            cleaned.push(character);
        }
    }
    let trimmed = cleaned.trim().trim_matches('.').trim();
    if trimmed.is_empty() {
        return "未命名会话".to_owned();
    }
    trimmed.chars().take(60).collect()
}

/// Replace the file only if it does not already exist, mirroring the export
/// contract used elsewhere in the product.
fn rename_partial(target: &Path) -> Result<(), ReportError> {
    let partial = PathBuf::from(format!("{}.partial", target.display()));
    std::fs::rename(&partial, target)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use archive_domain::MediaIntegrity;

    fn message(
        minutes: i64,
        sender: &str,
        body: Option<&str>,
        message_type: MessageType,
    ) -> ReportMessage {
        ReportMessage {
            sent_at: DateTime::from_timestamp(1_700_000_000 + minutes * 60, 0).unwrap(),
            sender_name: Some(sender.to_owned()),
            sender_id: Some(sender.to_owned()),
            direction: archive_domain::MessageDirection::Incoming,
            message_type,
            body_text: body.map(str::to_owned),
            media: Vec::new(),
        }
    }

    fn conversation(name: &str, messages: Vec<ReportMessage>) -> ReportConversation {
        ReportConversation {
            conversation_id: format!("c-{name}"),
            name: name.to_owned(),
            conversation_type: Some("direct".into()),
            messages,
        }
    }

    #[test]
    fn month_filter_uses_the_local_offset_not_utc() {
        // 2023-11-14 22:13:20 UTC is already 2023-11-15 in UTC+8.
        let messages = vec![message(0, "我", Some("月初的边界"), MessageType::Text)];
        let options = ReportOptions {
            month: Some((2023, 11)),
            ..ReportOptions::default()
        };
        assert_eq!(in_scope(&messages, options).len(), 1);
        let december = ReportOptions {
            month: Some((2023, 12)),
            ..ReportOptions::default()
        };
        assert!(in_scope(&messages, december).is_empty());
    }

    #[test]
    fn voice_counts_only_cover_audio_that_reached_the_media_store() {
        let mut with_audio = message(0, "我", None, MessageType::Audio);
        with_audio.media = vec![archive_domain::MediaRefV1 {
            content_hash: Some("a".repeat(64)),
            original_name: Some("v.silk".into()),
            mime_type: None,
            size_bytes: Some(12),
            source_locator: "sha256:aa".into(),
            integrity: MediaIntegrity::Verified,
            missing_reason: None,
        }];
        let mut silent = message(1, "我", None, MessageType::Audio);
        silent.media = vec![archive_domain::MediaRefV1 {
            content_hash: None,
            original_name: None,
            mime_type: None,
            size_bytes: None,
            source_locator: "unavailable".into(),
            integrity: MediaIntegrity::Missing,
            missing_reason: Some("voice_collection_disabled".into()),
        }];
        let mut image = message(2, "我", None, MessageType::Image);
        image.media = vec![archive_domain::MediaRefV1 {
            content_hash: Some("b".repeat(64)),
            original_name: Some("p.png".into()),
            mime_type: None,
            size_bytes: Some(9),
            source_locator: "sha256:bb".into(),
            integrity: MediaIntegrity::Verified,
            missing_reason: None,
        }];

        let messages = vec![with_audio, silent, image];
        assert_eq!(count_voice_with_audio(&messages), 1);
        assert_eq!(count_type(&messages, MessageType::Image), 1);
    }

    #[test]
    fn sanitize_component_neutralizes_traversal_and_reserved_names() {
        assert_eq!(sanitize_component("../../etc/passwd"), "_.._etc_passwd");
        assert_eq!(sanitize_component("a:b*c?d"), "a_b_c_d");
        assert_eq!(sanitize_component("   "), "未命名会话");
        assert_eq!(sanitize_component("..."), "未命名会话");
        assert_eq!(sanitize_component("张三"), "张三");
    }

    #[test]
    fn duplicate_conversation_names_get_distinct_note_files() {
        let mut used = BTreeMap::new();
        assert_eq!(unique_note_name("同一个群", &mut used), "同一个群");
        assert_eq!(unique_note_name("同一个群", &mut used), "同一个群-2");
        assert_eq!(unique_note_name("另一个", &mut used), "另一个");
    }

    #[test]
    fn generate_writes_a_workbook_and_one_note_per_in_scope_conversation() {
        let directory = tempfile::tempdir().unwrap();
        let conversations = vec![
            conversation(
                "在范围内",
                vec![message(0, "我", Some("你好"), MessageType::Text)],
            ),
            conversation("空会话", Vec::new()),
        ];
        let options = ReportOptions {
            month: Some((2023, 11)),
            include_media: false,
            ..ReportOptions::default()
        };
        let report = generate(directory.path(), &conversations, None, options).unwrap();

        assert_eq!(report.rows.len(), 1);
        assert_eq!(report.rows[0].name, "我", "single sender becomes the counterpart");
        assert_eq!(report.rows[0].period, "2023-11-01 ~ 2023-11-30");
        assert_eq!(report.notes.len(), 1);
        assert!(report.workbook_path.is_file());
        let note = std::fs::read_to_string(&report.notes[0]).unwrap();
        assert!(note.contains("沟通记录：我"), "got {note}");
        assert!(note.contains("你好"), "got {note}");
        // The note holds the conversation only; the operator's instructions are
        // sent as the request's system message, not baked into the transcript.
        assert!(!note.contains("请按以下结构总结"), "got {note}");
    }

    /// The isolation guarantee the summary depends on: a group chat must never
    /// be filed under one of its members, otherwise that person's row — and the
    /// request built from it — would contain other people's words.
    #[test]
    fn a_group_chat_is_summarised_under_the_group_not_a_member() {
        let group = ReportConversation {
            conversation_id: "c-group".into(),
            name: "产品群".into(),
            conversation_type: Some("group".into()),
            messages: vec![
                message(0, "甲", Some("甲先说话"), MessageType::Text),
                message(1, "乙", Some("乙接着说"), MessageType::Text),
                message(2, "我", Some("我回复"), MessageType::Text),
            ],
        };
        assert_eq!(counterpart_name(&group), "产品群");

        let direct = ReportConversation {
            conversation_id: "c-direct".into(),
            name: "群名不会盖过对方".into(),
            conversation_type: Some("direct".into()),
            messages: vec![message(0, "张三", Some("单聊"), MessageType::Text)],
        };
        assert_eq!(counterpart_name(&direct), "张三");
    }

    /// Two conversations with the same person in the same month share a row,
    /// but each one keeps its own transcript file: one conversation, one
    /// request, no wording carried over from its neighbour.
    #[test]
    fn each_conversation_gets_its_own_section_and_note() {
        let directory = tempfile::tempdir().unwrap();
        let conversations = vec![
            conversation(
                "会话一",
                vec![message(0, "张三", Some("第一段内容"), MessageType::Text)],
            ),
            conversation(
                "会话二",
                vec![message(1, "张三", Some("第二段内容"), MessageType::Text)],
            ),
        ];
        let options = ReportOptions {
            include_media: false,
            ..ReportOptions::default()
        };
        let report = generate(directory.path(), &conversations, None, options).unwrap();

        assert_eq!(report.rows.len(), 1, "one person-month row");
        let sections = &report.rows[0].sections;
        assert_eq!(sections.len(), 2, "one section per conversation");
        let by_name: BTreeMap<&str, &RowSection> = sections
            .iter()
            .map(|section| (section.conversation.as_str(), section))
            .collect();
        assert_eq!(by_name.len(), 2, "both conversations keep their own section");
        assert_ne!(by_name["会话一"].note, by_name["会话二"].note);

        let first = std::fs::read_to_string(&by_name["会话一"].note).unwrap();
        let second = std::fs::read_to_string(&by_name["会话二"].note).unwrap();
        assert!(
            first.contains("第一段内容") && !first.contains("第二段内容"),
            "section notes must not bleed into each other"
        );
        assert!(
            second.contains("第二段内容") && !second.contains("第一段内容"),
            "section notes must not bleed into each other"
        );
        assert!(
            !first.contains("请按以下结构总结"),
            "the note must stay the conversation itself"
        );
    }

    #[test]
    fn each_section_only_carries_its_own_media() {
        let directory = tempfile::tempdir().unwrap();
        let media_root = directory.path().join("media");
        std::fs::create_dir_all(&media_root).unwrap();
        let first_hash = "1".repeat(64);
        let second_hash = "2".repeat(64);
        std::fs::write(media_root.join(&first_hash), [0x89u8, 0x50, 0x4E, 0x47]).unwrap();
        std::fs::write(media_root.join(&second_hash), [0xFFu8, 0xD8, 0xFF, 0xD9]).unwrap();

        let picture = |hash: &str| {
            let mut message = message(0, "张三", None, MessageType::Image);
            message.media = vec![archive_domain::MediaRefV1 {
                content_hash: Some(hash.to_owned()),
                original_name: None,
                mime_type: None,
                size_bytes: Some(4),
                source_locator: "sha256:x".into(),
                integrity: MediaIntegrity::Verified,
                missing_reason: None,
            }];
            message
        };
        let conversations = vec![
            conversation("会话一", vec![picture(&first_hash)]),
            conversation("会话二", vec![picture(&second_hash)]),
        ];
        let report = generate(
            directory.path(),
            &conversations,
            Some(&media_root),
            ReportOptions::default(),
        )
        .unwrap();

        let sections = &report.rows[0].sections;
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].media.len(), 1);
        assert_eq!(sections[1].media.len(), 1);
        assert_ne!(sections[0].media[0].relative, sections[1].media[0].relative);
        assert_eq!(sections[0].media[0].kind, MediaKind::Image);
        assert!(
            sections[0].media[0].absolute.is_file(),
            "the embedded thumbnail path must resolve to a real file"
        );
    }

    #[test]
    fn only_direct_drops_group_conversations_but_keeps_unlabelled_ones() {
        let directory = tempfile::tempdir().unwrap();
        let conversations = vec![
            ReportConversation {
                conversation_id: "c-direct".into(),
                name: "同事".into(),
                conversation_type: Some("direct".into()),
                messages: vec![message(0, "同事甲", Some("你好"), MessageType::Text)],
            },
            ReportConversation {
                conversation_id: "c-group".into(),
                name: "项目群".into(),
                conversation_type: Some("group".into()),
                messages: vec![message(0, "同事乙", Some("群消息"), MessageType::Text)],
            },
            ReportConversation {
                conversation_id: "c-unknown".into(),
                name: "未标注".into(),
                conversation_type: None,
                messages: vec![message(0, "同事丙", Some("内容"), MessageType::Text)],
            },
        ];

        let all = ReportOptions {
            include_media: false,
            ..ReportOptions::default()
        };
        let everyone = generate(&directory.path().join("all"), &conversations, None, all).unwrap();
        assert_eq!(everyone.rows.len(), 3);

        let direct = ReportOptions {
            only_direct: true,
            ..all
        };
        let report = generate(&directory.path().join("direct"), &conversations, None, direct).unwrap();
        assert_eq!(report.rows.len(), 2);
        assert!(
            report
                .rows
                .iter()
                .all(|row| !row.transcript.contains("群消息")),
            "the group conversation must be excluded, got {:?}",
            report.rows.iter().map(|row| &row.name).collect::<Vec<_>>()
        );
    }

    /// The whole point of the report: one row per person per calendar month,
    /// named after the other side and not the archive owner.
    #[test]
    fn rows_are_one_per_person_per_month_named_after_the_counterpart() {
        let directory = tempfile::tempdir().unwrap();
        let first_month = DateTime::parse_from_rfc3339("2026-10-09T02:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let second_month = first_month + Duration::days(31);

        let direct = |id: &str, at: DateTime<Utc>, body: &str| ReportConversation {
            conversation_id: id.into(),
            name: "归档里的会话名".into(),
            conversation_type: Some("direct".into()),
            messages: vec![
                ReportMessage {
                    sent_at: at,
                    sender_name: Some("张三".into()),
                    sender_id: Some("me".into()),
                    direction: archive_domain::MessageDirection::Outgoing,
                    message_type: MessageType::Text,
                    body_text: Some("我问了一件事".into()),
                    media: Vec::new(),
                },
                ReportMessage {
                    sent_at: at + Duration::minutes(1),
                    sender_name: Some("李四".into()),
                    sender_id: Some("peer".into()),
                    direction: archive_domain::MessageDirection::Incoming,
                    message_type: MessageType::Text,
                    body_text: Some(body.into()),
                    media: Vec::new(),
                },
            ],
        };

        let conversations = vec![
            direct("c1", first_month, "十月回复一"),
            // Same person, different conversation, same month: must merge.
            direct("c2", first_month + Duration::days(2), "十月回复二"),
            direct("c3", second_month, "十一月回复"),
        ];
        let options = ReportOptions {
            include_media: false,
            ..ReportOptions::default()
        };
        let report = generate(directory.path(), &conversations, None, options).unwrap();

        // October collapses to one row, September into another.
        assert_eq!(report.rows.len(), 2, "got {:?}", report.rows);
        let october_row = report
            .rows
            .iter()
            .find(|row| row.month == "2026-10")
            .expect("one row for the October bucket");
        assert_eq!(october_row.name, "李四", "row must be named after the counterpart");
        // c1 and c2 merge: two conversations, four messages.
        assert_eq!(october_row.message_count, 4);
        assert!(october_row.transcript.contains("十月回复一"));
        assert!(october_row.transcript.contains("十月回复二"));
        assert_eq!(october_row.period, "2026-10-01 ~ 2026-10-31");

        let november_row = report
            .rows
            .iter()
            .find(|row| row.month == "2026-11")
            .expect("one row for the November bucket");
        assert_eq!(november_row.message_count, 2);
    }

    #[test]
    fn transcript_column_carries_the_full_conversation() {
        let directory = tempfile::tempdir().unwrap();
        let conversations = vec![conversation(
            "会话",
            vec![
                message(0, "我", Some("第一条"), MessageType::Text),
                message(60, "对方", Some("第二条"), MessageType::Text),
            ],
        )];
        let options = ReportOptions {
            include_media: false,
            ..ReportOptions::default()
        };
        let report = generate(directory.path(), &conversations, None, options).unwrap();
        let transcript = &report.rows[0].transcript;
        assert!(transcript.contains("第一条"), "got {transcript}");
        assert!(transcript.contains("第二条"), "got {transcript}");
        assert!(transcript.contains("我："), "got {transcript}");
    }

    #[test]
    fn apply_summary_fills_one_row_and_ignores_unknown_ids() {
        let directory = tempfile::tempdir().unwrap();
        let conversations = vec![conversation(
            "会话",
            vec![message(0, "我", Some("内容"), MessageType::Text)],
        )];
        let options = ReportOptions {
            include_media: false,
            ..ReportOptions::default()
        };
        let mut report = generate(directory.path(), &conversations, None, options).unwrap();
        assert!(report.rows[0].summary.is_empty());

        apply_summary(
            directory.path(),
            &report.rows[0].key.clone(),
            "结论是继续推进",
            &mut report.rows,
        )
        .unwrap();
        assert_eq!(report.rows[0].summary, "结论是继续推进");

        apply_summary(
            directory.path(),
            "不存在",
            "不应写入",
            &mut report.rows,
        )
        .unwrap();
        assert_eq!(report.rows[0].summary, "结论是继续推进");
    }
}