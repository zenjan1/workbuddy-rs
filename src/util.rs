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
}
