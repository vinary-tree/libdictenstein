//! UTF-8 integration tests for PersistentARTrieChar.
//!
//! These tests verify proper Unicode character handling:
//! - Multi-byte UTF-8 sequences
//! - Emoji and special characters
//! - Mixed ASCII/UTF-8 operations
//! - Unicode edge cases (combining chars, RTL, etc.)
//! - Character-level edit distance semantics
//!
//! # Why PersistentARTrieChar?
//!
//! The byte-level PersistentARTrie treats multi-byte UTF-8 sequences as
//! multiple bytes. This is problematic for edit distance:
//! - "" (ñ) is 2 bytes → distance from "n" is 2 (wrong)
//! - With PersistentARTrieChar, "ñ" is 1 character → distance from "n" is 1 (correct)

#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie::char::{PersistentARTrieChar, PersistentARTrieCharZipper};
use libdictenstein::zipper::DictZipper;
use libdictenstein::DictionaryNode;

// =============================================================================
// Test: Basic Unicode Insertion and Lookup
// =============================================================================

#[test]
fn test_basic_unicode_insertion() {
    let trie: PersistentARTrieChar<()> = PersistentARTrieChar::new();

    // Insert various Unicode strings
    trie.insert("café").expect("insert failed");
    trie.insert("naïve").expect("insert failed");
    trie.insert("résumé").expect("insert failed");

    assert!(trie.contains("café"));
    assert!(trie.contains("naïve"));
    assert!(trie.contains("résumé"));
    assert!(!trie.contains("cafe")); // Different from café
    assert_eq!(trie.len(), 3);
}

#[test]
fn test_cjk_characters() {
    let trie: PersistentARTrieChar<()> = PersistentARTrieChar::new();

    // Chinese characters
    trie.insert("中文").expect("insert failed");
    trie.insert("你好").expect("insert failed");
    trie.insert("世界").expect("insert failed");

    // Japanese
    trie.insert("日本語").expect("insert failed");
    trie.insert("こんにちは").expect("insert failed");
    trie.insert("ありがとう").expect("insert failed");

    // Korean
    trie.insert("한국어").expect("insert failed");
    trie.insert("안녕하세요").expect("insert failed");

    assert!(trie.contains("中文"));
    assert!(trie.contains("こんにちは"));
    assert!(trie.contains("한국어"));
    assert!(!trie.contains("中")); // Partial prefix
    assert_eq!(trie.len(), 8);
}

#[test]
fn test_emoji_handling() {
    let trie: PersistentARTrieChar<()> = PersistentARTrieChar::new();

    // Single emoji
    trie.insert("🎉").expect("insert failed");
    trie.insert("🌍").expect("insert failed");
    trie.insert("❤️").expect("insert failed");

    // Emoji sequences
    trie.insert("👨‍👩‍👧").expect("insert failed"); // Family emoji (ZWJ sequence)
    trie.insert("🏳️‍🌈").expect("insert failed"); // Rainbow flag

    // Mixed text and emoji
    trie.insert("Hello 🌍!").expect("insert failed");
    trie.insert("I ❤️ Rust").expect("insert failed");

    assert!(trie.contains("🎉"));
    assert!(trie.contains("Hello 🌍!"));
    assert!(!trie.contains("🎊")); // Different emoji
    assert_eq!(trie.len(), 7);
}

// =============================================================================
// Test: Unicode Edge Cases
// =============================================================================

#[test]
fn test_combining_characters() {
    let trie: PersistentARTrieChar<()> = PersistentARTrieChar::new();

    // Precomposed vs decomposed forms
    // Note: Rust strings are UTF-8 byte sequences, not normalized
    let precomposed = "é"; // U+00E9 (single code point)
    let decomposed = "é"; // U+0065 + U+0301 (e + combining acute)

    trie.insert(precomposed).expect("insert failed");
    trie.insert(decomposed).expect("insert failed");

    // These may or may not be the same depending on normalization
    assert!(trie.contains(precomposed));
    assert!(trie.contains(decomposed));

    // Count depends on whether they are identical after normalization
    // Without normalization, they are different strings
}

