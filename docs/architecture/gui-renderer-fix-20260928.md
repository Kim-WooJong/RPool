# GUI renderer workaround and warning cleanup — 2026-09-28

## Windows report and bounded workaround

The reported egui-wgpu 0.36.2 panic requested 69,372 bytes within a 92,112-byte index buffer. It does not establish an undersized destination buffer or prove a GPU-driver failure. WGPU staging-buffer creation/validation returned no buffer.

Eframe now disables default features, enables Glow explicitly and selects `Renderer::Glow`. The default dependency tree contains egui_glow/glutin and no wgpu/egui-wgpu. This bypasses the reported failing path; it does not diagnose or repair WGPU itself. Windows needs a working OpenGL driver and remains user-validation pending. No storage format, encryption policy or Initial-setup code changed.

## Warning treatment

Test-only crypt generation/export helpers, sensitive constructor and injection/accessor helpers are compiled only for tests. Unused GUI status/progress helpers, Pending and unreachable retry helper/metadata were removed; automatic retry was not introduced. Explicit config recovery is preserved without automatic execution. Future backend contracts use narrowly scoped production-only `expect(dead_code)` with reasons, not crate-wide warning suppression. Thus a warning-free build is not a claim that all future contracts are integrated.

Combined cancellation/deadline coverage and identity assertions were strengthened. Cross-platform CI now sets `RUSTFLAGS=-D warnings`.

## Validation

macOS ARM64: locked/offline nightly with `RUSTFLAGS=-D warnings`: default 151 passed, optional OpenDAL 162 passed, zero failures, 11 real-tool tests ignored in each suite. Both release builds succeeded without warnings. The optional release binary stayed alive for eight seconds with isolated settings and a nonexistent rclone executable, with no log output; then the harness terminated it. This is startup-only evidence, not interactive rendering verification. Windows runtime reproduction and interactive GUI navigation/cancellation remain unverified. External rclone/age tests are not rerun for this renderer/warning-only change.

## Windows retest

Extract the new archive into a separate source folder; do not reuse the old rpool.exe. Run `cargo build --release --locked`, then `target\release\rpool.exe gui`. Check startup, navigation, window resizing, light/dark themes and task progress with test data. Report any OpenGL initialization error separately from the former WGPU panic.
