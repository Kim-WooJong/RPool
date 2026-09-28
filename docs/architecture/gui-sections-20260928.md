# Section scrolling and source archive revisions — 2026-09-28

Dashboard now uses four bounded independent viewports (overview/attention, providers, pools, recent jobs), with no whole-page scroll. Upload has independent source queue, destination/options and preflight panes; its start toolbar remains outside the scroll panes. Settings has separate defaults, remote paths and destinations panes. Jobs separates current operation and history controls. Existing library/results/console scroll regions remain intact.

Each new region has a stable scroll ID and a viewport budget based on available height. Dashboard/upload clip child content and allow horizontal scrolling for wide tables and forms. Storage behavior, configuration formats, Glow renderer and Initial-setup are unchanged.

Source archives now live in `artifacts/rpoll` as `rpool-0.5.15-rN.zip`. `rN` identifies a source-delivery revision, not a Cargo version change; 0.5.16 remains reserved. Use the short-name archives in this folder as the canonical source deliveries.

Validation results are recorded in the archive folder README. Windows runtime and manual wheel/trackpad/resize interaction remain user-validation pending; compilation/tests do not establish visual behavior.
