#!/bin/sh
# Pi display defaults for the native TV renderer:
#   - labwc windowRule fullscreens the `ultra-pi-star` surface (Wayland
#     clients can't self-fullscreen through minifb — the compositor must)
#   - HDMI out at the panel's preferred mode — the app renders a fixed
#     720p buffer and the compositor stretches it fullscreen; forcing a
#     non-native mode hands the upscale to the display's own scaler,
#     which some panels/projectors zoom or overscan badly
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
# (re)install our block — marker-delimited so re-runs replace instead of
# stacking, and so it also replaces the old forced-720p line
sed -i '/# ups-display begin/,/# ups-display end/d' "$as"
sed -i '1a\
# ups-display begin\
# output at the panel'"'"'s preferred mode — the app renders a fixed 720p\
# buffer and the compositor stretches it fullscreen. Forcing a non-native\
# mode hands the upscale to the display'"'"'s own scaler, which some\
# panels and projectors zoom or overscan badly.\
out=$(wlr-randr 2>/dev/null | awk '"'"'/^HDMI|^eDP|^LVDS/{print $1; exit}'"'"')\
mode=$(wlr-randr 2>/dev/null | awk '"'"'/\(preferred/{print $1; exit}'"'"')\
[ -n "$out" ] \&\& [ -n "$mode" ] \&\&\
  wlr-randr --output "$out" --mode "$mode" 2>/dev/null &\
# ups-display end' "$as"
grep -q 'start ups-app' "$as" || echo 'systemctl --user start ups-app' >> "$as"

# live-apply if a session is already running
out=$(wlr-randr 2>/dev/null | awk '/^HDMI|^eDP|^LVDS/{print $1; exit}')
mode=$(wlr-randr 2>/dev/null | awk '/\(preferred/{print $1; exit}')
[ -n "$out" ] && [ -n "$mode" ] && wlr-randr --output "$out" --mode "$mode" 2>/dev/null || true
labwc -r 2>/dev/null || labwc --reconfigure 2>/dev/null || true

echo "pi-display: fullscreen windowRule + preferred-mode autostart installed"
