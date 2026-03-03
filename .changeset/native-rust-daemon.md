---
"agent-browser": minor
---

Added experimental native Rust daemon (`--native` flag, `AGENT_BROWSER_NATIVE=1` env, or `"native": true` in config). The native daemon communicates with Chrome directly via CDP, eliminating Node.js and Playwright dependencies. Supports 150+ commands with full parity to the default Node.js daemon. Includes WebDriver backend for Safari/iOS, CDP protocol codegen, request tracking, frame context management, and comprehensive e2e and parity tests.
