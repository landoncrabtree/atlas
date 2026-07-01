# /// script
# requires-python = ">=3.10"
# dependencies = [
#     "mcp>=1.2",
#     "pyautogui>=0.9.54",
#     "pyobjc-framework-Quartz; sys_platform == 'darwin'",
#     "pyobjc-core; sys_platform == 'darwin'",
#     "Pillow>=10",
# ]
# ///
"""Computer-use MCP server (Python + pyautogui).

A minimal Model Context Protocol server exposing basic desktop-automation
primitives (screenshot, click, type, keybind, scroll, move) for use with the
GitHub Copilot CLI or any other MCP client.

Why a second computer-use MCP?
    The stock nut-js-based server occasionally has trouble routing synthetic
    keystrokes to some macOS apps (notably Slint apps). pyautogui uses
    Quartz CGEvent under the hood on macOS, which delivers events through
    the same path as physical hardware and tends to work more reliably.

Run standalone (for smoke-testing):
    uv run tools/computer-use-mcp/server.py

Register with the Copilot CLI (one-off):
    See tools/computer-use-mcp/README.md for the exact command.

macOS permissions:
    Whichever Python interpreter runs this script needs:
      * System Settings → Privacy & Security → Accessibility  → ON
      * System Settings → Privacy & Security → Screen Recording → ON
      * System Settings → Privacy & Security → Input Monitoring → ON
    Grant permissions to the *python* / *uv* binary that shows up in the
    corresponding permission prompt.
"""

from __future__ import annotations

import io
import logging
import sys
from typing import Optional

# ── pyautogui setup ────────────────────────────────────────────────────────
# Disable the corner-triggered abort; we call it programmatically.
import pyautogui  # noqa: E402

pyautogui.FAILSAFE = False
# Small default delay so target apps have time to react between events.
pyautogui.PAUSE = 0.03

# ── MCP setup ──────────────────────────────────────────────────────────────
from mcp.server.fastmcp import FastMCP, Image  # noqa: E402

# Everything the server logs goes to stderr so stdout stays a clean JSON-RPC
# pipe for the MCP client.
logging.basicConfig(
    level=logging.INFO,
    stream=sys.stderr,
    format="[computer-use-py] %(levelname)s %(message)s",
)
log = logging.getLogger("computer-use-py")

mcp = FastMCP("computer-use-py")


# ── Key aliasing ───────────────────────────────────────────────────────────
# Map friendly names to whatever pyautogui expects. pyautogui uses its own
# KEYBOARD_KEYS set; on macOS Cmd is spelled "command", Option is "option",
# etc. We accept the common shorthands and normalise here.
_KEY_ALIASES = {
    "cmd": "command",
    "meta": "command",
    "super": "command",
    "win": "command",
    "opt": "option",
    "alt": "option",
    "control": "ctrl",
    "esc": "escape",
    "return": "enter",
    "del": "delete",
    "pgup": "pageup",
    "pgdn": "pagedown",
    "pgdown": "pagedown",
    "arrowup": "up",
    "arrowdown": "down",
    "arrowleft": "left",
    "arrowright": "right",
}


def _normalize_key(key: str) -> str:
    """Return the pyautogui-canonical name for a single key token."""
    lo = key.lower().strip()
    return _KEY_ALIASES.get(lo, lo)


# ── Tools ──────────────────────────────────────────────────────────────────
@mcp.tool()
def take_screenshot() -> Image:
    """Capture the entire virtual screen and return it as a PNG image."""
    img = pyautogui.screenshot()
    buf = io.BytesIO()
    img.save(buf, format="PNG")
    log.info("take_screenshot size=%dx%d bytes=%d", img.width, img.height, buf.tell())
    return Image(data=buf.getvalue(), format="png")


@mcp.tool()
def get_cursor_position() -> dict:
    """Return the current (x, y) cursor position in logical screen pixels."""
    x, y = pyautogui.position()
    return {"x": int(x), "y": int(y)}


