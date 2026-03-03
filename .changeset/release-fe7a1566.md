---
"agent-browser": patch
---

Improved Chrome launch reliability by automatically detecting containerized environments (Docker, Podman, Kubernetes) and enabling --no-sandbox when needed. Added support for discovering Playwright-installed Chromium browsers and enhanced error messages with helpful diagnostics when Chrome fails to launch.
