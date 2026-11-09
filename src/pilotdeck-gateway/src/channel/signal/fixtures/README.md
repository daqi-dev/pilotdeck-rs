# Signal direct-fetch diagnostic

capture.mjs reads fixed cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85 Git sources and records their SHA256. Unlike SDK-backed channels, this captures the actual baseline fetch path against an owned 127.0.0.1 SSE/POST mock. UTF8 is deliberately split inside a code point; the group message retains the baseline sender-recipient mapping. Stop must cause the real receive socket to close before output. No accounts, external service, baseline edits or new packages.

The committed oracle-diagnostic.json was actually observed on cb8 Node22.22.1/ICU78.2 with PDRS_DIAGNOSTIC=1 and formalAcceptance:false. Default capture requires22.22.0/78.2; frozen prerequisites are unchanged. Rust tests compare the fixed TS A7 request projection and independently exercise real fragmented chunked HTTP, response POST and stop socket closure. Rust tests have been written but not executed as of this batch; this is not #1337 completion.

negative_controls.py includes three compile-then-runtime mutations, first verifies the unmodified baseline, restores original source bytes in finally, and rebuilds/tests the restored original. It requires a disposable isolated worktree and caller-supplied JSON argv, and must not run concurrently with writers/builders. It has not been run. Formal double-platform checks, interaction/reload/failure matrix and independent reviews remain required.