@mcp.tool()
def move_mouse_to(x: int, y: int, duration_ms: int = 0) -> str:
    """Move the mouse to (x, y). `duration_ms` > 0 animates the move."""
    pyautogui.moveTo(x, y, duration=max(0.0, duration_ms) / 1000.0)
    log.info("move_mouse_to x=%d y=%d dur_ms=%d", x, y, duration_ms)
    return "ok"


@mcp.tool()
def left_click(x: Optional[int] = None, y: Optional[int] = None) -> str:
    """Left-click at (x, y), or at the current cursor position if omitted."""
    if x is not None and y is not None:
        pyautogui.click(x, y)
        log.info("left_click x=%d y=%d", x, y)
    else:
        pyautogui.click()
        log.info("left_click (current pos)")
    return "ok"


@mcp.tool()
def double_click(x: Optional[int] = None, y: Optional[int] = None) -> str:
    """Double-left-click at (x, y), or at the current cursor position if omitted."""
    if x is not None and y is not None:
        pyautogui.doubleClick(x, y)
        log.info("double_click x=%d y=%d", x, y)
    else:
        pyautogui.doubleClick()
        log.info("double_click (current pos)")
    return "ok"


@mcp.tool()
def right_click(x: Optional[int] = None, y: Optional[int] = None) -> str:
    """Right-click at (x, y), or at the current cursor position if omitted."""
    if x is not None and y is not None:
        pyautogui.rightClick(x, y)
        log.info("right_click x=%d y=%d", x, y)
    else:
        pyautogui.rightClick()
        log.info("right_click (current pos)")
    return "ok"


@mcp.tool()
def type_text(text: str, interval_ms: int = 10) -> str:
    """Type a literal string of text. `interval_ms` is the per-character delay.

    Uses pyautogui.typewrite so only ASCII keys are supported. For arbitrary
    Unicode text, paste via the clipboard from your client instead.
    """
    pyautogui.typewrite(text, interval=max(0, interval_ms) / 1000.0)
    log.info("type_text len=%d interval_ms=%d", len(text), interval_ms)
    return "ok"


@mcp.tool()
def send_keybind(keys: str) -> str:
    """Press a keyboard shortcut. Keys are '+' separated, e.g.:

    - `cmd+d`
    - `cmd+shift+p`
    - `ctrl+alt+delete`
    - `f5`
    - `enter`

    Aliases: cmd/meta/super/win → command; opt/alt → option; esc → escape;
    return → enter; pgup/pgdn → pageup/pagedown.
    """
    parts = [_normalize_key(k) for k in keys.split("+") if k.strip()]
    if not parts:
        return "error: no keys provided"
    log.info("send_keybind %s -> %s", keys, "+".join(parts))
    pyautogui.hotkey(*parts)
    return "ok"


@mcp.tool()
def scroll(
    dy: int,
    dx: int = 0,
    x: Optional[int] = None,
    y: Optional[int] = None,
) -> str:
    """Scroll by (dy, dx) in the app under the cursor.

    Positive `dy` scrolls down, negative up. If `x`/`y` are provided the
    cursor moves there first so the correct scroll target is hit.
    """
    if x is not None and y is not None:
        pyautogui.moveTo(x, y)
    if dy:
        pyautogui.scroll(-dy)  # pyautogui: positive click count = up
    if dx:
        pyautogui.hscroll(dx)
    log.info("scroll dy=%d dx=%d", dy, dx)
    return "ok"


@mcp.tool()
def drag_to(
    x: int,
    y: int,
    duration_ms: int = 200,
    button: str = "left",
) -> str:
    """Press-and-drag from the current cursor position to (x, y)."""
    pyautogui.dragTo(x, y, duration=max(0, duration_ms) / 1000.0, button=button)
    log.info("drag_to x=%d y=%d dur_ms=%d button=%s", x, y, duration_ms, button)
    return "ok"


@mcp.tool()
def screen_size() -> dict:
    """Return the primary screen size in logical pixels."""
    size = pyautogui.size()
    return {"width": int(size.width), "height": int(size.height)}


if __name__ == "__main__":
    log.info("computer-use-py MCP server starting on stdio")
    mcp.run()