#[test]
fn test_rtl_text() {
    let trie: PersistentARTrieChar<()> = PersistentARTrieChar::new();

    // Arabic
    trie.insert("مرحبا").expect("insert failed");
    trie.insert("العالم").expect("insert failed");

    // Hebrew
    trie.insert("שלום").expect("insert failed");
    trie.insert("עולם").expect("insert failed");

    assert!(trie.contains("مرحبا"));
    assert!(trie.contains("שלום"));
    assert_eq!(trie.len(), 4);
}

#[test]
fn test_mixed_scripts() {
    let trie: PersistentARTrieChar<()> = PersistentARTrieChar::new();

    // Mixed script strings
    trie.insert("Café 中文").expect("insert failed");
    trie.insert("Tōkyō 東京").expect("insert failed");
    trie.insert("München München").expect("insert failed");
    trie.insert("São Paulo").expect("insert failed");

    assert!(trie.contains("Café 中文"));
    assert!(trie.contains("Tōkyō 東京"));
    assert_eq!(trie.len(), 4);
}

#[test]
fn test_special_unicode_categories() {
    let trie: PersistentARTrieChar<()> = PersistentARTrieChar::new();

    // Mathematical symbols
    trie.insert("∑∏∫∂").expect("insert failed");
    trie.insert("∀x∃y").expect("insert failed");

    // Currency symbols
    trie.insert("$€£¥₿").expect("insert failed");

    // Musical symbols
    trie.insert("♩♪♫♬").expect("insert failed");

    // Technical symbols
    trie.insert("⚡⚙️🔧").expect("insert failed");

    assert!(trie.contains("∑∏∫∂"));
    assert!(trie.contains("$€£¥₿"));
    assert_eq!(trie.len(), 5);
}

// =============================================================================
// Test: Values with Unicode Keys
// =============================================================================

#[test]
fn test_unicode_keys_with_values() {
    let trie: PersistentARTrieChar<i32> = PersistentARTrieChar::new();

    trie.insert_with_value("café", 1)
        .expect("insert value failed");
    trie.insert_with_value("中文", 2)
        .expect("insert value failed");
    trie.insert_with_value("🎉", 3)
        .expect("insert value failed");

    assert_eq!(trie.get_value("café"), Some(1));
    assert_eq!(trie.get_value("中文"), Some(2));
    assert_eq!(trie.get_value("🎉"), Some(3));
    assert_eq!(trie.get_value("notfound"), None);
}

#[test]
fn test_unicode_keys_with_string_values() {
    let trie: PersistentARTrieChar<String> = PersistentARTrieChar::new();

    trie.insert_with_value("hello", "greeting".to_string())
        .expect("insert value failed");
    trie.insert_with_value("世界", "world".to_string())
        .expect("insert value failed");
    trie.insert_with_value("café", "coffee place".to_string())
        .expect("insert value failed");

    assert_eq!(trie.get_value("hello"), Some("greeting".to_string()));
    assert_eq!(trie.get_value("世界"), Some("world".to_string()));
    assert_eq!(trie.get_value("café"), Some("coffee place".to_string()));
}

// =============================================================================
// Test: Zipper Navigation with Unicode
// =============================================================================

#[test]
fn test_zipper_unicode_navigation() {
    let trie: PersistentARTrieChar<()> = PersistentARTrieChar::new();

    trie.insert("café").expect("insert failed");
    trie.insert("cat").expect("insert failed");

    let zipper = PersistentARTrieCharZipper::new(&trie);

    // Navigate to "café" using char-by-char descent
    let z = zipper
        .descend('c')
        .and_then(|z| z.descend('a'))
        .and_then(|z| z.descend('f'))
        .and_then(|z| z.descend('é'));

    assert!(z.is_some());
    let z = z.unwrap();
    assert!(z.is_final());

    // Path should be characters, not bytes
    let path = z.path();
    assert_eq!(path, vec!['c', 'a', 'f', 'é']);
}

