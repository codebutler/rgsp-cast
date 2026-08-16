#!/bin/sh
# Exercises launch.sh's toggle logic with a stub daemon, off-device.
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
TMPBASE="${TMPDIR:-.}"
TMP=$(mktemp -d "$TMPBASE/test-XXXXXX")
trap 'rm -rf "$TMP"' EXIT

mkdir -p "$TMP/pak"
cp "$HERE/../pak/launch.sh" "$TMP/pak/"
# Stub daemon: sleeps until killed, creates and removes PID file to emulate real daemon.
cat > "$TMP/pak/rgsp-host" <<'EOF'
#!/bin/sh
mkdir -p "$(dirname "$RGSP_RUN_DIR/daemon.pid")"
echo $$ > "$RGSP_RUN_DIR/daemon.pid"
# Use a wrapper to handle cleanup
(
  trap 'exit 0' TERM
  sleep 300
)
rm -f "$RGSP_RUN_DIR/daemon.pid"
EOF
chmod +x "$TMP/pak/rgsp-host" "$TMP/pak/launch.sh"
# Stub show2 so the script does not need NextUI.
mkdir -p "$TMP/bin"
printf '#!/bin/sh\nexit 0\n' > "$TMP/bin/show2.elf"
chmod +x "$TMP/bin/show2.elf"
export PATH="$TMP/bin:$PATH"
export RGSP_RUN_DIR="$TMP/run"
export SHARED_USERDATA_PATH="$TMP/userdata"

echo "--- first launch should start the daemon ---"
sh "$TMP/pak/launch.sh"
PID=$(cat "$TMP/run/daemon.pid")
kill -0 "$PID" || { echo "FAIL: daemon not running"; exit 1; }

echo "--- second launch should stop it ---"
sh "$TMP/pak/launch.sh"
sleep 1
if kill -0 "$PID" 2>/dev/null; then echo "FAIL: daemon still running"; exit 1; fi
[ -f "$TMP/run/daemon.pid" ] && { echo "FAIL: pidfile left behind"; exit 1; }

echo PASS
