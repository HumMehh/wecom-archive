// 临时验证：用合成数据生成一份沟通记录供人工检查，不含任何真实聊天内容。
use std::path::Path;

use archive_domain::{MediaIntegrity, MediaRefV1, MessageDirection, MessageType};
use chrono::{DateTime, Duration, Utc};
use communication_report::{ReportConversation, ReportMessage, ReportOptions, generate};

fn media(hash: Option<&str>, name: Option<&str>, reason: Option<&str>) -> MediaRefV1 {
    MediaRefV1 {
        content_hash: hash.map(str::to_owned),
        original_name: name.map(str::to_owned),
        mime_type: None,
        size_bytes: None,
        source_locator: "sha256:x".into(),
        integrity: if hash.is_some() {
            MediaIntegrity::Verified
        } else {
            MediaIntegrity::Missing
        },
        missing_reason: reason.map(str::to_owned),
    }
}

/// `sent_at` is UTC; the report renders it in the local offset it is given.
fn message(
    base: DateTime<Utc>,
    offset: i64,
    sender: &str,
    sender_id: &str,
    direction: MessageDirection,
    body: &str,
    kind: MessageType,
) -> ReportMessage {
    ReportMessage {
        sent_at: base + Duration::minutes(offset),
        sender_name: Some(sender.into()),
        sender_id: Some(sender_id.into()),
        direction,
        message_type: kind,
        body_text: Some(body.into()),
        media: Vec::new(),
    }
}

#[test]
fn writes_a_sample_report_for_manual_inspection() {
    let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/sample-report");
    let _ = std::fs::remove_dir_all(&out);
    let media_root = out.join("media-store");
    std::fs::create_dir_all(&media_root).unwrap();
    let png_hash = "e".repeat(64);
    // A real 1x1 RGBA PNG so the workbook actually embeds a thumbnail.
    const TINY_PNG: [u8; 70] = [137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 218, 99, 252, 207, 192, 80, 15, 0, 4, 133, 1, 128, 132, 169, 140, 33, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130];
    std::fs::write(media_root.join(&png_hash), TINY_PNG).unwrap();
    // A voice file that really exists, so the workbook gets a playable row.
    let silk_hash = "f".repeat(64);
    std::fs::write(media_root.join(&silk_hash), b"#!SILK_V3 synthetic").unwrap();

    // 2026-10-09 02:00 UTC == 2026-10-09 10:00 in UTC+8
    let base = chrono::DateTime::parse_from_rfc3339("2026-10-09T02:00:00Z")
        .unwrap()
        .with_timezone(&Utc);

    // 李四: two separate chats in October plus one in September.
    let mut october_direct = vec![
        message(
            base,
            0,
            "张三",
            "me",
            MessageDirection::Outgoing,
            "项目的上线时间确认了吗？",
            MessageType::Text,
        ),
        message(
            base,
            10,
            "李四",
            "peer-li",
            MessageDirection::Incoming,
            "确认了，10月20日完成交付，预算是 12 万元。",
            MessageType::Text,
        ),
    ];
    let mut with_image = message(
        base,
        20,
        "李四",
        "peer-li",
        MessageDirection::Incoming,
        "",
        MessageType::Image,
    );
    with_image.media = vec![media(Some(&png_hash), Some("截图.png"), None)];
    let mut with_voice = message(
        base,
        30,
        "李四",
        "peer-li",
        MessageDirection::Incoming,
        "",
        MessageType::Audio,
    );
    with_voice.media = vec![media(Some(&silk_hash), Some("语音.silk"), None)];
    october_direct.extend([with_image, with_voice]);

    let september_direct = vec![
        message(
            base - Duration::days(31),
            0,
            "李四",
            "peer-li",
            MessageDirection::Incoming,
            "上个月的项目还没结项。",
            MessageType::Text,
        ),
        message(
            base - Duration::days(31),
            5,
            "张三",
            "me",
            MessageDirection::Outgoing,
            "收到，这个月跟进。",
            MessageType::Text,
        ),
    ];

    let mut group = vec![message(
        base,
        5,
        "王五",
        "peer-wang",
        MessageDirection::Incoming,
        "第三方接口还没联调完。",
        MessageType::Text,
    )];
    let mut with_file = message(
        base,
        40,
        "张三",
        "me",
        MessageDirection::Outgoing,
        "交付清单.xlsx",
        MessageType::File,
    );
    with_file.media = vec![media(
        None,
        Some("交付清单.xlsx"),
        Some("not_found_in_authorized_media_roots"),
    )];
    group.push(with_file);

    let conversations = vec![
        ReportConversation {
            conversation_id: "c-oct-li".into(),
            name: "归档会话名一".into(),
            conversation_type: Some("direct".into()),
            messages: october_direct,
        },
        ReportConversation {
            conversation_id: "c-oct-li2".into(),
            name: "归档会话名二".into(),
            conversation_type: Some("direct".into()),
            messages: vec![message(
                base + Duration::days(1),
                0,
                "李四",
                "peer-li",
                MessageDirection::Incoming,
                "项目管理再确认一次风险。",
                MessageType::Text,
            )],
        },
        ReportConversation {
            conversation_id: "c-sep-li".into(),
            name: "归档会话名三".into(),
            conversation_type: Some("direct".into()),
            messages: september_direct,
        },
        ReportConversation {
            conversation_id: "R:group".into(),
            name: "交付群/测试".into(),
            conversation_type: Some("group".into()),
            messages: group,
        },
    ];

    let options = ReportOptions {
        month: Some((2026, 10)),
        utc_offset_minutes: 480,
        include_media: true,
        only_direct: true,
    };
    let report = generate(&out, &conversations, Some(&media_root), options).unwrap();

    println!("工作簿: {}", report.workbook_path.display());
    for row in &report.rows {
        println!(
            "行 -> 姓名={} | 时间={} | 消息{} | 语音{} | 带音频{} | 全文{}字",
            row.name,
            row.period,
            row.message_count,
            row.audio_count,
            row.voice_with_audio,
            row.transcript.chars().count()
        );
    }

    // Only October is in scope, so the September conversation drops out; the
    // group is excluded; both 李四 conversations merge into a single row.
    assert_eq!(report.rows.len(), 1, "got {:?}", report.rows);
    assert_eq!(report.rows[0].name, "李四");
    assert_eq!(report.rows[0].message_count, 5);
    assert!(report.workbook_path.is_file());
}