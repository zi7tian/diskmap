# Cleanup guide: the right way to free space

Use this after DiskMap has shown where the space is. For each big item, find the matching entry
and recommend the method given here instead of deleting files by hand. Always show the user the
command and what it removes, and wait for confirmation before running it. Commands marked
**(admin)** need administrator or root rights; let the user run those themselves.

Space estimates come from DiskMap: use the `size` of the matching folder.

## Any system: developer and app caches

| What DiskMap shows | How to clean | Notes |
|---|---|---|
| npm cache (`~/.npm/_cacache`, `%LOCALAPPDATA%\npm-cache`) | `npm cache clean --force` | Re-downloaded when needed |
| pnpm store (`~/.local/share/pnpm/store`, `~/Library/pnpm/store`, `%LOCALAPPDATA%\pnpm\store`) | `pnpm store prune` | Removes only packages no project references |
| Yarn cache | `yarn cache clean` | |
| pip cache (`~/.cache/pip`, `~/Library/Caches/pip`, `%LOCALAPPDATA%\pip\Cache`) | `pip cache purge` | |
| uv cache | `uv cache clean` (or `uv cache prune` to keep what is in use) | |
| Conda packages | `conda clean --all` | Removes unused packages and tarballs |
| Go build / module cache | `go clean -cache` / `go clean -modcache` | Module cache is re-downloaded |
| Rust build output (`target/` in a project) | `cargo clean` in that project | Next build is slower |
| Cargo registry (`~/.cargo/registry`) | Removing `registry/cache` and `registry/src` is safe | Re-downloaded on next build |
| Gradle (`~/.gradle/caches`), Maven (`~/.m2/repository`) | Remove while no build is running | Re-downloaded; next build is slower |
| `node_modules` in old projects | Remove the folder; `npm install` / `pnpm install` restores it | Ask which projects are still active |
| Docker / Podman | `docker system df` to see usage, then `docker system prune` | `prune` removes stopped containers, unused networks, dangling images and build cache. `-a` also removes all unused images; `--volumes` removes data volumes: ask first |
| AI models: Ollama (`~/.ollama/models`) | `ollama list`, then `ollama rm <model>` | Large and slow to download again: ask |
| AI models: Hugging Face (`~/.cache/huggingface`) | The Hugging Face CLI cache commands (`hf cache` or `huggingface-cli delete-cache`, depending on version) | Ask which models are still used |
| Browser caches | The browser's "Clear browsing data → Cached images and files" | Do not delete profile folders: they hold passwords, history and extensions |
| Steam, Epic and other game libraries | Uninstall games from the launcher | Never delete game folders by hand while the launcher is running |
| VM disk images (`.vmdk`, `.vdi`, `.qcow2`, `.vhdx`) | Delete or compact through the VM app | Category `other`/`user`: these hold whole systems |
| Trash / Recycle Bin | Empty it from the file manager | Make sure the user does not need anything in it |

## Linux

| What DiskMap shows | How to clean | Notes |
|---|---|---|
| `/var/cache/pacman/pkg` (Arch) | `paccache -rk2` **(admin)** keeps the last 2 versions of each package (from `pacman-contrib`); `pacman -Sc` **(admin)** removes cached packages that are no longer installed | Keeping recent versions allows downgrades |
| Orphaned packages (Arch) | `pacman -Qdtq` lists them; review before removing with `pacman -Rns` **(admin)** | |
| `/var/cache/apt/archives` (Debian, Ubuntu) | `apt clean` **(admin)** | `apt autoremove` **(admin)** removes unused dependencies and old kernels |
| `/var/cache/dnf` (Fedora, RHEL) | `dnf clean packages` **(admin)** | `dnf autoremove` **(admin)** for unused dependencies |
| `/var/log/journal` | `journalctl --disk-usage`, then `journalctl --vacuum-size=500M` **(admin)** | Keeps the newest 500 MB of logs |
| Old rotated logs in `/var/log` (`*.gz`, `*.1`) | Safe to remove **(admin)** | Leave the current log files in place |
| Flatpak (`/var/lib/flatpak`, `~/.local/share/flatpak`) | `flatpak uninstall --unused` | Removes runtimes no app needs |
| Snap (`/var/lib/snapd/snaps`) | `snap list --all` shows `disabled` revisions; `snap remove <name> --revision=<rev>` **(admin)** | `snap set system refresh.retain=2` **(admin)** keeps fewer old revisions |
| `~/.cache` | Mostly safe; close the matching app first | `~/.cache/thumbnails` is always safe |
| `~/.local/share/Trash` | Empty the trash in the file manager, or `gio trash --empty` | |
| Swap file (`/swapfile`, category `core`) | Only resize through the distribution's swap setup | Never delete while active |

