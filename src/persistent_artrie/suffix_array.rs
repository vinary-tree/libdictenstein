//! Suffix-array helpers shared by the native persistent suffix indexes.

/// Return byte suffix starts in lexicographic suffix order.
///
/// Uses prefix doubling over integer ranks. This avoids repeatedly comparing
/// whole suffix slices in sort comparators, which is especially costly for long
/// shared prefixes.
pub(crate) fn sorted_byte_suffix_starts(bytes: &[u8]) -> Vec<usize> {
    let n = bytes.len();
    let mut suffixes: Vec<usize> = (0..n).collect();
    if n <= 1 {
        return suffixes;
    }

    let mut ranks: Vec<u32> = bytes.iter().map(|&byte| u32::from(byte)).collect();
    let mut next_ranks = vec![0_u32; n];
    let mut width = 1_usize;

    loop {
        suffixes.sort_unstable_by_key(|&start| suffix_key(&ranks, start, width));

        let mut class = 0_u32;
        next_ranks[suffixes[0]] = class;
        let mut previous_key = suffix_key(&ranks, suffixes[0], width);

        for &start in suffixes.iter().skip(1) {
            let key = suffix_key(&ranks, start, width);
            if key != previous_key {
                class += 1;
                previous_key = key;
            }
            next_ranks[start] = class;
        }

        std::mem::swap(&mut ranks, &mut next_ranks);

        if class as usize + 1 == n {
            break;
        }
        width = width.saturating_mul(2).min(n);
    }

    suffixes
}

/// Return UTF-8 character-boundary suffix starts in Rust `str` lexicographic order.
pub(crate) fn sorted_char_boundary_suffix_starts(text: &str) -> Vec<usize> {
    sorted_byte_suffix_starts(text.as_bytes())
        .into_iter()
        .filter(|&start| text.is_char_boundary(start))
        .collect()
}

#[inline]
fn suffix_key(ranks: &[u32], start: usize, width: usize) -> (u32, Option<u32>) {
    (ranks[start], ranks.get(start + width).copied())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn naive_byte_suffix_starts(bytes: &[u8]) -> Vec<usize> {
        let mut starts: Vec<usize> = (0..bytes.len()).collect();
        starts.sort_by(|left, right| bytes[*left..].cmp(&bytes[*right..]));
        starts
    }

    fn naive_char_suffix_starts(text: &str) -> Vec<usize> {
        let mut starts: Vec<usize> = text.char_indices().map(|(idx, _)| idx).collect();
        starts.sort_by(|left, right| text[*left..].cmp(&text[*right..]));
        starts
    }

    #[test]
    fn byte_suffix_array_matches_naive_order() {
        for bytes in [
            b"".as_slice(),
            b"a",
            b"banana",
            b"mississippi",
            b"aaaaaaaaaaaaaaaa",
            b"abcabcabcabc",
            b"\xff\x00\xff\x00\x01",
        ] {
            assert_eq!(
                sorted_byte_suffix_starts(bytes),
                naive_byte_suffix_starts(bytes)
            );
        }
    }

    #[test]
    fn char_boundary_suffix_array_matches_naive_str_order() {
        for text in [
            "",
            "banana",
            "mississippi",
            "caf\u{e9}\u{1f389}a",
            "\u{1f389}a\u{1f389}",
            "\u{00e9}e\u{00e9}",
        ] {
            assert_eq!(
                sorted_char_boundary_suffix_starts(text),
                naive_char_suffix_starts(text)
            );
        }
    }
}
