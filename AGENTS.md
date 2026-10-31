# Ultra Pi Star (Rust) — agent notes

## Pi access

- `ssh ups-pi` — ssh config entry: `jaison@192.150.2.244`, key `id_ups_pi`
  (mDNS name `utra-pi-star.local` if DHCP moves the IP). The login user is
  **`jaison`**, not `pi`.
- Rust checkout on the Pi: `~/ups-rust`; binary
  `~/ups-rust/target/release/ups-app`.
- App runs as systemd **user** service `ups-app` (`--tv`), env
  `UPS_HWACCEL=drm`, `UPS_RENDER_RES=720`,
  `WAYLAND_DISPLAY=wayland-0`. Restart: `systemctl --user restart ups-app`.
- Logs go to the **system** journal (`journalctl -b | grep tv:`), not the
  user journal — `journalctl --user` shows nothing.
- Deploy: `ssh ups-pi 'cd ~/ups-rust && git pull --ff-only &&
  cargo build --release -p ups-app'`, then restart the service.
- Library root: `/media/jaison/ULTRAPISTAR/ultrapistar` (USB stick).
- `scripts/pi-display.sh` installs the labwc fullscreen windowRule +
  preferred-mode autostart. Output runs at the panel's EDID preferred
  mode — the app renders 720p and the compositor stretches it. Do NOT
  force a non-native output mode; some panels zoom/overscan it badly.
- HTTP API on `:5000`; `/api/queue` lists `now_playing`.
- fail2ban bans aggressive key sweeps — if ssh suddenly refuses at TCP
  level, `sudo fail2ban-client unban --all` on the Pi console.

## House rules

- No attribution trailers (`Co-Authored-By`, `Generated with`) in commits.
- Project is GPL-3.0; port inventory in `NOTICE.md`.
