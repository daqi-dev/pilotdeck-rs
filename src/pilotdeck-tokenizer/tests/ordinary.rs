use pilotdeck_protocol::JsString;
use pilotdeck_tokenizer::count_tokens;
#[test]
fn empty_is_zero() {
    assert_eq!(count_tokens(&JsString::default()).unwrap(), 0);
}
#[test]
fn original_rank_single_word_and_two_pieces() {
    // Original o200k dictionary contains hello24912 and space-world2375.
    assert_eq!(count_tokens(&"hello".into()).unwrap(), 1);
    assert_eq!(count_tokens(&"hello world".into()).unwrap(), 2);
}
#[test]
fn three_digit_groups_are_preserved() {
    assert_eq!(count_tokens(&"12345".into()).unwrap(), 2);
}
#[test]
fn ordinary_special_spellings_are_not_special_tokens() {
    for text in ["<|endoftext|>", "<|endofprompt|>"] {
        assert!(count_tokens(&text.into()).unwrap() > 1);
    }
}
#[test]
fn surrounding_whitespace_is_not_trimmed() {
    assert_eq!(count_tokens(&" hello ".into()).unwrap(), 2);
    assert_eq!(count_tokens(&"hello".into()).unwrap(), 1);
}
#[test]
fn lone_surrogates_are_textencoder_replacement_not_erased() {
    assert_eq!(
        count_tokens(&JsString::from_utf16(vec![0xd800])).unwrap(),
        1
    );
    assert_eq!(
        count_tokens(&JsString::from_utf16(vec![0xdc00])).unwrap(),
        1
    );
    assert_eq!(
        count_tokens(&JsString::from_utf16(vec![0xd800, 0x78])).unwrap(),
        count_tokens(&"\u{fffd}x".into()).unwrap()
    );
}
#[test]
fn paired_surrogate_matches_same_unicode_scalar() {
    assert_eq!(
        count_tokens(&JsString::from_utf16(vec![0xd83d, 0xde00])).unwrap(),
        count_tokens(&"😀".into()).unwrap()
    );
}
#[test]
fn multiple_scripts_and_combining_marks_are_not_normalized() {
    for text in [
        "你好世界",
        "مرحبا بالعالم",
        "नमस्ते",
        "e\u{301}",
        "é",
        "\u{1c89}\u{1c8a}",
    ] {
        assert!(count_tokens(&text.into()).unwrap() > 0);
    }
}
#[test]
fn concurrency_uses_immutable_bpe_and_equivalent_regex_clones() {
    let workers: Vec<_> = (0..8)
        .map(|_| {
            std::thread::spawn(|| {
                for _ in 0..16 {
                    assert_eq!(count_tokens(&"hello world".into()).unwrap(), 2);
                    assert_eq!(
                        count_tokens(&JsString::from_utf16(vec![0xd800])).unwrap(),
                        1
                    );
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
}
#[test]
fn bounded_adversarial_dense_text_is_deterministic_and_not_zero() {
    let dense = JsString::from("x".repeat(16384));
    let first = count_tokens(&dense).unwrap();
    assert!(first > 0);
    assert_eq!(count_tokens(&dense).unwrap(), first);
}