## macOS

| What DiskMap shows | How to clean | Notes |
|---|---|---|
| Homebrew (`/opt/homebrew`, `/usr/local/Cellar`, `~/Library/Caches/Homebrew`) | `brew cleanup` (old versions and downloads); `brew cleanup --prune=all` for the whole download cache | `brew autoremove` removes unused dependencies |
| Xcode `~/Library/Developer/Xcode/DerivedData` | Safe to remove | Rebuilt on the next build |
| Xcode `iOS DeviceSupport` | Safe to remove versions for devices no longer used | Recreated when a device connects |
| Xcode `Archives` | Ask | Needed to symbolicate crash reports of shipped builds |
| iOS simulators (`~/Library/Developer/CoreSimulator`) | `xcrun simctl delete unavailable` | Removes simulators for SDKs no longer installed |
| `~/Library/Caches` | Mostly safe; quit the matching app first | |
| iPhone/iPad backups (`~/Library/Application Support/MobileSync/Backup`) | Manage in Finder (device → Manage Backups) | Personal data: ask |
| Time Machine local snapshots | `tmutil listlocalsnapshots /` | macOS removes them when space is needed; not counted by DiskMap |
| Mail attachments, Messages | Manage in the app or System Settings → General → Storage | |
| General | System Settings → General → Storage shows Apple's recommendations | |

## Windows

| What DiskMap shows | How to clean | Notes |
|---|---|---|
| `%TEMP%` (`C:\Users\<name>\AppData\Local\Temp`), `C:\Windows\Temp` | Settings → System → Storage → Temporary files, or Disk Cleanup (`cleanmgr`) | Files in use are skipped automatically |
| `C:\Windows\SoftwareDistribution\Download`, Delivery Optimization | Settings → System → Storage → Temporary files ("Windows Update Cleanup", "Delivery Optimization Files") | |
| `C:\Windows.old` | Temporary files → "Previous Windows installation(s)" | Removes the option to roll back the last upgrade |
| `C:\Windows\WinSxS` | `Dism.exe /Online /Cleanup-Image /AnalyzeComponentStore`, then `/StartComponentCleanup` **(admin)** | Use component servicing to clean it. A whole-volume MFT scan counts hard links once. Generic scans can overstate the size through repeated hard links |
| `hiberfil.sys` (category `core`) | `powercfg /hibernate off` **(admin)** removes it; `powercfg /h /type reduced` shrinks it | Turning hibernation off also disables Fast Startup |
| `pagefile.sys` (category `core`) | System → Advanced system settings → Performance → Virtual memory | Leave on automatic unless the user knows why |
| `C:\Windows\Installer`, `C:\ProgramData\Package Cache` | Do not delete | Needed to repair, update and uninstall programs |
| System Restore (`System Volume Information`, usually unreadable) | System Properties → System Protection → Configure | Lower the maximum usage instead of deleting points one by one |
| WSL disks (`ext4.vhdx`) | Clean up inside the distribution first; `wsl --manage <distro> --set-sparse true` lets the disk shrink automatically | Run `wsl --shutdown` first |
| Docker Desktop (`docker_data.vhdx`) | `docker system prune` inside Docker, then Docker Desktop → Troubleshoot → Clean / Purge data if needed | Purge removes all images and containers: ask |
| Recycle Bin (`C:\$Recycle.Bin`) | Empty from the desktop icon | |
| Large installed programs (category `system`) | Settings → Apps → Installed apps → Uninstall | Never delete folders in Program Files directly |
| General | Turn on Storage Sense (Settings → System → Storage) | Cleans temp files and the Recycle Bin on a schedule |

## Personal files (`ask_user`)

DiskMap cannot tell which personal files still matter. Help the user decide instead:
- Point out the biggest items in Downloads, Videos and Desktop, and duplicated installers
  (`.dmg`, `.exe`, `.msi`, `.iso`) that are no longer needed.
- Suggest moving archives to an external drive or cloud storage rather than deleting them.
- Mention that cloud folders (OneDrive, iCloud Drive, Dropbox) can free local space by keeping
  files online only, through the sync app's own settings.
