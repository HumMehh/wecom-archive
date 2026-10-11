//! Extractive highlighting.
//!
//! No model runs here: the workbook needs a starting point even when nothing
//! external is configured. The extractor is deliberately conservative — it
//! surfaces frequent terms and sentences that read like decisions, deadlines
//! or risks, and never invents text that was not in the conversation.

use std::collections::BTreeMap;

use archive_domain::MessageType;

use crate::ReportMessage;

const KEYWORD_MIN_FREQUENCY: usize = 3;
const KEYWORD_LIMIT: usize = 12;
const SIGNAL_LIMIT: usize = 8;
const MIN_SIGNAL_LENGTH: usize = 4;
const MAX_SIGNAL_LENGTH: usize = 120;

const STOPWORDS: &[&str] = &[
    "我们", "你们", "他们", "这个", "那个", "什么", "怎么", "可以", "应该", "已经", "还是", "就是",
    "不是", "没有", "一个", "现在", "然后", "这样", "那样", "因为", "所以", "但是", "如果", "可能",
    "需要", "进行", "这边", "那边", "大家", "问题", "时候", "东西", "事情", "一下", "知道", "觉得",
    "看到", "出来", "进去", "的话", "一下", "现在",
];

/// Sentences containing any of these usually carry a decision, an owner, a date
/// or an open risk.
const SIGNAL_MARKERS: &[&str] = &[
    "截止", "完成", "确认", "上线", "交付", "需求", "反馈", "问题", "风险", "负责", "安排", "待办",
    "跟进", "同步", "评审", "测试", "延期", "阻塞", "报错", "故障", "方案", "结论",
];

const SENTENCE_BREAKS: &[char] = &['。', '！', '？', '!', '?', '；', ';', '\n', '\r'];

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Highlights {
    pub keywords: Vec<String>,
    pub signals: Vec<String>,
}

impl Highlights {
    /// One cell value for the workbook. Empty when nothing stood out.
    pub fn to_summary(&self) -> String {
        let mut parts = Vec::new();
        if !self.keywords.is_empty() {
            parts.push(format!("高频词：{}", self.keywords.join("、")));
        }
        if !self.signals.is_empty() {
            parts.push(format!("关键句：{}", self.signals.join(" / ")));
        }
        parts.join("\n")
    }
}

pub fn extract_highlights(messages: &[ReportMessage]) -> Highlights {
    let mut frequencies: BTreeMap<String, usize> = BTreeMap::new();
    let mut sentences = Vec::new();

    for message in messages {
        let Some(text) = message
            .body_text
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
        else {
            continue;
        };
        collect_signals(text, &mut sentences);
        if message.message_type != MessageType::Text {
            continue;
        }
        collect_grams(text, &mut frequencies);
    }

    Highlights {
        keywords: rank_keywords(&frequencies),
        signals: sentences,
    }
}

fn split_sentences(text: &str) -> Vec<String> {
    text.split(SENTENCE_BREAKS)
        .map(|sentence| sentence.trim().to_owned())
        .filter(|sentence| sentence.chars().count() >= MIN_SIGNAL_LENGTH)
        .collect()
}

fn collect_signals(text: &str, into: &mut Vec<String>) {
    // `into` is shared across messages, so the cap has to be re-checked for
    // every message rather than only after this message's own push.
    for sentence in split_sentences(text) {
        if into.len() >= SIGNAL_LIMIT {
            return;
        }
        if !SIGNAL_MARKERS
            .iter()
            .any(|marker| sentence.contains(*marker))
        {
            continue;
        }
        if sentence.chars().count() > MAX_SIGNAL_LENGTH {
            continue;
        }
        let signature: String = sentence.chars().take(24).collect();
        if into
            .iter()
            .any(|existing: &String| existing.starts_with(&signature))
        {
            continue;
        }
        into.push(sentence);
    }
}

/// Count overlapping 2..=4 character CJK runs. Runs are cut at anything that is
/// not a CJK character, a letter or a digit, so punctuation and whitespace
/// terminate a candidate instead of joining two unrelated halves.
fn collect_grams(text: &str, into: &mut BTreeMap<String, usize>) {
    for segment in split_segments(text) {
        let characters: Vec<char> = segment.chars().collect();
        let maximum = characters.len().min(4);
        if maximum < 2 {
            continue;
        }
        for size in 2..=maximum {
            for start in 0..=(characters.len() - size) {
                let gram: String = characters[start..start + size].iter().collect();
                if STOPWORDS.contains(&gram.as_str()) {
                    continue;
                }
                *into.entry(gram).or_default() += 1;
            }
        }
    }
}