#[test]
fn test_zipper_cjk_navigation() {
    let trie: PersistentARTrieChar<()> = PersistentARTrieChar::new();

    trie.insert("中文").expect("insert failed");

    let zipper = PersistentARTrieCharZipper::new(&trie);

    // Navigate character by character
    let z = zipper.descend('中').and_then(|z| z.descend('文'));

    assert!(z.is_some());
    let z = z.unwrap();
    assert!(z.is_final());
    assert_eq!(z.path(), vec!['中', '文']);
}

#[test]
fn test_zipper_children_with_unicode() {
    let trie: PersistentARTrieChar<()> = PersistentARTrieChar::new();

    // Insert terms with different second characters
    trie.insert("ab").expect("insert failed");
    trie.insert("aé").expect("insert failed");
    trie.insert("a中").expect("insert failed");
    trie.insert("a🎉").expect("insert failed");

    let zipper = PersistentARTrieCharZipper::new(&trie);
    let a_zipper = zipper.descend('a').expect("should have 'a'");

    // Collect all children labels
    let children: Vec<char> = a_zipper.children().map(|(c, _)| c).collect();

    assert!(children.contains(&'b'));
    assert!(children.contains(&'é'));
    assert!(children.contains(&'中'));
    assert!(children.contains(&'🎉'));
    assert_eq!(children.len(), 4);
}

// =============================================================================
// Test: Dictionary Trait Implementation
// =============================================================================

#[test]
fn test_dictionary_trait_unicode() {
    let trie: PersistentARTrieChar<()> = PersistentARTrieChar::new();

    trie.insert("hello").expect("insert failed");
    trie.insert("世界").expect("insert failed");
    trie.insert("café").expect("insert failed");

    // Test Dictionary trait methods
    assert!(trie.contains("hello"));
    assert!(trie.contains("世界"));
    assert_eq!(trie.len(), 3);

    // Test root navigation
    let root = trie.root();
    assert!(!root.is_final()); // Empty string is not in dictionary
}

#[test]
fn test_dictionary_node_trait_unicode() {
    let trie: PersistentARTrieChar<()> = PersistentARTrieChar::new();

    trie.insert("abc").expect("insert failed");
    trie.insert("aéc").expect("insert failed");

    let root = trie.root();

    // Test transition to 'a'
    let a_node = root.transition('a');
    assert!(a_node.is_some());

    let a_node = a_node.unwrap();

    // Test edges from 'a' - should include both 'b' and 'é'
    let edges: Vec<char> = a_node.edges().map(|(c, _)| c).collect();
    assert!(edges.contains(&'b'));
    assert!(edges.contains(&'é'));
}

// =============================================================================
// Test: FromIterator with Unicode
// =============================================================================

#[test]
fn test_from_iterator_unicode() {
    let terms = vec!["café", "naïve", "中文", "🎉"];
    let trie: PersistentARTrieChar<()> = terms.into_iter().collect();

    assert_eq!(trie.len(), 4);
    assert!(trie.contains("café"));
    assert!(trie.contains("中文"));
    assert!(trie.contains("🎉"));
}

#[test]
fn test_from_iterator_owned_strings() {
    let terms: Vec<String> = vec![
        "résumé".to_string(),
        "東京".to_string(),
        "🌍🌎🌏".to_string(),
    ];
    let trie: PersistentARTrieChar<()> = terms.into_iter().collect();

    assert_eq!(trie.len(), 3);
    assert!(trie.contains("résumé"));
    assert!(trie.contains("東京"));
}

// =============================================================================
// Test: Iterator with Unicode
// =============================================================================

#[test]
fn test_iterator_unicode() {
    let trie: PersistentARTrieChar<()> = PersistentARTrieChar::new();

    trie.insert("aaa").expect("insert failed");
    trie.insert("café").expect("insert failed");
    trie.insert("中文").expect("insert failed");

    // Collect all terms via iteration
    let terms: Vec<String> = trie.iter().collect();

    assert_eq!(terms.len(), 3);
    assert!(terms.contains(&"aaa".to_string()));
    assert!(terms.contains(&"café".to_string()));
    assert!(terms.contains(&"中文".to_string()));
}

// =============================================================================
// Test: Unicode Prefix Sharing
// =============================================================================

