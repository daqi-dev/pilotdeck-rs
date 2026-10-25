use super::*;
use tokio::io::AsyncWriteExt;

fn oracle() -> serde_json::Value {
    serde_json::from_str(include_str!(
        "../../../tests/fixtures/hooks-output/oracle.json"
    ))
    .unwrap()
}

#[test]
fn output_limit_matches_node_and_rejected_chunks_do_not_poison_later_appends() {
    let golden = oracle();
    assert_eq!(
        LocalShellPort::default().output_limit,
        golden["maxStringLength"].as_u64().unwrap() as usize
    );
    // Same relative chunks as the actual V8 boundary probe, scaled to ten
    // units. The other stream remains independent, including at its cap.
    let mut streams = Vec::new();
    for case in golden["boundaries"].as_array().unwrap() {
        let stream = case["stream"].as_str().unwrap();
        streams.push(stream);
        assert_eq!(case["settledAfterOverflow"], false);
        assert_eq!(case["failures"], golden["hookCatch"]["failures"]);
        let mut output = PipeOutput::default();
        let mut other = PipeOutput::default();
        output.append(10, "123456789");
        assert!(!output.truncated);
        output.append(10, "zz");
        assert!(output.truncated);
        assert_eq!(output.bytes, b"123456789");
        output.append(10, "y");
        other.append(10, "ok");
        assert_eq!(output.bytes, b"123456789y");
        assert_eq!(output.units, 10);
        let (capped, uncapped) = if stream == "stdout" {
            ("stdoutLength", "stderrLength")
        } else {
            ("stderrLength", "stdoutLength")
        };
        assert_eq!(case[capped], golden["maxStringLength"]);
        assert_eq!(other.units as u64, case[uncapped]);
        assert!(!other.truncated);
    }
    assert_eq!(streams, ["stdout", "stderr"]);
    let mut unicode = PipeOutput::default();
    unicode.append(3, "😀a");
    assert_eq!(unicode.bytes, "😀a".as_bytes());
    assert_eq!(unicode.units, 3);
    unicode.append(3, "b");
    assert_eq!(unicode.bytes, "😀a".as_bytes());
    assert!(unicode.truncated);
}

#[tokio::test]
async fn output_decoder_flush_is_capped_and_split_unicode_counts_units() {
    for case in oracle()["decoding"].as_array().unwrap() {
        let mut decoder = Utf8Decoder::default();
        for (chunk, expected) in case["chunks"]
            .as_array()
            .unwrap()
            .iter()
            .zip(case["decoded"].as_array().unwrap())
        {
            let bytes: Vec<_> = chunk
                .as_str()
                .unwrap()
                .as_bytes()
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
                .collect();
            assert_eq!(decoder.decode(&bytes), expected.as_str().unwrap());
        }
        assert_eq!(decoder.flush(), case["tail"].as_str().unwrap());
    }
    let mut decoder = Utf8Decoder::default();
    let mut output = PipeOutput::default();
    for chunk in [&b"\xef\xbb"[..], &b"\xbf\xf0\x9f"[..], &b"\x98\x80a"[..]] {
        output.append(3, &decoder.decode(chunk));
    }
    assert_eq!(output.bytes, "😀a".as_bytes());
    assert_eq!(output.units, 3);
    assert!(!output.truncated);
    // Real reader EOF flush adds U+FFFD for an incomplete UTF-8 sequence.
    for (limit, expected, truncated) in [(1, "a", true), (2, "a�", false)] {
        let output = read_pipe(Some(&b"a\xf0"[..]), limit).await;
        assert_eq!(output.bytes, expected.as_bytes());
        assert_eq!(output.truncated, truncated);
    }
}

#[tokio::test]
async fn output_64_mib_is_drained_with_bounded_retained_capacity() {
    const LIMIT: usize = 8192;
    let (mut writer, reader) = tokio::io::duplex(LIMIT);
    let total = oracle()["large"][0]["bytes"].as_u64().unwrap() as usize;
    let send = async move {
        let chunk = [0; 64 * 1024];
        for _ in 0..total / chunk.len() {
            writer.write_all(&chunk).await.unwrap();
        }
    };
    let (output, ()) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(read_pipe(Some(reader), LIMIT), send)
    })
    .await
    .expect("reader must drain to EOF, not leave the producer blocked");
    // UTF-8 <= 3 bytes per UTF-16 unit; Vec growth adds at most a factor 2.
    // This measures retained allocation on the real reader, not process RSS.
    eprintln!(
        "drained={total}, retained={}, capacity={}, bound={}",
        output.bytes.len(),
        output.bytes.capacity(),
        6 * LIMIT
    );
    assert!(output.bytes.capacity() <= 6 * LIMIT);
    assert_eq!(output.bytes.len(), LIMIT);
    assert!(output.units <= LIMIT);
    assert!(output.truncated);
}
