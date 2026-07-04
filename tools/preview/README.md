# UI preview harnesses

Standalone [`slint-viewer`](https://slint.dev/) entry points that render individual modals from `assets/ui/components/` without launching the full Atlas app.

Useful for:

- **Visual verification** of modal changes without spinning up the entire binary
- **Screenshot capture** for PR reviews (each harness paints a plausible workspace backdrop behind the modal so scrim + shadow read at their real visual weight)
- **Bisecting layout regressions** without waiting for a full `cargo build`

## Usage

Install `slint-viewer` once (matches the workspace-pinned Slint version):

```sh
cargo install slint-viewer --version "=1.17.0" --locked
```

Then render any harness:

```sh
slint-viewer tools/preview/conflict-preview.slint
slint-viewer tools/preview/progress-preview.slint
slint-viewer tools/preview/bulk-rename-preview.slint
slint-viewer tools/preview/palette-preview.slint
```

Screenshot with `screencapture -x <out.png>` or your OS's built-in tool.

## Guardrails

- **These files are *not* used at runtime.** The Atlas app compiles `assets/ui/atlas.slint` directly; the previews only exist for developer verification.
- **Keep them in sync with the real components.** If a modal's public property list changes, update the matching preview file so the harness still compiles.
- **Do not add behavior.** Preview harnesses set static input properties and never wire callbacks. Behavior is tested in Rust integration tests, not here.