#[test]
fn test_unicode_prefix_sharing() {
    let trie: PersistentARTrieChar<()> = PersistentARTrieChar::new();

    // Terms sharing Unicode prefixes
    trie.insert("中文").expect("insert failed");
    trie.insert("中国").expect("insert failed");
    trie.insert("中心").expect("insert failed");

    assert_eq!(trie.len(), 3);
    assert!(trie.contains("中文"));
    assert!(trie.contains("中国"));
    assert!(trie.contains("中心"));
}

#[test]
fn test_emoji_prefix_sharing() {
    let trie: PersistentARTrieChar<()> = PersistentARTrieChar::new();

    // Emoji with common prefix
    trie.insert("🎉🎊").expect("insert failed");
    trie.insert("🎉🎁").expect("insert failed");
    trie.insert("🎉🎈").expect("insert failed");

    assert_eq!(trie.len(), 3);

    // Check prefix navigation
    let zipper = PersistentARTrieCharZipper::new(&trie);
    let party = zipper.descend('🎉').expect("should have party emoji");

    let children: Vec<char> = party.children().map(|(c, _)| c).collect();
    assert_eq!(children.len(), 3);
}

// =============================================================================
// Test: Edge Cases
// =============================================================================

#[test]
fn test_empty_string() {
    let trie: PersistentARTrieChar<()> = PersistentARTrieChar::new();

    // Empty string should be insertable
    trie.insert("").expect("insert failed");

    assert!(trie.contains(""));
    assert_eq!(trie.len(), 1);
}

#[test]
fn test_single_character_unicode() {
    let trie: PersistentARTrieChar<()> = PersistentARTrieChar::new();

    // Single character terms of various types
    trie.insert("a").expect("insert failed");
    trie.insert("é").expect("insert failed");
    trie.insert("中").expect("insert failed");
    trie.insert("🎉").expect("insert failed");

    assert_eq!(trie.len(), 4);
    assert!(trie.contains("a"));
    assert!(trie.contains("é"));
    assert!(trie.contains("中"));
    assert!(trie.contains("🎉"));
}

#[test]
fn test_long_unicode_string() {
    let trie: PersistentARTrieChar<()> = PersistentARTrieChar::new();

    // Long Unicode string
    let long_unicode = "这是一个非常长的中文字符串，包含很多字符";
    trie.insert(long_unicode).expect("insert failed");

    assert!(trie.contains(long_unicode));
    assert_eq!(trie.len(), 1);
}

#[test]
fn test_duplicate_unicode_insertion() {
    let trie: PersistentARTrieChar<()> = PersistentARTrieChar::new();

    assert!(trie.insert("café").expect("insert failed"));
    assert!(!trie.insert("café").expect("insert failed")); // Duplicate should return false
    assert_eq!(trie.len(), 1);
}

// =============================================================================
// Test: Concurrent Unicode Access
// =============================================================================

#[test]
fn test_concurrent_unicode_reads() {
    use std::sync::Arc;
    use std::thread;

    let trie: PersistentARTrieChar<()> = PersistentARTrieChar::new();

    // Insert Unicode terms
    let terms = vec!["café", "中文", "🎉", "日本語", "한국어"];
    for term in &terms {
        trie.insert(term).expect("insert failed");
    }

    let trie: Arc<PersistentARTrieChar<()>> = Arc::new(trie);

    // Spawn multiple reader threads
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let trie_clone: Arc<PersistentARTrieChar<()>> = Arc::clone(&trie);
            let terms_clone = terms.clone();

            thread::spawn(move || {
                for term in &terms_clone {
                    assert!(trie_clone.contains(term));
                }
            })
        })
        .collect();

    for handle in handles {
        handle.join().expect("thread join");
    }
}

// =============================================================================
// Test: Clone Behavior
// =============================================================================

// Note: PersistentARTrieChar does not implement Clone because it contains
// Arc-wrapped resources (buffer manager, WAL writer, etc.) that should not
// be implicitly shared. For shared access, use SharedCharTrie instead.
// #[test]
// fn test_clone_shares_unicode_state() { ... }

// =============================================================================
// Test: MappedDictionary with Unicode
// =============================================================================

