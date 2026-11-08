fn main() {
    let label = pilotdeck_protocol::JsString::from("independent capability consumer");
    assert_eq!(pilotdeck_budget::MULTIMEDIA_TOKENS, 2_000);
    let _protected = pilotdeck_context::ProtectedNames::from_options(None);
    let _stream = pilotdeck_model_anthropic::anthropic::StreamState::default();
    let _ = label;
    println!("budget/context/anthropic APIs loaded without gateway or Kernel");
}
