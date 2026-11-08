# WT Armory Manager

Browse [live.warthunder.com](https://live.warthunder.com) and install War Thunder community content with one click. You can remove it just as easily. No manual copying of folders and no editing of `config.blk` by hand.

> **Alpha.** This is an early version. Back up anything you care about, and please report problems in the Issues tab.

## Features

- **Browse like the website.** Camouflages, sound mods, sights, missions, custom models, hangars and locations, and control presets, with the site's sort orders, hashtag search, a vehicle-type filter for skins, and infinite scrolling.
- **One-click install.** Each type goes where War Thunder expects it:

  | Type | Installed to |
  |---|---|
  | Camouflages | `<game>\UserSkins\<vehicle>` |
  | Sound mods | `<game>\sound\mod`, and turns on `enable_mod:b=yes` in `config.blk` |
  | Sights | `Documents\My Games\WarThunder\Saves\<user ID>\production\UserSights` |
  | Missions | `<game>\UserMissions` |
  | Custom models | merged into the game folder (`content`, `UserMissions`) |
  | Hangars | merged into the game folder; switch them on and off in the app (sets `hangarBlk` in `config.blk`) |
  | Control presets | `Documents\My Games\WarThunder\WT Armory Manager presets` (import in game: Controls > Import from file) |

- **Hangar switch.** After you install a hangar, the app asks before replacing your current one. Each installed hangar gets an on/off switch, and one click on **Use default hangar** brings back the normal hangar.
- **Clean uninstall.** The app removes exactly the files it added, restores any file it replaced, and undoes its `config.blk` changes.
- **Compatibility check.** Each mod is compared with the latest major War Thunder update, read live from the official changelog. A badge shows whether it was made for the current version or how many major updates old it is. There is also a filter to show only up-to-date mods.
- **Finds your game automatically** in the Gaijin launcher folder or any Steam library, or lets you choose the folder yourself.
- **Safe with `config.blk`.** A backup is saved before every change, and the app won't edit the file while the game is running.

## Download

Get `WT-Armory-Manager-0.1.0a.exe` from the [Releases](https://github.com/maxlvl01/wt-armory-manager/releases) page and run it. Nothing needs to be installed. Windows SmartScreen may warn you because the exe isn't code-signed.

## Build from source

Requires [Rust](https://rustup.rs) and the Visual Studio C++ build tools.

```powershell
cargo build --release
.\target\release\WTArmoryManager.exe
```

## Notes

- Only `.zip` archives and single files can be installed automatically. `.rar` and `.7z` posts need to be installed manually.
- The app keeps its data in `%LOCALAPPDATA%\WT Armory Manager`: settings, the list of installed items, and backups.
- Content on live.warthunder.com is made by the War Thunder community. This project is not affiliated with or endorsed by Gaijin Entertainment.

## License

[MIT](LICENSE)