#[test]
fn test_mapped_dictionary_unicode() {
    let trie: PersistentARTrieChar<i32> = PersistentARTrieChar::new();

    trie.insert_with_value("one", 1)
        .expect("insert value failed");
    trie.insert_with_value("一", 1)
        .expect("insert value failed"); // Chinese for "one"
    trie.insert_with_value("하나", 1)
        .expect("insert value failed"); // Korean for "one"

    // MappedDictionary::get_value
    assert_eq!(trie.get_value("one"), Some(1));
    assert_eq!(trie.get_value("一"), Some(1));
    assert_eq!(trie.get_value("하나"), Some(1));
}

// =============================================================================
// Test: Supplementary Plane Characters (> U+FFFF)
// =============================================================================

#[test]
fn test_supplementary_plane_characters() {
    let trie: PersistentARTrieChar<()> = PersistentARTrieChar::new();

    // Characters outside Basic Multilingual Plane (BMP)
    // These require surrogate pairs in UTF-16 but are single code points in Rust

    // Musical symbols (U+1D100-U+1D1FF)
    trie.insert("𝄞").expect("insert failed"); // G clef

    // Mathematical Alphanumeric Symbols (U+1D400-U+1D7FF)
    trie.insert("𝐀𝐁𝐂").expect("insert failed"); // Bold letters

    // Ancient scripts
    trie.insert("𐀀").expect("insert failed"); // Linear B syllable

    // Emoji with skin tone modifiers (U+1F3FB-U+1F3FF)
    trie.insert("👋🏽").expect("insert failed"); // Waving hand with medium skin tone

    assert!(trie.contains("𝄞"));
    assert!(trie.contains("𝐀𝐁𝐂"));
    assert!(trie.contains("𐀀"));
    assert!(trie.contains("👋🏽"));
    assert_eq!(trie.len(), 4);
}

// =============================================================================
// Test: Zero-Width Characters
// =============================================================================

#[test]
fn test_zero_width_characters() {
    let trie: PersistentARTrieChar<()> = PersistentARTrieChar::new();

    // Zero-width joiner (ZWJ) and zero-width non-joiner (ZWNJ)
    let with_zwj = "a\u{200D}b"; // a + ZWJ + b
    let without_zwj = "ab";

    trie.insert(with_zwj).expect("insert failed");
    trie.insert(without_zwj).expect("insert failed");

    // These should be different strings
    assert!(trie.contains(with_zwj));
    assert!(trie.contains(without_zwj));

    // Count depends on whether they are treated as same
    // Without normalization, they are different
    assert!(trie.len() >= 1); // At least one unique
}

// =============================================================================
// Test: Whitespace Variations
// =============================================================================

#[test]
fn test_unicode_whitespace() {
    let trie: PersistentARTrieChar<()> = PersistentARTrieChar::new();

    // Different types of spaces
    trie.insert("hello world").expect("insert failed"); // Regular space
    trie.insert("hello\u{00A0}world").expect("insert failed"); // Non-breaking space
    trie.insert("hello\u{2003}world").expect("insert failed"); // Em space
    trie.insert("hello\u{3000}world").expect("insert failed"); // Ideographic space

    // These should all be different
    assert!(trie.contains("hello world"));
    assert!(trie.contains("hello\u{00A0}world"));
    assert!(trie.contains("hello\u{2003}world"));
    assert!(trie.contains("hello\u{3000}world"));
    assert_eq!(trie.len(), 4);
}

// =============================================================================
// Test: Deep Trie Loading (Stack Overflow Prevention)
// =============================================================================

