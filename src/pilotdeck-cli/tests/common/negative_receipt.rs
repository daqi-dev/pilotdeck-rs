use serde_json::Value;
use std::{fs, io::Write, path::Path};

// Bind the negative verdict to the current libtest and exact assertion; arbitrary
// server/CLI text cannot forge a kill. Uses negative_suite.ReceiptRun's protocol.
pub(crate) fn verify(condition: bool, assertion: &str) {
    if condition {
        return;
    }
    if let Ok(config) = std::env::var("PDRS_NEGATIVE_RECEIPTS") {
        let config: Value = serde_json::from_str(&config).expect("#1058 receipt configuration");
        let thread = std::thread::current();
        let name = thread.name().expect("#1058 named test");
        for target in config["targets"].as_array().expect("#1058 receipt targets") {
            if target["test"] == name && target["assertion"] == assertion {
                let path = Path::new(target["path"].as_str().expect("#1058 receipt path"));
                let temporary = path.with_extension("tmp");
                let mut receipt = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&temporary)
                    .expect("#1058 fresh receipt");
                receipt
                    .write_all(
                        config["nonce"]
                            .as_str()
                            .expect("#1058 receipt nonce")
                            .as_bytes(),
                    )
                    .expect("#1058 receipt write");
                fs::rename(temporary, path).expect("#1058 atomic receipt publication");
            }
        }
    }
    panic!("{assertion}");
}
