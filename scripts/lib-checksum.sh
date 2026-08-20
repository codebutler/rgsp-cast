#!/bin/sh
# Shared checksum helpers for install-pak.sh and verify-pak.sh.
#
# Not standalone -- sourced with `.`. Exists so both scripts detect the
# checksum tool and diff a manifest the same way instead of drifting apart.
#
# The device runs BusyBox, which may or may not have been built with
# CONFIG_SHA256SUM; md5sum is the safer bet there. macOS ships neither GNU
# tool by default, only `shasum` and BSD `md5`. Rather than assume either
# side, remote_sum_cmd() probes the device once and both ends then use
# whatever it found.

# Prints "sha256sum" or "md5sum" -- whichever BusyBox on $1 actually has,
# preferring sha256. Exits non-zero if it has neither (some very stripped
# BusyBox builds omit both).
remote_sum_cmd() {
    ssh "$1" '
        if command -v sha256sum >/dev/null 2>&1; then echo sha256sum
        elif command -v md5sum >/dev/null 2>&1; then echo md5sum
        else exit 1
        fi
    '
}

# Computes $2's checksum locally using the algorithm named by $1
# ("sha256sum" or "md5sum", as returned by remote_sum_cmd), printing just
# the hex digest. macOS has neither GNU tool by default, so each algorithm
# falls back to whatever BSD/macOS does provide.
local_sum() {
    algo=$1
    file=$2
    case "$algo" in
        sha256sum)
            if command -v sha256sum >/dev/null 2>&1; then
                sha256sum "$file" | cut -d' ' -f1
            else
                shasum -a 256 "$file" | cut -d' ' -f1
            fi
            ;;
        md5sum)
            if command -v md5sum >/dev/null 2>&1; then
                md5sum "$file" | cut -d' ' -f1
            elif command -v md5 >/dev/null 2>&1; then
                md5 -q "$file"
            else
                openssl dgst -md5 "$file" | awk '{print $NF}'
            fi
            ;;
        *)
            echo "unknown checksum algorithm: $algo" >&2
            return 1
            ;;
    esac
}

# Prints one "relpath<TAB>localabspath" line per regular file under local
# dir $1, with paths relative to $1. Used to build the diff manifest below.
manifest_of_dir() {
    dir=$1
    [ -d "$dir" ] || return 0
    ( cd "$dir" && find . -type f | sed 's|^\./||' ) | while read -r rel; do
        printf '%s\t%s\n' "$rel" "$dir/$rel"
    done
}

# Diffs a manifest (lines of "relpath<TAB>localabspath", as produced by
# manifest_of_dir) against the matching files under $2 on device $1.
# Prints nothing and returns 0 if everything matches; otherwise prints one
# "MISMATCH"/"MISSING" line per bad file to stderr and returns non-zero.
# Deliberately silent on success -- a wall of matching hashes is not worth
# reading, only the exceptions are.
verify_manifest() {
    device=$1
    remote_base=$2
    manifest=$3

    [ -s "$manifest" ] || return 0

    algo=$(remote_sum_cmd "$device") || {
        echo "no sha256sum or md5sum found on $device" >&2
        return 1
    }

    # Flattened to one space-separated line: the manifest's newlines would
    # otherwise land inside the remote command string as literal newlines,
    # i.e. as separate shell commands instead of separate sha256sum args.
    rels=$(cut -f1 "$manifest" | tr '\n' ' ')
    # Combined stdout+stderr: a missing file makes sha256sum/md5sum print an
    # error line instead of a "digest  path" line, which simply never
    # matches the awk lookup below and so is correctly reported as missing.
    remote_out=$(ssh "$device" "cd '$remote_base' && $algo $rels" 2>&1) || true

    fail=0
    while IFS="$(printf '\t')" read -r rel local_abs; do
        [ -n "$rel" ] || continue
        want=$(local_sum "$algo" "$local_abs")
        got=$(printf '%s\n' "$remote_out" | awk -v p="$rel" '$2 == p { print $1 }')
        if [ -z "$got" ]; then
            echo "MISSING on device: $remote_base/$rel" >&2
            fail=1
        elif [ "$want" != "$got" ]; then
            echo "MISMATCH: $remote_base/$rel (local $want, device $got)" >&2
            fail=1
        fi
    done < "$manifest"
    return $fail
}