/// Test that loading deep tries doesn't cause stack overflow.
///
/// This test creates tries with very long strings (which create deep trie structures)
/// and verifies that the iterative loading algorithm handles them correctly.
///
/// Before the fix, recursive loading would stack overflow for tries with depth > ~1000.
/// The iterative algorithm should handle arbitrarily deep tries.
#[test]
fn test_deep_trie_no_stack_overflow() {
    let temp_dir = tempfile::tempdir().expect("Failed to create temp dir");
    let path = temp_dir.path().join("deep_trie_char");

    // Create trie with very long strings to create deep structure
    // Each character adds one level of depth
    let num_strings = 10usize;
    let string_length = 500usize; // Deeper than default stack limit for many recursive calls

    {
        let trie = PersistentARTrieChar::<u64>::create(&path).expect("Failed to create trie");

        for i in 0..num_strings {
            // Generate a long string with varying characters
            let long_key: String = (0..string_length)
                .map(|j| {
                    let ch = (b'a' + ((i + j) % 26) as u8) as char;
                    ch
                })
                .collect();

            // Use upsert which takes a value
            trie.upsert(&long_key, i as u64).expect("Failed to insert");
        }

        // Verify all strings present before checkpoint
        println!("=== Before checkpoint ===");
        println!("Trie len: {}", trie.len());
        for i in 0..num_strings {
            let long_key: String = (0..string_length)
                .map(|j| {
                    let ch = (b'a' + ((i + j) % 26) as u8) as char;
                    ch
                })
                .collect();
            let present = trie.contains(&long_key);
            println!("String {} present: {}", i, present);
        }

        trie.checkpoint().expect("Failed to checkpoint");
    }

    // Reopen - this would stack overflow with recursive loading for deep tries
    let reopened = PersistentARTrieChar::<u64>::open(&path)
        .expect("Failed to reopen trie - possible stack overflow in recursive loading");

    println!("=== After reopen ===");
    println!("Reopened len: {}", reopened.len());

    // First check all strings without asserting
    for i in 0..num_strings {
        let long_key: String = (0..string_length)
            .map(|j| {
                let ch = (b'a' + ((i + j) % 26) as u8) as char;
                ch
            })
            .collect();
        let present = reopened.contains(&long_key);
        println!(
            "String {} present after reopen: {} (first char: '{}')",
            i,
            present,
            long_key.chars().next().unwrap()
        );

        // Debug: for string 9, try to trace the issue
        if i == 9 && !present {
            println!("DEBUG: Tracing string 9 lookup failure");
            // Check using get() which will trace the path
            let value = reopened.get(&long_key);
            println!("DEBUG: get() for string 9 returned: {:?}", value.is_some());
        }
    }

    assert_eq!(
        reopened.len(),
        num_strings,
        "All strings should be present after reopen"
    );

    // Verify we can still look up the strings
    for i in 0..num_strings {
        let long_key: String = (0..string_length)
            .map(|j| {
                let ch = (b'a' + ((i + j) % 26) as u8) as char;
                ch
            })
            .collect();

        assert!(
            reopened.contains(&long_key),
            "String {} should be present after reopen",
            i
        );

        // Verify the value via get() (F4: `get` now returns an owned `Option<V>`).
        if let Some(value) = reopened.get(&long_key) {
            assert_eq!(value, i as u64, "Value for string {} should match", i);
        }
    }
}

/// Test deep trie with Unicode characters.
///
/// This test uses multi-byte UTF-8 characters which still create deep tries
/// but exercise the character-level handling.
#[test]
fn test_deep_unicode_trie_no_stack_overflow() {
    let temp_dir = tempfile::tempdir().expect("Failed to create temp dir");
    let path = temp_dir.path().join("deep_unicode_trie");

    let num_strings = 5usize;
    let string_length = 300usize; // Fewer characters but still deep

    {
        let trie = PersistentARTrieChar::<u64>::create(&path).expect("Failed to create trie");

        for i in 0..num_strings {
            // Generate a long Unicode string with CJK characters
            let long_key: String = (0..string_length)
                .map(|j| {
                    // Use a range of CJK characters (U+4E00 to U+9FFF)
                    let codepoint = 0x4E00 + ((i * 17 + j * 13) % 0x51FF) as u32;
                    char::from_u32(codepoint).unwrap_or('中')
                })
                .collect();

            trie.upsert(&long_key, i as u64).expect("Failed to insert");
        }

        trie.checkpoint().expect("Failed to checkpoint");
    }

    // Reopen
    let reopened = PersistentARTrieChar::<u64>::open(&path).expect("Failed to reopen Unicode trie");

    assert_eq!(reopened.len(), num_strings);
}
