# computer-use-py (MCP server)

Minimal Python + pyautogui MCP server used to smoke-test Atlas via desktop
automation from the GitHub Copilot CLI or any other MCP client. Sister to
the stock nut-js-based `computer-use` server; useful when nut-js has trouble
delivering synthetic keys to a specific app (some Slint / Skia apps hit this).

## Tools

| Tool | Purpose |
| ---- | ------- |
| `take_screenshot` | PNG of the whole virtual screen. |
| `get_cursor_position` | `{x, y}` in logical pixels. |
| `screen_size` | `{width, height}` in logical pixels. |
| `move_mouse_to(x, y, duration_ms?)` | Move the mouse. |
| `left_click(x?, y?)` | Left-click, optionally at coords. |
| `double_click(x?, y?)` | Double-left-click. |
| `right_click(x?, y?)` | Right-click. |
| `type_text(text, interval_ms?)` | Type ASCII characters. |
| `send_keybind(keys)` | Press a shortcut like `cmd+d` / `cmd+shift+p` / `f5`. |
| `scroll(dy, dx?, x?, y?)` | Scroll wheel; +dy = down, +dx = right. |
| `drag_to(x, y, duration_ms?, button?)` | Press-and-drag from current position. |

## Prerequisites

- `uv` (recommended) or Python ≥ 3.10 with `pip`.
- macOS Accessibility, Screen Recording, and Input Monitoring permissions
  granted to whichever Python interpreter runs the server (System Settings
  → Privacy & Security).

## Running standalone (smoke test)

```bash
uv run tools/computer-use-mcp/server.py
```

The server speaks MCP over stdio. Without a client connected it will just
wait; hit `Ctrl-C` to exit.

## Register with the Copilot CLI

Add to your Copilot MCP config (`~/.copilot/mcp-config.json`, or the file
your `copilot mcp add` command edits — check the CLI docs):

```json
{
  "mcpServers": {
    "computer-use-py": {
      "command": "uv",
      "args": [
        "run",
        "--script",
        "/absolute/path/to/atlas/tools/computer-use-mcp/server.py"
      ]
    }
  }
}
```

Or in a single command (if your CLI version supports it):

```bash
copilot mcp add computer-use-py -- uv run --script \
  $(pwd)/tools/computer-use-mcp/server.py
```

Restart the Copilot CLI session so the new server is discovered.

## Notes on macOS keystroke delivery

Slint / Skia windows sometimes ignore keystrokes sent via the accessibility
API used by some automation frameworks. pyautogui uses
`Quartz.CGEventCreateKeyboardEvent`, which delivers events through the same
path as physical keyboards and works reliably against Slint apps in local
testing.

`cmd+d` and other macOS shortcuts are aliased automatically — see the
`_KEY_ALIASES` table at the top of `server.py`.
