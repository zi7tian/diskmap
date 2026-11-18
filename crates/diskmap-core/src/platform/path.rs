//! 唯一的路径大小写规则：Windows 使用 BMP 简单大写，Unix 按原字节。
//!
//! 输入为 `OsStr::as_encoded_bytes()`，不依赖宿主平台。Windows 按 UTF-16 码元逐个
//! 映射（Unicode 17.0），孤立代理项按 WTF-8 解码并保持原样；增补平面、非法字节也不变。
//! 不做上下文变换、字符展开或 Unicode 规范化；例如 Σ/σ/ς 相等，ß/ẞ、ﬀ/FF 不等。
//! 键使用折叠后的 WTF-8 字节顺序；比较和键的顺序完全一致。这里不读取卷的 `$UpCase`，
//! 也不解析符号链接、点、重复或末尾分隔符。macOS 与旧版一样区分大小写。

use std::cmp::Ordering;

use super::{Os, upcase::UPPER};

/// 名字的比较键；只有 Windows 折叠大小写，不改分隔符。
#[inline]
pub fn name_key(os: Os, input: &[u8]) -> Vec<u8> {
    key::<false>(os, input)
}

/// 比较名字；与 [`name_key`] 的字节顺序一致，且不分配内存。
#[inline]
pub fn cmp_names(os: Os, a: &[u8], b: &[u8]) -> Ordering {
    compare::<false>(os, a, b)
}

/// 路径的比较键；Windows 额外把 `/` 变为 `\`。
#[inline]
pub fn path_key(os: Os, input: &[u8]) -> Vec<u8> {
    key::<true>(os, input)
}

/// 比较路径；与 [`path_key`] 的字节顺序一致，且不分配内存。
#[inline]
pub fn cmp_paths(os: Os, a: &[u8], b: &[u8]) -> Ordering {
    compare::<true>(os, a, b)
}

/// 已由 [`path_key`] 生成的键与原始路径比较；查询过程不分配内存。
#[inline]
pub fn cmp_path_key(os: Os, key: &[u8], input: &[u8]) -> Ordering {
    if !os.case_insensitive() {
        return key.cmp(input);
    }
    if input.is_ascii() {
        return key
            .iter()
            .copied()
            .cmp(input.iter().map(|&b| ascii::<true>(b)));
    }
    key.iter().copied().cmp(Fold::<true>::new(input))
}

/// MFT 连续存储复用目标缓冲区，避免逐文件分配短键。
pub(crate) fn extend_name_key(os: Os, input: &[u8], output: &mut Vec<u8>) {
    extend::<false>(os, input, output);
}

#[inline]
fn key<const PATH: bool>(os: Os, input: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(input.len());
    extend::<PATH>(os, input, &mut output);
    output
}

#[inline]
fn extend<const PATH: bool>(os: Os, input: &[u8], output: &mut Vec<u8>) {
    if !os.case_insensitive() {
        output.extend_from_slice(input);
    } else if input.is_ascii() {
        output.extend(input.iter().map(|&b| ascii::<PATH>(b)));
    } else {
        output.extend(Fold::<PATH>::new(input));
    }
}

#[inline]
fn compare<const PATH: bool>(os: Os, a: &[u8], b: &[u8]) -> Ordering {
    if !os.case_insensitive() {
        return a.cmp(b);
    }
    if a.is_ascii() && b.is_ascii() {
        return a
            .iter()
            .map(|&b| ascii::<PATH>(b))
            .cmp(b.iter().map(|&b| ascii::<PATH>(b)));
    }
    Fold::<PATH>::new(a).cmp(Fold::<PATH>::new(b))
}

#[inline]
fn ascii<const PATH: bool>(byte: u8) -> u8 {
    if PATH && byte == b'/' {
        b'\\'
    } else {
        byte.to_ascii_uppercase()
    }
}

/// 流式读取 UTF-8/WTF-8，代理码元保持编码，不使用有损转换。
struct Fold<'a, const PATH: bool> {
    rest: &'a [u8],
    pending: [u8; 3],
    start: usize,
    end: usize,
}

impl<'a, const PATH: bool> Fold<'a, PATH> {
    fn new(rest: &'a [u8]) -> Self {
        Self {
            rest,
            pending: [0; 3],
            start: 0,
            end: 0,
        }
    }
}

impl<const PATH: bool> Iterator for Fold<'_, PATH> {
    type Item = u8;

    #[inline]
    fn next(&mut self) -> Option<u8> {
        if self.start < self.end {
            let byte = self.pending[self.start];
            self.start += 1;
            return Some(byte);
        }
        let &first = self.rest.first()?;
        if first < 0x80 {
            self.rest = &self.rest[1..];
            return Some(ascii::<PATH>(first));
        }
        let continuation = |b: u8| b & 0xc0 == 0x80;
        let decoded = match self.rest {
            [a @ 0xc2..=0xdf, b, ..] if continuation(*b) => {
                Some((((a & 0x1f) as u16) << 6 | (b & 0x3f) as u16, 2))
            }
            [a @ 0xe0..=0xef, b, c, ..]
                if continuation(*b) && continuation(*c) && (*a != 0xe0 || *b >= 0xa0) =>
            {
                Some((
                    ((a & 0x0f) as u16) << 12 | ((b & 0x3f) as u16) << 6 | (c & 0x3f) as u16,
                    3,
                ))
            }
            _ => None,
        };
        let Some((unit, width)) = decoded else {
            // 四字节增补字符和不合法的编码逐字节通过，不会误当作 BMP 码元。
            self.rest = &self.rest[1..];
            return Some(first);
        };
        self.rest = &self.rest[width..];
        let upper = UPPER[unit as usize];
        self.start = 1;
        self.end = if upper < 0x80 {
            self.pending[0] = upper as u8;
            1
        } else if upper < 0x800 {
            self.pending[0] = 0xc0 | (upper >> 6) as u8;
            self.pending[1] = 0x80 | (upper & 0x3f) as u8;
            2
        } else {
            self.pending[0] = 0xe0 | (upper >> 12) as u8;
            self.pending[1] = 0x80 | ((upper >> 6) & 0x3f) as u8;
            self.pending[2] = 0x80 | (upper & 0x3f) as u8;
            3
        };
        Some(self.pending[0])
    }
}

#[cfg(test)]
mod tests;