fn split_segments(text: &str) -> Vec<String> {
    text.split(|character: char| {
        !(is_cjk(character) || character.is_alphanumeric())
    })
    .filter(|segment| segment.chars().any(is_cjk))
    .map(str::to_owned)
    .collect()
}

fn is_cjk(character: char) -> bool {
    matches!(character as u32, 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF)
}

/// Keep frequent grams, then drop a short gram that a longer equally frequent
/// gram already contains, so "项目管理" wins over "项目" and "管理".
fn rank_keywords(frequencies: &BTreeMap<String, usize>) -> Vec<String> {
    let mut candidates: Vec<(&String, usize)> = frequencies
        .iter()
        .filter(|(_, count)| **count >= KEYWORD_MIN_FREQUENCY)
        .map(|(gram, count)| (gram, *count))
        .collect();

    candidates.retain(|(gram, count)| {
        !frequencies.iter().any(|(other, other_count)| {
            other != *gram
                && other.chars().count() > gram.chars().count()
                && other.contains(gram.as_str())
                && *other_count >= *count
        })
    });

    candidates.sort_by(|left, right| {
        right
            .1
            .cmp(&left.1)
            .then_with(|| right.0.chars().count().cmp(&left.0.chars().count()))
            .then_with(|| left.0.cmp(right.0))
    });
    candidates.truncate(KEYWORD_LIMIT);
    candidates
        .into_iter()
        .map(|(gram, count)| format!("{gram}({count})"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ReportConversation, ReportOptions, generate};
    use archive_domain::MessageType;
    use chrono::DateTime;

    fn text(body: &str) -> ReportMessage {
        ReportMessage {
            sent_at: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            sender_name: Some("我".into()),
            sender_id: Some("p1".into()),
            direction: archive_domain::MessageDirection::Incoming,
            message_type: MessageType::Text,
            body_text: Some(body.to_owned()),
            media: Vec::new(),
        }
    }

    #[test]
    fn repeated_terms_become_keywords_and_generic_words_are_dropped() {
        let messages = vec![
            text("项目管理的进度怎么样"),
            text("项目管理这周继续推进"),
            text("项目管理需要再对一次"),
            text("我们的那个事情"),
        ];
        let highlights = extract_highlights(&messages);
        assert!(
            highlights
                .keywords
                .iter()
                .any(|keyword| keyword.starts_with("项目管理")),
            "expected 项目管理 in {:?}",
            highlights.keywords
        );
        assert!(
            !highlights
                .keywords
                .iter()
                .any(|keyword| keyword.starts_with("我们的")),
            "stop words should not surface"
        );
    }

    #[test]
    fn decision_sentences_are_surfaced_and_deduplicated() {
        let messages = vec![
            text("确认一下，10月20日完成交付。"),
            text("确认一下，10月20日完成交付。"),
            text("今天天气不错。"),
        ];
        let highlights = extract_highlights(&messages);
        assert_eq!(highlights.signals.len(), 1);
        assert!(highlights.signals[0].contains("交付"));
    }

    #[test]
    fn signal_extraction_stops_at_the_limit() {
        let messages = (0..40)
            .map(|index| text(&format!("第{index}条需求已经完成")))
            .collect::<Vec<_>>();
        let highlights = extract_highlights(&messages);
        assert_eq!(highlights.signals.len(), SIGNAL_LIMIT);
    }

    #[test]
    fn summary_cell_combines_both_sections() {
        let highlights = Highlights {
            keywords: vec!["上线(3)".into()],
            signals: vec!["确认完成交付".into()],
        };
        assert_eq!(
            highlights.to_summary(),
            "高频词：上线(3)\n关键句：确认完成交付"
        );
        assert!(Highlights::default().to_summary().is_empty());
    }

    #[test]
    fn highlight_output_lands_in_the_workbook_row() {
        let directory = tempfile::tempdir().unwrap();
        let conversation = ReportConversation {
            conversation_id: "c1".into(),
            name: "项目群".into(),
            conversation_type: Some("group".into()),
            messages: vec![
                text("上线时间需要确认"),
                text("上线时间需要确认"),
                text("上线时间需要确认"),
            ],
        };
        let options = ReportOptions {
            include_media: false,
            ..ReportOptions::default()
        };
        let report = generate(directory.path(), &[conversation], None, options).unwrap();
        assert!(
            report.rows[0].highlights.contains("高频词"),
            "got {:?}",
            report.rows[0].highlights
        );
    }
}