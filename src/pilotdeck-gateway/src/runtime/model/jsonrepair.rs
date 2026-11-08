fn units(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}
use pilotdeck_model_anthropic::jsonrepair::{MAX_DEPTH, RepairError, jsonrepair};
#[cfg(test)]
#[path = "jsonrepair_tests.rs"]
mod replay;

#[cfg(test)]
mod tests {
    use super::*;

    fn repair(text: &str) -> Result<String, RepairError> {
        jsonrepair(&units(text)).map(|out| String::from_utf16(&out).unwrap())
    }

    #[test]
    fn depth_limit_fails_instead_of_overflowing_the_stack() {
        let deep = "[".repeat(MAX_DEPTH + 5);
        assert!(repair(&deep).is_err());
        let ok = "[".repeat(MAX_DEPTH - 1);
        assert_eq!(
            repair(&ok).unwrap(),
            format!("{ok}{}", "]".repeat(MAX_DEPTH - 1))
        );
        // Runs within a 2 MiB thread (tokio's default worker stack).
        let handle = std::thread::Builder::new()
            .stack_size(2 << 20)
            .spawn(move || {
                let nested = "{\"a\":[".repeat(MAX_DEPTH / 2 - 1);
                repair(&nested).is_ok()
            })
            .unwrap();
        assert!(handle.join().unwrap());
    }
}
