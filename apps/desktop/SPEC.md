# Desktop interface — first visual delivery

Task 24. Frontend for the future Tauri 2 shell, implemented with native HTML/CSS/ES modules and no dependency installation.

## Contract
- Four Portuguese views: home, local clips, friends/groups, settings. Dark theme, keyboard navigation, visible focus, reduced-motion support, responsive layout.
- No fake clip, online friend, recording status or cloud connection. Recording is explicitly disconnected; friends have no active network controls yet.
- The user selects or drops nonempty MP4 files. Object URLs stay in this page; no upload, folder scan, telemetry, remote font or other external request.
- Search by name, sort by date/name/size, play with native controls, remove from the in-memory list without changing the original file. Metadata/codec failures give feedback.
- Preferences persist in browser localStorage only, versioned and validated on restore. They do not change `%APPDATA%\\DuoClip\\config.toml` automatically.
- Export a complete TOML file compatible with `duoclip-recorder/src/config.rs`, including custom video quality, timing, shortcut, audio volumes, sound paths, extra executable and clip folder. Reject invalid values before exporting; keep root keys before sections.
- Preview server binds only `127.0.0.1` and serves an explicit allowlist of interface assets. No access to repository secrets.

## Integration boundary
No Tauri/Rust dependency, recorder launch, new capture or Worker request in this delivery. Keep core/network ownership untouched. Later, a Tauri command adapter replaces manual clip selection/config export and supplies real recorder events/group snapshots. Do not put Cloudflare credentials in the frontend; only the device identity belongs on the native side.

## Verification
`npm.cmd test`: export/validation/draft recovery and local-server isolation. `npm.cmd run test:browser`: headless Edge via CDP, real MP4 import/playback, search, remove, settings persistence, export, routing and mobile overflow; saves viewport screenshots in repository `test-output/interface-inicial/`. Browser screenshots cover only this test page, never the desktop.
Also run the repository's required Rust checks. Use `--check-config` to validate exported TOML against the actual recorder without recording.

## Limits
Clips disappear from the list when the page is closed/reloaded and must be selected again. Settings belong to the preview origin/browser, not the native config. Automatic folder reading, tray, recording buttons, editor, group interaction and packaging are future integration work.
