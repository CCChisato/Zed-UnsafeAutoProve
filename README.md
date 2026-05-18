# Zed-UnsafeAutoProve

[![License: AGPL v3](https://img.shields.io/badge/License-AGPL_v3-blue.svg)](LICENSE-AGPL)

> **⚠️ Warning: Unsafe & Experimental**  
> This is a heavily modified fork of [Zed](https://zed.dev) with **auto-approve (all tools bypass confirmation)**, auto-summary, auto-cycle heartbeat, and more.  
> **Use at your own risk.** Not recommended for production or sensitive projects.

A fork of Zed — a high-performance, multiplayer code editor from the creators of [Atom](https://github.com/atom/atom) and [Tree-sitter](https://github.com/tree-sitter/tree-sitter) — customized for **fully autonomous agentic workflows** using cheap/abundant LLM APIs like **DeepSeek**.

---

## ✨ Custom Features

### 🛡️ Auto-Approve (Unsafe)
All tool call authorization (file read/write, terminal, MCP, etc.) is **automatically granted without user confirmation**. Humans are removed from the approval loop entirely.

- `authorize()` and `authorize_third_party_tool()` return `Task::ready(Ok(()))` unconditionally
- Auto-approve can be **toggled per thread** via the UI button (✓ icon)
- Configurable **max auto-approval count per session** (0 = unlimited)
- Right-click the auto-approve button to adjust limit and reset counter

### 📝 Auto-Summary
When token usage reaches a configurable threshold, automatically generates a summary and creates a new thread — enabling **infinite-length conversations**.

- Toggle via UI button (thread-from-summary icon)
- Configurable threshold (right-click to adjust) — default: 75% of model's max tokens
- Auto-inherited settings when creating new threads from summary
- Auto-sends "continue" (or custom prompt) to the new thread

### 🔄 Auto-Cycle (Heartbeat)
Periodically sends a configurable prompt to keep the agent running autonomously — ideal for long-running, multi-step tasks.

- Configurable **interval** (default: 30s)
- Configurable **prompt** (default: "continue")
- Only fires when the thread is in `Idle` state and message editor is empty

### 🖼️ Custom Background Images
Set a background image behind the editor content or the entire workspace.

- **Settings:**
  - `editor_background_image` (path to image file)
  - `editor_background_opacity` (0.0–1.0, default: 1.0)
  - `global_background_image` (path to image file)
  - `global_background_opacity` (0.0–1.0, default: 0.5)
- Supports all image formats supported by GPUI
- Uses `object-fit: cover` sizing

### 🤖 Collaborative Subagent Tool (Claw Integration)
Built-in integration with [Claw-Code-DeepSeek-Default-Proxy](https://github.com/CCChisato/Claw-Code-DeepSeek-Default-Proxy).

- **`spawn_agent`** — non-blocking subagent dispatch (returns immediately with a `session_id`)
- **`check_subagent_status`** — poll subagent results by `session_id` or scan all pending notifications
- Automatic notification injection — completed subagent results are injected as user messages on the next agent turn
- Supports both **Zed native subagents** (`.native.json`) and **Claw subagents** (`.notification.json`)
- Native subagent writes output to `~/.claw/sessions/notifications/<session_id>.native.json`
- Claw subagent integration via `claw subagent spawn/status/list/batch` CLI commands

### 🧩 Non-Blocking Subagents
Unlike upstream Zed's blocking `spawn_agent`, our version returns the `session_id` immediately and runs the subagent in the background. Results are collected asynchronously.

---

## ⚠️ Important Caveats

- **Not compatible with upstream** — significant refactoring in thread/authorization/UI layers
- **Incompatible with Zed's cloud/collaboration features** — may not compile or work correctly
- **Not fully tested** — many features are experimental and may contain bugs
- **Auto-approve bypasses all security** — the AI can read/write any file, execute any terminal command
- Built for **DeepSeek / cheap API** use cases — designed for rough automation, not precision work
- **`target/` directory is excluded** — delete before building

---

## 🚀 Getting Started

### Prerequisites

Same as upstream Zed. See:
- [Building Zed for macOS](docs/src/development/macos.md)
- [Building Zed for Linux](docs/src/development/linux.md)
- [Building Zed for Windows](docs/src/development/windows.md)

### Quick Start

```bash
# Clone (if you haven't already)
git clone --recursive https://github.com/CCChisato/Zed-UnsafeAutoProve.git
cd Zed-UnsafeAutoProve

# Build (this will take a while)
cargo build --release -p zed

# Run
./target/release/zed
```

### Configure Background Image

Add to your `settings.json`:

```json
{
  "editor_background_image": "/path/to/your/image.png",
  "editor_background_opacity": 0.3,
  "global_background_image": "/path/to/your/background.png",
  "global_background_opacity": 0.5
}
```

---

## 🔧 Architecture of Changes

| Area | Files Changed | Description |
|------|--------------|-------------|
| **Auto-Approve** | `crates/agent/src/thread.rs` | `authorize()` and `authorize_third_party_tool()` return `Ok(())` unconditionally |
| **Auto-Approve UI** | `crates/agent_ui/src/conversation_view/thread_view.rs` | Toggle button, right-click menu, counter |
| **Auto-Approve Logic** | `crates/agent_ui/src/conversation_view.rs` | Auto-approve on `ToolAuthorizationRequested` event |
| **Auto-Summary** | `crates/agent_ui/src/conversation_view/thread_view.rs` | Auto-summary toggle, threshold config |
| **Auto-Summary Bridge** | `crates/agent_ui/src/conversation_view.rs` | `AutoSummaryPending` struct + global bridge for settings inheritance |
| **Auto-Cycle** | `crates/agent_ui/src/conversation_view/thread_view.rs` | Timer-based periodic auto-send |
| **Background Image** | `crates/editor/src/editor_settings.rs`, `crates/editor/src/element.rs`, `crates/workspace/src/workspace.rs`, `crates/workspace/src/workspace_settings.rs`, `crates/settings_content/src/editor.rs`, `crates/settings_content/src/workspace.rs`, `assets/settings/default.json` | Custom `img` element with object-fit cover |
| **Subagent Notifier** | `crates/agent/src/subagent_notifier.rs` | Polls `~/.claw/sessions/notifications/` for completed subagents |
| **CheckSubagentStatus** | `crates/agent/src/tools/check_subagent_status_tool.rs` | New tool for polling subagent results |
| **Non-Blocking Spawn** | `crates/agent/src/tools/spawn_agent_tool.rs` | Returns `session_id` immediately, runs background |
| **Action Definitions** | `crates/agent_ui/src/agent_ui.rs` | `ToggleAutoApprove`, `SetAutoApproveLimit` actions |

---

## 📜 License

This project is a fork of [Zed](https://github.com/zed-industries/zed), originally licensed under AGPL v3 / Apache 2.0 / GPL v3.

- **Upstream Zed**: © Zed Industries, Inc. — [AGPL v3](LICENSE-AGPL), [Apache 2.0](LICENSE-APACHE), [GPL v3](LICENSE-GPL)
- **Modifications**: © CCChisato — same licenses as upstream

---

## 🙏 Credits

- [Zed](https://zed.dev) — the amazing upstream editor
- [Claw-Code-DeepSeek-Default-Proxy](https://github.com/CCChisato/Claw-Code-DeepSeek-Default-Proxy) — subagent integration proxy
- Built with ❤️ for autonomous coding workflows
