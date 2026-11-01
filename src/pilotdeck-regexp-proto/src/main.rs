//! Small repeatable artifact for release size / RSS measurements (no user JS).
use pilotdeck_regexp_proto::{Engine, Limits};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut engine = Engine::new(Limits::default())?;
    let pattern: Vec<u16> = "^(?<word>a+)\\k<word>$".encode_utf16().collect();
    let input: Vec<u16> = "aaaaaaaa".encode_utf16().collect();
    let mut matches = 0;
    for _ in 0..100 {
        matches += usize::from(
            engine
                .request(10_000)
                .exec(&pattern, "du", &input, 0)?
                .matched
                .is_some(),
        );
    }
    println!("#1265 requests=100 matches={matches}");
    Ok(())
}
