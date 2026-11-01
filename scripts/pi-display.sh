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

# each rule lands once — identifier matches the app_id minifb sets,
# title is the fallback if a future backend stops setting app_id
rule_id='    <windowRule identifier="ultra-pi-star" serverDecoration="no">
      <action name="ToggleFullscreen"/>
    </windowRule>'
rule_ti='    <windowRule title="ultra-pi-star" serverDecoration="no">
      <action name="ToggleFullscreen"/>
    </windowRule>'

if [ ! -f "$rc" ]; then
    cat > "$rc" <<EOF
<?xml version="1.0"?>
<labwc_config>
  <windowRules>
$rule_id
$rule_ti
  </windowRules>
</labwc_config>
EOF
else
    if ! grep -q '<windowRules>' "$rc"; then
        sed -i "s|</labwc_config>|  <windowRules>\n  </windowRules>\n</labwc_config>|" "$rc"
    fi
    # awk -v carries multi-line values that sed s/// can't
    ins() {
        grep -q "$2" "$rc" && return 0
        awk -v r="$1" '/<\/windowRules>/{print r} {print}' \
            "$rc" > "$rc.new" && mv "$rc.new" "$rc"
    }
    ins "$rule_id" 'identifier="ultra-pi-star"'
    ins "$rule_ti" 'title="ultra-pi-star"'
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
# live-reload rc.xml — labwc --reconfigure signals the running
# instance via LABWC_PID; without it the call errors and the
# windowRule never reaches a long-running session
[ -z "$LABWC_PID" ] && LABWC_PID=$(pgrep -x labwc 2>/dev/null | head -1)
[ -n "$LABWC_PID" ] && LABWC_PID=$LABWC_PID labwc --reconfigure 2>/dev/null || true

echo "pi-display: fullscreen windowRule + preferred-mode autostart installed"
