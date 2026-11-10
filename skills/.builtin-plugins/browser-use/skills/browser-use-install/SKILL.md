---
name: browser-use-install
description: Use before browser-use, Playwright MCP, web browsing, browser automation, or when browser-use fails with a missing browser, missing executable, "Executable doesn't exist", or Chrome/Chromium install error in PilotDeck Desktop.
---

# Browser-Use Install

PilotDeck Desktop ships browser-use without a bundled Chrome browser. When browser automation is needed, install the Playwright Chrome for Testing browser into the user's writable PilotDeck data directory, then retry the browser-use tool.

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

## After Install

Retry the browser-use request after the install succeeds. If the current MCP session still reports the old missing-browser error, tell the user to send the same request once more so PilotDeck can start a fresh browser-use session.
