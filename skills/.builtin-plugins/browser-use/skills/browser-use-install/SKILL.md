---
name: browser-use-install
description: Use before browser-use, Playwright MCP, web browsing, browser automation, or when browser-use fails with a missing browser, missing executable, "Executable doesn't exist", or Chrome/Chromium install error in PilotDeck Desktop.
---

# Browser-Use Install

The Rust source generation provides the browser-use integration and expects an existing Playwright MCP SDK and browser. Locate those dependencies, configure their absolute paths, and verify an actual browser-use launch before treating the integration as ready.

## Install

The Rust source generation keeps Playwright MCP as a separately prepared third-party SDK. It does not ship the old `dist/src/extension/plugins/builtin/browser-use/scripts/install-browser.mjs` entrypoint. Do not run that missing path in a Rust source installation.

If an existing SDK package and browser are available, configure their absolute executable paths in the project's `.pilotdeck/mcp.json` (or the intended PilotDeck data directory's `mcp.json`). Merge the `browser-use` entry with other configured servers:

```json
{
  "mcpServers": {
    "browser-use": {
      "command": "/absolute/path/to/node",
      "args": [
        "/absolute/path/to/node_modules/@playwright/mcp/cli.js",
        "--browser", "chromium",
        "--executable-path", "/absolute/path/to/chrome-or-chromium",
        "--allow-unrestricted-file-access"
      ],
      "perSession": true
    }
  }
}
```

Alternatively, `PILOTDECK_RUNTIME_ROOT` can point to an existing third-party SDK package root containing `node_modules/@playwright/mcp/cli.js` before the Gateway starts. This discovers the SDK, but does not install a browser. Confirm that browser-use can actually launch before treating it as ready.

## Mirrors And Offline

For an offline Rust source installation, supply the existing SDK and browser paths above. This path performs no automatic downloads. If either dependency is absent, obtain an approved local dependency installation first; do not claim that configuring the MCP entry installed it.

Separately packaged TypeScript desktop runtimes retain their own browser installer and mirror/archive settings. Use those runtime-specific commands only when their referenced installer actually exists in that runtime.

## After Configuration

Retry the browser-use request after the configured SDK and browser launch succeeds. If the current MCP session still reports the old missing-browser error, tell the user to send the same request once more so PilotDeck can start a fresh browser-use session.
