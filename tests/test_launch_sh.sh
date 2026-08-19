#!/bin/sh
# launch.sh is now a thin exec into rgsp-ui; the toggle/pidfile/show2 logic
# this test used to exercise lives in rgsp-host and rgsp-ui now, with its own
# tests there. This just checks the two things still specific to the pak
# script itself: the git executable bit, and that it hands off to rgsp-ui
# with LD_LIBRARY_PATH set rather than doing anything else.
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)

# CRITICAL: Verify the committed launch.sh is executable (fix for mode bit regression)
# Skip gracefully if not in a git repository (e.g., from tarball)
if git rev-parse --git-dir >/dev/null 2>&1; then
    if ! git ls-tree HEAD pak/launch.sh | grep -q '^100755'; then
        echo "FAIL: pak/launch.sh is not executable in git repo"
        exit 1
    fi
else
    echo "INFO: not in git repo, skipping executable bit check"
fi

LAUNCH="$HERE/../pak/launch.sh"

grep -q 'exec "\$PAK_DIR/rgsp-ui"' "$LAUNCH" || {
    echo "FAIL: launch.sh does not exec rgsp-ui"
    exit 1
}
grep -q 'LD_LIBRARY_PATH="\$PAK_DIR/lib' "$LAUNCH" || {
    echo "FAIL: launch.sh does not set LD_LIBRARY_PATH before exec"
    exit 1
}

echo PASS
