//! 输出前的链接与敏感内容过滤。

/// 从协议前缀直到下一个空白字符整体替换，保留周围文本。
pub fn urls(text: &str) -> String {
    let mut rest = text;
    let mut output = String::new();
    while let Some(start) = rest
        .find("http://")
        .into_iter()
        .chain(rest.find("https://"))
        .min()
    {
        output.push_str(&rest[..start]);
        output.push_str("<url>");
        let tail = &rest[start..];
        let end = tail.find(char::is_whitespace).unwrap_or(tail.len());
        rest = &tail[end..];
    }
    output.push_str(rest);
    output
}

/// 链接之外还过滤协议敏感词与当前命令已知的秘密值。
pub fn message(text: &str, secrets: &[&str]) -> String {
    let text = urls(text);
    let forbidden = [
        "Xiaomi",
        "xiaomi",
        "mi.com",
        "passToken",
        "serviceToken",
        "ssecurity",
        "userId",
        "deviceId",
        "did=",
        "DID",
        "cookie",
        "Cookie",
        "ota_url",
        "OTA_URL",
        ".bin",
    ];
    let mut ranges: Vec<_> = secrets
        .iter()
        .copied()
        .chain(forbidden)
        .filter(|s| !s.is_empty())
        .flat_map(|s| {
            text.match_indices(s)
                .map(|(start, m)| (start, start + m.len()))
        })
        .collect();
    ranges.sort_unstable();
    let mut ranges = ranges.into_iter().peekable();
    let mut output = String::new();
    let mut cursor = 0;
    while let Some((start, mut end)) = ranges.next() {
        while let Some((_, next_end)) = ranges.next_if(|&(next_start, _)| next_start <= end) {
            end = end.max(next_end);
        }
        output.push_str(&text[cursor..start]);
        output.push_str("<已隐藏>");
        cursor = end;
    }
    output.push_str(&text[cursor..]);
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_nested_secret_is_fully_hidden() {
        let key = "MDEyMzQ1Njc4OWFiY2RlZg==";
        let output = message(&format!("前 {key} 后"), &["1", key]);
        assert_eq!(output, "前 <已隐藏> 后");
        for fragment in key.as_bytes().windows(3) {
            assert!(!output.contains(std::str::from_utf8(fragment).unwrap()));
        }
    }
    #[test]
    fn redact_overlapping_secrets_merge() {
        assert_eq!(
            message("前 abcdefghi 后", &["abcdef", "defghi"]),
            "前 <已隐藏> 后"
        );
        assert_eq!(message("前 甲乙cookie 后", &["", "甲乙"]), "前 <已隐藏> 后");
    }
    #[test]
    fn redact_query_url() {
        assert_eq!(
            urls("请访问 https://example.invalid/a?token=synthetic 完成验证"),
            "请访问 <url> 完成验证"
        );
    }
    #[test]
    fn redact_multiple_urls() {
        assert_eq!(
            urls("甲http://example.invalid/a\t乙https://example.invalid/b\n尾"),
            "甲<url>\t乙<url>\n尾"
        );
    }
    #[test]
    fn redact_without_url() {
        assert_eq!(urls("中文说明，无链接"), "中文说明，无链接");
    }
    #[test]
    fn redact_sensitive_words_and_values() {
        assert_eq!(
            message("凭据 synthetic-secret cookie Xiaomi", &["synthetic-secret"]),
            "凭据 <已隐藏> <已隐藏> <已隐藏>"
        );
    }
}
