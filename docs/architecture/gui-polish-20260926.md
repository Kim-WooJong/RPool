# GUI polish — 2026-09-26

Presentation-only update, version unchanged. Light/dark panel palettes, larger controls and rounded cards, grouped full-width navigation, default 1180x800 window (minimum 800x600), dashboard cards with narrow-layout stacking and scrolling, wrapped operation header and progress metadata, explanatory idle state. Existing structured byte/item progress now renders for current or last task; failed/cancelled partial progress is preserved. Retry is still deferred; no backend or settings persistence changes.

Validation: macOS default suite 150 passed, 11 ignored; build succeeded. Independent targeted review found no definite blockers. Native startup smoke is recorded separately in task output; no screenshot review, Windows execution or interactive click validation is claimed. Optional backend behavior was not changed and optional suite was not rerun for this UI-only update. Earlier delivery ZIP remains historical; use the gui-polish source ZIP for these changes.
