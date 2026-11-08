# WT Armory Manager

A Windows desktop app for browsing community content on [live.warthunder.com](https://live.warthunder.com) (skins, sound mods, sights, missions, models, hangars, control presets). It installs and removes that content with one click. It is written in **Rust** with **egui/eframe 0.29**: one .exe, no web code. The owner is Max (GitHub `maxlvl01`).

## Build, run, test

Rust lives in `%USERPROFILE%\.cargo\bin`, which may be missing from the PATH of an already-open shell:

```powershell
$env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"
cargo build            # debug: target\debug\WTArmoryManager.exe
cargo build --release  # release: target\release\WTArmoryManager.exe (~10 MB, no console window)
cargo test             # unit tests: archive layout, config.blk edits, changelog parsing, dates
```

Windows can't overwrite a running exe, so close the app (or check `Get-Process WTArmoryManager`) before rebuilding that profile.

## Code map (`src/`)

| File | Responsibility |
|---|---|
| `main.rs` | UI and app state. Background threads report back via an `mpsc` channel (`Msg`). Widgets add an `Action` during drawing, and actions are applied after the frame, which keeps the borrow checker happy. |
| `api.rs` | live.warthunder.com client (reqwest blocking), the `Post` model with lenient serde, and HTML-to-text. |
| `installer.rs` | Installs and uninstalls per content type, and keeps the manifest (`installed.json`). |
| `game.rs` | Finds the game folder and the Saves/user ID, and makes line-based `config.blk` edits. |
| `updates.rs` | Major WT updates (changelog scraping plus a built-in list), the compatibility check, and date helpers. |
| `storage.rs` | App data in `%LOCALAPPDATA%\WT Armory Manager`: settings, manifest, file-date cache, backups. |
| `icons.rs` | Vector icons painted with egui shapes. |
| `updater.rs` | Self-update from GitHub releases: version compare, download with SHA-256 check, swap of the running exe. |
| `login.rs` | live.warthunder.com login: a WebView2 window in a child process (`--login`), plus the saved session. |

## Facts that aren't obvious from the code

- **Site API** (no login, no cookies): form-encoded `POST /api/feed/get_regular/` with `content`, `sort`, `period`, `searchString` (words must be `#hashtags`), `page`, `subtype=all`, and `vehicleType` for camouflages. Single posts come from `POST /api/posts/get/` with `lang_group`. `lang_group`, not `id`, is the post's identity. `file.link` redirects (302) to the CDN.
- **Hangars** have no type of their own. They are `location` posts, and the post text contains `hangarBlk:t="..."`.
- **Posts have no "edited" field.** The CDN's `Last-Modified` header on the file is the real "last updated" date: a HEAD request through 4 worker threads, cached by link in `file_dates.json`.
- **Latest major update** is the *pinned first* entry on `https://warthunder.com/en/game/changelog/?page=1`, titled `War Thunder "<Name>" - Changelog`. Its version comes from the first `Update X.Y.0.n` hotfix after it. When a new major ships, add it to `KNOWN` in `updates.rs`; the built-in list is the offline fallback.
- **Game folder detection** requires `win64\aces.exe` or `content\pkg_main.ver`. A launcher-only folder (Max has one at `C:\Program Files (x86)\Steam\...`; the real game is on `D:\SteamLibrary\...`) has its own `config.blk` that the game never reads. The installed version is in `content\pkg_main.ver`.
- **Install targets:**
  - Skins go in `UserSkins\<vehicle folder>`.
  - Sound mods go in `sound\mod` and need `enable_mod:b=yes` inside `sound{}`.
  - Sights go in `Documents\My Games\WarThunder\Saves\<uid>\production\UserSights` (since 2.53).
  - Missions go in `UserMissions`, with no subfolders.
  - Models and hangars merge into the game root.
  - Control presets can only be imported in game, so they are saved to `Documents\My Games\WarThunder\WT Armory Manager presets`. They used to go to `Documents\WT Armory Manager\...`, which is this repo's folder on Max's PC, so keep downloads out of the project folder.
- **Hangars are never switched on by installing.** The installer only records `ConfigChange::Hangar { value }`. After installing, a popup asks "Use new hangar / Keep current hangar / Remove it". The Hangar panel (Installed page and the Hangars page) has one on/off switch per installed hangar (only one can be active) and a "Use default hangar" button. That button removes the top-level `hangarBlk` line, which brings back War Thunder's default hangar. Uninstalling the active hangar also falls back to the default.
- **Uninstall must be exact.** The installer records every file written and every folder created, and backs up every file it overwrites. Never delete anything that isn't recorded in the manifest.
- **config.blk:** edit line by line while tracking brace depth, never parse and rewrite the whole file. Take a timestamped backup before each write. Refuse edits while `aces.exe` runs, because the game rewrites the file when it exits.
- **Creators:** a user's posts come from `POST /api/feed/get_user/` with `user=<id>` (`content=all`, `sort=created`; it returns mixed types, so filter by `type`). There is no JSON profile: nickname, avatar and counts are scraped from `/user/<id>/` (`data_line nickname`, `Reputation: <strong>`, `<strong>N</strong> posts`). Following and hidden creators are local only (`settings.json`), because the site has no favourites.
- **Anime filter:** the site has no flag for it. `Post::looks_anime` checks hashtags (`ANIME_TAGS`) and whole-word franchise names (`ANIME_WORDS`). Words like "panzer", "girl" or "fate" are left out on purpose (false positives). "Hide creator" covers what text can't detect.
- **Login:** `/auth/auth/?return=<b64>` redirects to Gaijin SSO. The `--login` process loads it in WebView2 (profile in `<app dir>\webview`), waits until a page reports `live_wt.user_id != 0` over IPC, then saves the `live.warthunder.com` cookies to `session.json`. `api::set_session` adds them as a `Cookie` header to every site request. At start the session is checked with `api::whoami` (reads `user_id:` from `/feed/`); if it is 0, the session is dropped. The write endpoints are `POST /api/post/like/` (`lang_group`, toggles; returns `likes`, `isLiked`) and `POST /api/comment/create/` (`text`, `postId`, `langGroup`, optional `replyId`). Neither has been verified with a real account yet. Reading comments (`/api/comment/get_list/` with `postId` = the post's `id` and `langGroup`) works anonymously.
- **Self-update:** `GET api.github.com/repos/maxlvl01/wt-armory-manager/releases` (not `/latest`, which skips pre-releases). It takes the newest tag by `updater::version_key` (a < b < rc < final) and the first `.exe` asset, and verifies its `digest` (sha256) and size. The running exe is renamed to `.exe.old` and the new one moved into place; `cleanup_old()` deletes the leftover on the next start. Debug builds only report updates and never install them. Releases must therefore keep attaching a single `.exe`.
- **Download queue:** `MAX_PARALLEL_DOWNLOADS` (2) jobs run at once, and `pump_queue()` starts the next one. Cancelling sets an `AtomicBool` that `api::download`'s progress callback checks (it returns `false`, which gives `api::CANCELLED`).
- **Bug reports** only open `github.com/.../issues/new?title=&body=` in the browser; nothing is sent by the app. The home folder is replaced with `%USERPROFILE%` (`redact`) and the body is kept under about 6000 encoded characters.

## Conventions

- **No emoji anywhere in the UI.** Draw icons in `icons.rs` instead (Max's standing rule).
- egui is pinned to 0.29. Use `id_salt` (not `id_source`), `Rounding`, and `ui.new_child(UiBuilder…)` for detached child UIs. Write float literals passed to `Stroke::new` as `1.0_f32` (the f32 fallback lint).
- Theme colours are the constants at the top of `main.rs` (dark steel with amber `ACCENT`).
- Research and asset gathering go to a Sonnet subagent; implementation stays in the main session (see the global CLAUDE.md).

## Verifying the UI

The computer-use tool can't target this app because it isn't "installed". To take a screenshot, run a PowerShell `CopyFromScreen` script on the window rect. That pulls the window to the front, so don't do it while Max is using the app. Clicks injected with `SetCursorPos` and `mouse_event` only work reliably while the window already has focus.

To test without touching Max's real game, set `WTAM_DATA_DIR` to a scratch folder. Its `settings.json` can point `game_root` at a fake game folder (it needs `win64\aces.exe`, `content\pkg_main.ver` and `config.blk`), and `installed.json` can be hand-written.

For screenshots, `WTAM_SCREEN` opens a page at start: `camouflage` (any content code), `following`, `creator:<id>`, `detail:<lang_group>`, `downloads`, `bug` or `installed`. Capture with `PrintWindow` (flag 2) on the process's window handle rather than `CopyFromScreen`, so other windows on top don't end up in the picture and the window doesn't need focus. Wait about 15 s for the thumbnails to load. The README screenshots live in `docs/screenshots/`.

## Releases

Cargo needs a semver version (`0.1.0-alpha`). The tag and the displayed `APP_VERSION` in `main.rs` use Max's style (`v0.1.0a`). Mark alpha and beta releases as GitHub pre-releases:

```powershell
cargo build --release
gh release create v0.1.0a "target\release\WTArmoryManager.exe#WT-Armory-Manager-0.1.0a.exe" --prerelease --title "WT Armory Manager 0.1.0a" --notes-file notes.md
```
