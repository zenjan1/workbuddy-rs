/// 按字符数安全截断字符串(保证返回合法 &str,不切断多字节字符)。
pub fn clip(s: &str, max_chars: usize) -> &str {
    s.char_indices()
        .nth(max_chars)
        .map(|(i, _)| &s[..i])
        .unwrap_or(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_short_string() {
        assert_eq!(clip("abc", 10), "abc");
    }

    #[test]
    fn clip_long_string_on_char_boundary() {
        assert_eq!(clip("中文测试abc", 3), "中文测");
    }

    #[test]
    fn test_clip_empty() {
        assert_eq!(clip("", 5), "");
    }

    #[test]
    fn test_clip_exact() {
        // 英文字符数恰好等于max
        assert_eq!(clip("abc", 3), "abc");
        // 中文字符数恰好等于max
        assert_eq!(clip("中文测", 3), "中文测");
        // emoji字符数恰好等于max
        assert_eq!(clip("🚀", 1), "🚀");
    }

    #[test]
    fn test_clip_overflow() {
        // 中文+英文混合字符串超过max
        assert_eq!(clip("中文测试abc", 3), "中文测");
        // 多字节字符在中间位置
        assert_eq!(clip("abc中文测试", 4), "abc中");
        // emoji在中间被截断保护
        assert_eq!(clip("abc🚀xyz", 3), "abc");
        // 纯英文字符串超过max
        assert_eq!(clip("abcdef", 3), "abc");
    }
}
