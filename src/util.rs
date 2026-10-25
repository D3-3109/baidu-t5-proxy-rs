//! 字节/文本工具：子串查找、latin-1 编解码、手写正则等价物。

/// 在 haystack 中查找 needle 的首次出现位置。
pub fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// latin-1 字节 -> String（每个字节对应 U+0000..=U+00FF）。
pub fn latin1_to_string(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect()
}

/// String -> latin-1 字节。输入应来自 latin1_to_string，反向一一对应。
pub fn latin1_to_bytes(s: &str) -> Vec<u8> {
    s.chars().map(|c| c as u32 as u8).collect()
}

/// 等价于 Python `re.match(r"HTTP/1\.\d\s+(\d{3})", line)`。
pub fn status_code(line: &str) -> Option<u16> {
    let b = line.as_bytes();
    if b.len() < 9 || &b[..7] != b"HTTP/1." || !b[7].is_ascii_digit() {
        return None;
    }
    let mut i = 8;
    while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c) {
        i += 1;
    }
    if i + 3 > b.len() || !b[i..i + 3].iter().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let d = |k: usize| (b[i + k] - b'0') as u16;
    Some(d(0) * 100 + d(1) * 10 + d(2))
}

/// 等价于 Python `re.search(r"(\d{1,3}(?:\.\d{1,3}){3})", ...)`，返回第一个匹配。
pub fn find_ipv4(bytes: &[u8]) -> Option<String> {
    for start in 0..bytes.len() {
        if !bytes[start].is_ascii_digit() {
            continue;
        }
        if let Some(end) = match_ip_from(bytes, start, 4) {
            return Some(String::from_utf8_lossy(&bytes[start..end]).into_owned());
        }
    }
    None
}

fn digit_run(b: &[u8], pos: usize) -> usize {
    let mut n = 0;
    while n < 3 && pos + n < b.len() && b[pos + n].is_ascii_digit() {
        n += 1;
    }
    n
}

/// 模拟正则的贪婪 + 回溯：每组取尽量长的数字，失败则回退。
fn match_ip_from(b: &[u8], pos: usize, groups_left: usize) -> Option<usize> {
    let run = digit_run(b, pos);
    if run == 0 {
        return None;
    }
    for len in (1..=run).rev() {
        let end = pos + len;
        if groups_left == 1 {
            return Some(end);
        }
        if end < b.len() && b[end] == b'.' {
            if let Some(e) = match_ip_from(b, end + 1, groups_left - 1) {
                return Some(e);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_line_parsing() {
        assert_eq!(
            status_code("HTTP/1.1 200 Connection established"),
            Some(200)
        );
        assert_eq!(status_code("HTTP/1.0  403 Forbidden"), Some(403));
        assert_eq!(status_code("HTTP/1.1 403 Forbidden"), Some(403));
        assert_eq!(status_code("HTTP/2 200"), None);
        assert_eq!(status_code("garbage"), None);
        assert_eq!(status_code("HTTP/1.1 20x"), None);
    }

    #[test]
    fn ipv4_search_matches_regex_semantics() {
        let f = |s: &str| find_ipv4(s.as_bytes()).unwrap();
        assert_eq!(f("egress is 1.2.3.4 ok"), "1.2.3.4");
        // 与正则一致：贪婪回溯后 "x1234.5.6.7" 里匹配到 "234.5.6.7"
        assert_eq!(f("x1234.5.6.7"), "234.5.6.7");
        assert_eq!(
            find_ipv4(b"current ip is 10.0.0.1 from 8.8.8.8").unwrap(),
            "10.0.0.1"
        );
    }

    #[test]
    fn ipv4_no_match() {
        assert_eq!(find_ipv4(b"1.2345.6.7"), None);
        assert_eq!(find_ipv4(b""), None);
    }
}
