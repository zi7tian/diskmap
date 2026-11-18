//! 包管理器文本字段的 UTF-8 解码；路径比较统一位于 platform::path。

pub(super) fn first_char(bytes: &[u8]) -> Option<(char, usize)> {
    let &first = bytes.first()?;
    let width = match first {
        0..=0x7f => 1,
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => return None,
    };
    std::str::from_utf8(bytes.get(..width)?)
        .ok()?
        .chars()
        .next()
        .map(|c| (c, width))
}
