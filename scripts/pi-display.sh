#!/bin/sh
# Pi display defaults for the native TV renderer:
#   - labwc windowRule fullscreens the `ultra-pi-star` surface (Wayland
#     clients can't self-fullscreen through minifb — the compositor must)
#   - HDMI out at 1280x720@60 so the panel's own scaler does the upscale
# Idempotent — safe to re-run; only touches what it owns.
set -e

mkdir -p "$HOME/.config/labwc"
rc="$HOME/.config/labwc/rc.xml"

rule='    <windowRule identifier="ultra-pi-star" serverDecoration="no">
      <action name="ToggleFullscreen"/>
    </windowRule>'

if [ -f "$rc" ] && grep -q 'ultra-pi-star' "$rc"; then
    :
elif [ -f "$rc" ] && grep -q '<windowRules>' "$rc"; then
    sed -i "s|</windowRules>|$rule\n  </windowRules>|" "$rc"
elif [ -f "$rc" ] && grep -q '</labwc_config>' "$rc"; then
    sed -i "s|</labwc_config>|  <windowRules>\n$rule\n  </windowRules>\n</labwc_config>|" "$rc"
else
    cat > "$rc" <<'EOF'
<?xml version="1.0"?>
<labwc_config>
  <windowRules>
    <windowRule identifier="ultra-pi-star" serverDecoration="no">
      <action name="ToggleFullscreen"/>
    </windowRule>
  </windowRules>
</labwc_config>
EOF
fi

as="$HOME/.config/labwc/autostart"
[ -f "$as" ] || printf '#!/bin/sh\n' > "$as"
chmod +x "$as"
if ! grep -q 'wlr-randr' "$as"; then
    sed -i '1a\
# 720p HDMI out — the panel'"'"'s hardware scaler upscales, and every pixel\
# of framebuffer work on the Pi costs ~4x less than 1080p.\
out=$(wlr-randr 2>/dev/null | awk '"'"'/^HDMI|^eDP|^LVDS/{print $1; exit}'"'"')\
[ -n "$out" ] \&\& wlr-randr --output "$out" --mode 1280x720@60 2>/dev/null &' "$as"
fi
grep -q 'start ups-app' "$as" || echo 'systemctl --user start ups-app' >> "$as"

# live-apply if a session is already running
wlr-randr --output HDMI-A-1 --mode 1280x720@60 2>/dev/null || true
labwc -r 2>/dev/null || labwc --reconfigure 2>/dev/null || true

echo "pi-display: fullscreen windowRule + 720p autostart installed"
