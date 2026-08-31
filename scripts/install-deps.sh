#!/bin/sh
# Getting Node and dsh onto a macOS or Linux machine: the counterpart of
# `install-deps.ps1`, for the one caller that exists on those platforms —
# `src-tauri/src/dsh.rs`, when a launch finds nothing to run.
#
# There is no installer hook here, because neither a .dmg nor an .AppImage has
# one and a .deb's runs as root, which is the wrong user to install a per-user
# Node for. So on these platforms the first launch is what does it, with the
# progress on the loading page — which is now the only path on Windows too: its
# installer used to run this script's counterpart and no longer does.
#
# Nothing here needs root. Node goes under the app's own data directory, npm
# writes to a prefix inside it, and the only thing touched outside is a marked
# block appended to the user's shell profile.
#
# No mode here picks a Node. `switch` and `install-dsh` are handed the one the
# user chose in the app's chooser, and `install-node` is the case where the user
# asked for a fresh one because nothing on the machine would do. Updating is a
# different matter: whoever installed the dsh, it is one `npm install -g` in some
# prefix, so `update` replaces it in place in the prefix it is actually in. What this script installed is written down in
# `bootstrap.json`, which is what keeps `uninstall` off a Node it did not put
# there.
#
# The flags are spelled the way `install-deps.ps1` spells them, because
# `src-tauri/src/dsh.rs` calls both with the same arguments:
#
#   sh install-deps.sh -Mode update|uninstall|list|switch|install-dsh|install-node
#                      [-Prefix <dir>] [-NodeExe <path>]
#                      [-RemoveDsh] [-RemoveNode] [-Progress]
#
# Output is a plain log. Pass `-Progress` and it also emits `::status <text>`,
# `::progress <percent>` and `::error <text>` lines for the app's loading page to
# parse — see `run` in `src-tauri/src/dsh.rs`. Everything goes to stdout: that
# caller reads stdout and throws stderr away.
#
# POSIX sh, no bashisms: on a .deb machine `/bin/sh` is dash.

set -u

# ------------------------------------------------------------------ options --

MODE=''
PREFIX=''
NODE_EXE=''
REMOVE_DSH=0
REMOVE_NODE=0
PROGRESS=0

while [ $# -gt 0 ]; do
    case "$1" in
        -Mode)
            [ $# -ge 2 ] || { echo "-Mode 后面要跟一个模式名" >&2; exit 2; }
            MODE=$2
            shift 2
            ;;
        # `update` only: the npm global prefix holding the dsh to replace. The
        # app resolves it from the copy it is actually running — see `prefix_of`
        # in `src-tauri/src/dsh.rs` — because a dsh the user installed themselves
        # sits in their own prefix, not in the one this script would pick. Empty
        # falls back to the marker's prefix, and then to `find_prefix`.
        -Prefix)
            [ $# -ge 2 ] || { echo "-Prefix 后面要跟一个目录" >&2; exit 2; }
            PREFIX=$2
            shift 2
            ;;
        # `switch` and `install-dsh` only: the `node` the user chose in the
        # setup panel. Everything this script needs — npm, the prefix, the dsh
        # that is or will be there — hangs off it.
        -NodeExe)
            [ $# -ge 2 ] || { echo "-NodeExe 后面要跟一个路径" >&2; exit 2; }
            NODE_EXE=$2
            shift 2
            ;;
        -RemoveDsh) REMOVE_DSH=1; shift ;;
        -RemoveNode) REMOVE_NODE=1; shift ;;
        -Progress) PROGRESS=1; shift ;;
        *) echo "未知参数：$1" >&2; exit 2 ;;
    esac
done

# No default. There used to be one — `install`, a mode that picked a Node itself
# and installed dsh into it — and a bare run of this script is not a thing that
# should still be able to change the machine.
case "$MODE" in
    update|uninstall|list|switch|install-dsh|install-node) ;;
    '') echo "要用 -Mode 指定一个模式" >&2; exit 2 ;;
    *) echo "未知的 -Mode：$MODE" >&2; exit 2 ;;
esac

PACKAGE='@deepseek-ai/dsh'

# The same directory Tauri resolves as `app_local_data_dir()`. The two have to
# agree; see `app_dir` in `src-tauri/src/dsh.rs`.
IDENTIFIER='ai.deepseek.dsh.desktop'
case "$(uname -s)" in
    Darwin) APP_DIR="$HOME/Library/Application Support/$IDENTIFIER" ;;
    *) APP_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/$IDENTIFIER" ;;
esac
NODE_DIR="$APP_DIR/node"
MARKER="$APP_DIR/bootstrap.json"

# Pinned rather than resolved from `latest-v24.x`, so that what a user gets is a
# visible commit here rather than whatever nodejs.org was serving that day.
NODE_VERSION='24.19.0'

# What an existing Node has to be for us to use it instead of installing our
# own. dsh declares no `engines` field itself, but its direct dependency
# commander@15 does — `>=22.12.0` — so anything under that will not run dsh at
# all. Kept a little above that floor rather than pinned to it exactly.
#
# Has to match `$NodeMinimum` in `install-deps.ps1`: this decides whether a
# machine downloads 30 MB of Node it did not need, and the two answering
# differently means the same Node is fine on one platform and not on another.
NODE_MINIMUM='22.19.0'

# The mirrors that carry Node's own layout — same paths, same SHASUMS256.txt.
# Which one is used is decided by measuring them (see `rank_mirrors`); this order
# is only what a machine where nothing could be measured falls back to.
NODE_MIRRORS='https://nodejs.org/dist
https://registry.npmmirror.com/-/binary/node
https://mirrors.aliyun.com/nodejs-release
https://mirrors.huaweicloud.com/nodejs'

# An empty URL is whatever npm resolves on its own, which is the user's own
# `.npmrc` if they have one. Which of these is used is decided by measuring them
# (see `rank_registries`), with the one exception that keeps a `.npmrc` pointing
# somewhere of the user's own choosing: a private mirror or a corporate proxy is
# there for a reason, may be the only route out of the network at all, and so is
# never raced against anything.
REGISTRIES='默认源|
npmmirror|https://registry.npmmirror.com/
腾讯云|https://mirrors.cloud.tencent.com/npm/
华为云|https://mirrors.huaweicloud.com/repository/npm/'

# What an empty URL above resolves to when npm is left to itself and has nothing
# configured. Recognised rather than assumed: it is the difference between a
# default worth racing and a choice worth respecting.
PUBLIC_REGISTRY='https://registry.npmjs.org'

# How long one probe request to a registry may take before that source is written
# off. Two requests per registry and four registries, so this is a quarter of the
# worst case a measurement can cost — and a source that cannot serve 5 KB of
# metadata inside it is not the one to start 185 MB with.
#
# The seconds and bytes below are the Node mirrors', which are measured by how
# fast they serve rather than by how quickly they answer: the whole budget for one
# measurement, and the most it may read. The seconds cover connecting as well as
# reading, because curl's `--max-time` does; a mirror that spends most of them
# answering is marked down by that alone, which is the right answer for a single
# 30 MB download.
#
# The bytes are thrown away, so the cap is what bounds the cost: four mirrors on a
# fast connection spend a few seconds and 8 MB to choose between 30 MB downloads
# that can differ by minutes.
PROBE_TIMEOUT=4
PROBE_SECONDS=3
PROBE_BYTES=2097152

# How many packages a dsh install pulls, for the progress bar to divide by. npm
# reports nothing usable to drive one, so what fills the bar is how many
# tarballs have come back against how many are expected. Approximate by
# construction, and held short of the end rather than allowed to claim more than
# it knows.
PACKAGE_COUNT=600
PROGRESS_CEILING=99

# The block versions up to 0.1.2 appended to the user's shell profile, and how
# it is found again to take it back out. Nothing writes one any more — see
# `clean_path` — but the ones already out there have to be removable.
BEGIN_MARK='# >>> dsh desktop >>>'
END_MARK='# <<< dsh desktop <<<'

# ---------------------------------------------------------------- reporting --

# Everything a human reads goes through these, and never through a function's
# own output: a function that prints a value is read with `$(...)`, and a log
# line landing in one is a path with a sentence glued to the front of it.
say() {
    printf '%s\n' "$1"
}

# A line for the log and, when asked, a line for the loading page. A negative
# percentage puts the progress bar away.
step() {
    say "$1"
    if [ "$PROGRESS" = 1 ]; then
        printf '::status %s\n' "$1"
        printf '::progress %s\n' "${2--1}"
    fi
}

report() {
    if [ "$PROGRESS" = 1 ]; then
        printf '::progress %s\n' "$1"
    fi
}

fail() {
    say "错误：$1"
    if [ "$PROGRESS" = 1 ]; then
        printf '::error %s\n' "$1"
    fi
    exit 1
}

have() {
    command -v "$1" >/dev/null 2>&1
}

# `command -v "$1"`, and then the same question asked of a login, interactive
# shell if that came up empty. A version manager like nvm adds itself in
# `~/.bashrc` or `~/.zshrc`; a GUI launch of this app inherits neither, so a
# node or dsh installed that way is otherwise invisible to this script even
# though a terminal on the same machine finds it fine. Bounded to a few
# seconds in case an rc file hangs on something.
resolve() {
    found=$(command -v "$1" 2>/dev/null)
    if [ -n "$found" ]; then
        printf '%s' "$found"
        return 0
    fi

    shell=${SHELL:-}
    [ -x "$shell" ] || return 1

    out=$(mktemp "${TMPDIR:-/tmp}/dsh-path-XXXXXX") || return 1
    # Single-quoted on purpose: `$PATH` has to expand inside the login shell
    # this starts, once its rc files have had their say, not here.
    # shellcheck disable=SC2016
    "$shell" -ilc 'printf %s "$PATH"' >"$out" 2>/dev/null &
    pid=$!

    n=0
    while kill -0 "$pid" 2>/dev/null && [ "$n" -lt 5 ]; do
        sleep 1
        n=$((n + 1))
    done
    if kill -0 "$pid" 2>/dev/null; then
        kill "$pid" 2>/dev/null
        wait "$pid" 2>/dev/null
        rm -f "$out"
        return 1
    fi
    wait "$pid" 2>/dev/null

    extra=$(cat "$out" 2>/dev/null)
    rm -f "$out"
    [ -n "$extra" ] || return 1

    found=$(PATH="$extra" command -v "$1" 2>/dev/null)
    [ -n "$found" ] || return 1
    printf '%s' "$found"
}

# ------------------------------------------------------------------- marker --

# What this script installed, so that `uninstall` can leave alone what it did
# not. Absent until something is actually installed — a machine that already had
# both Node and dsh gets nothing written and nothing removed.
#
# Written and read by hand rather than with a JSON tool: there is no jq on a
# stock macOS, the file has five string fields, and this is the only thing that
# writes it on these platforms.
M_NODE=''
M_NODE_EXE=''
M_NPM_CLI=''
M_PREFIX=''
M_DSH=''
# The directory a `switch` or `install-node` put on the user's PATH, so a later
# switch can take it back off before adding the new one rather than stacking
# Node directories. Empty when nothing this script did touched PATH.
M_PATH_ENTRY=''

marker_field() {
    [ -f "$MARKER" ] || return 0
    sed -n 's/.*"'"$1"'"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$MARKER" | head -n 1
}

read_marker() {
    M_NODE=$(marker_field node)
    M_NODE_EXE=$(marker_field nodeExe)
    M_NPM_CLI=$(marker_field npmCli)
    M_PREFIX=$(marker_field prefix)
    M_DSH=$(marker_field dsh)
    M_PATH_ENTRY=$(marker_field pathEntry)
}

write_marker() {
    mkdir -p "$APP_DIR" || return 1

    JSON=''
    json_field node "$M_NODE"
    json_field nodeExe "$M_NODE_EXE"
    json_field npmCli "$M_NPM_CLI"
    json_field prefix "$M_PREFIX"
    json_field dsh "$M_DSH"
    json_field pathEntry "$M_PATH_ENTRY"

    printf '{\n%s\n}\n' "$JSON" > "$MARKER"
}

# One field, skipped when empty, with the comma the previous one needs.
json_field() {
    [ -n "$2" ] || return 0
    if [ -n "$JSON" ]; then
        JSON="$JSON,
"
    fi
    JSON="$JSON  \"$1\": \"$2\""
}

# ------------------------------------------------------------------ versions --

# Whether $1 is at least $2, compared field by field as numbers. `sort -V` would
# be shorter and is not on a BSD userland worth relying on.
version_ge() {
    awk -v have="$1" -v want="$2" 'BEGIN {
        n = split(have, a, "."); m = split(want, b, ".");
        for (i = 1; i <= 3; i++) {
            x = (i <= n ? a[i] + 0 : 0); y = (i <= m ? b[i] + 0 : 0);
            if (x > y) exit 0;
            if (x < y) exit 1;
        }
        exit 0
    }'
}

# ------------------------------------------------------------------ probing --

# Which source to use, measured rather than assumed.
#
# Before this the lists above were tried in order and only ever moved on from
# when one had *failed*, so a source that answered slowly was used to the end.
# What follows measures each of them first and starts with the fastest — and
# keeps the rest, in that order, as what to fall back to when the fastest one
# turns out not to work after all.
#
# The two measurements are deliberately different, because the two downloads
# are. Node is one 30 MB archive, where bandwidth is the whole story, so a mirror
# is judged by how fast it actually serves that archive. dsh is 600 requests of
# which its own tarball is 32 KB, where the answer time of each request is the
# story, so a registry is judged by how quickly it serves a fixed small workload
# rather than by a bandwidth figure that would not predict much.
#
# All of it needs curl: wget reports neither a transfer time nor a byte count in
# any form worth parsing, and `date` on a BSD userland has no sub-second field to
# time it by hand with. Without curl the declared order stands, which is what this
# script did all along.
#
# Both ranking functions write their answer to `$RANKED` rather than printing it,
# because they also log while they work and the two must not come back down one
# pipe together.
RANKED=''

# Milliseconds a registry takes over a fixed workload: dsh's own metadata, and
# the tarball that metadata names. Both are small — the dsh package is 32 KB, the
# 185 MB is its dependencies — so what comes out is how quickly this source
# answers, which for an install that makes 600 requests of exactly this shape is
# the number that matters. Around 40 KB per source, against the several MB a
# bandwidth test would have cost. Prints nothing when the source failed.
measure_source() {
    base=${1%/}

    body=$(curl -fsSL --max-time "$PROBE_TIMEOUT" -w '\n%{time_total}' \
        "$base/$PACKAGE/latest" 2>/dev/null) || return 1
    meta_time=$(printf '%s\n' "$body" | tail -n 1)

    # `dist.tarball` as this source itself published it: every mirror rewrites
    # that field onto its own host or CDN, which is where npm would then go, so
    # following it measures the path the install actually takes.
    tarball=$(printf '%s\n' "$body" |
        sed -n 's|.*"tarball"[[:space:]]*:[[:space:]]*"\([^"]*\)".*|\1|p' | head -n 1)
    [ -n "$tarball" ] || return 1

    # Huawei publishes a path rather than a URL — `/@deepseek-ai/dsh/-/dsh-…tgz`
    # — which npm reads relative to the registry it asked. Relative to the
    # registry and not to its host: rooted at the host, Huawei's answers 200 with
    # an 11 KB page that is not the tarball, and a probe measuring that is not
    # measuring the same workload as the others.
    case "$tarball" in
        http*) ;;
        *) tarball="$base/${tarball#/}" ;;
    esac

    tarball_time=$(curl -fsSL --max-time "$PROBE_TIMEOUT" -o /dev/null \
        -w '%{time_total}' "$tarball" 2>/dev/null) || return 1

    awk -v a="$meta_time" -v b="$tarball_time" 'BEGIN { printf "%d", (a + b) * 1000 }'
}

# Bytes per second a Node mirror serves the very archive that is about to be
# downloaded. The read is abandoned after `$PROBE_SECONDS` or `$PROBE_BYTES`,
# whichever comes first — the byte cap is a range request, and a mirror that
# ignores it is stopped by the clock instead. Connecting counts against the
# mirror, since `%{time_total}` covers all of it. Prints nothing when nothing
# arrived.
measure_mirror() {
    # `--max-time` makes curl exit 28 rather than 0, so the exit code says
    # nothing here and the byte count is what decides.
    measured=$(curl -sL --max-time "$PROBE_SECONDS" -r "0-$((PROBE_BYTES - 1))" \
        -o /dev/null -w '%{size_download} %{time_total}' "$1" 2>/dev/null)

    printf '%s\n' "$measured" | awk '{
        if ($1 + 0 <= 0 || $2 + 0 <= 0) exit 1
        printf "%d", $1 / $2
    }'
}

# The Node mirrors in the order to try them, fastest first, with the ones that
# never answered at the back — still there, because a mirror that refuses a probe
# can be the one that ends up serving the file.
rank_mirrors() {
    RANKED=$NODE_MIRRORS
    have curl || return 0

    step '正在测试 Node 镜像的速度…' 0

    scored=''
    while IFS= read -r mirror; do
        host=$(printf '%s' "$mirror" | sed -e 's|^[a-z]*://||' -e 's|/.*$||')
        speed=$(measure_mirror "$mirror/v$NODE_VERSION/$1")
        if [ -z "$speed" ]; then
            say "$host：太慢或连不上"
            speed=0
        else
            # MB/s once there is more than one of them, KB/s below that, where
            # `0.0 MB/s` would be the whole answer for every mirror worth ranking
            # against each other.
            say "$host：约 $(awk -v s="$speed" 'BEGIN {
                if (s >= 1048576) printf "%.1f MB/s", s / 1048576
                else printf "%d KB/s", s / 1024
            }')"
        fi
        scored="$scored$speed|$mirror
"
    done <<MIRRORS
$NODE_MIRRORS
MIRRORS

    # Fastest first, and then the score dropped back off. A mirror that answered
    # with nothing scored zero and so sorts to the back, which is as far as it
    # goes: it is still a mirror to fall back to.
    ordered=$(printf '%s' "$scored" | sort -s -t'|' -k1,1rn)
    RANKED=$(printf '%s\n' "$ordered" | cut -d'|' -f2-)

    best=$(printf '%s\n' "$ordered" | head -n 1)
    if [ "${best%%|*}" != 0 ]; then
        say "最快的是 $(printf '%s' "${best#*|}" | sed -e 's|^[a-z]*://||' -e 's|/.*$||')，就用它。"
    fi
}

# The registries in the order to try them, fastest first.
#
# Not measured at all when npm is configured to a registry of the user's own:
# that one is used first whatever it would have scored, and racing three mirrors
# it is going to beat anyway would only add its own timeouts to an install on a
# network where they are all blocked.
rank_registries() {
    RANKED=$REGISTRIES

    # `$1` and `$2` rather than names of its own: the caller's `node` and `cli` are
    # what would be assigned, and this runs in the middle of its loop.
    configured=$("$1" "$2" config get registry 2>/dev/null | tr -d '\r' | tail -n 1)
    case "${configured%/}" in
        ''|"$PUBLIC_REGISTRY") ;;
        *)
            say "检测到你自己配置的 npm 源（$configured），优先用它，不参与测速。"
            return 0
            ;;
    esac

    have curl || return 0

    step '正在测试各个源的速度…' "$3"

    scored=''
    while IFS= read -r source; do
        label=${source%%|*}
        url=${source#*|}
        # The default source is npm's own, and npm's own is the public registry —
        # the case above is what handles it being anything else.
        ms=$(measure_source "${url:-$PUBLIC_REGISTRY}")
        if [ -z "$ms" ]; then
            say "$label：太慢或连不上"
            # Sorted last rather than dropped: the measurement decides what to
            # start with, not what is allowed to work.
            ms=999999
        else
            say "$label：$(awk -v ms="$ms" 'BEGIN { printf "%.1f", ms / 1000 }') 秒"
        fi
        scored="$scored$ms|$source
"
    done <<SOURCES
$REGISTRIES
SOURCES

    ordered=$(printf '%s' "$scored" | sort -s -t'|' -k1,1n)
    RANKED=$(printf '%s\n' "$ordered" | cut -d'|' -f2-)

    best=$(printf '%s\n' "$ordered" | head -n 1)
    if [ "${best%%|*}" != 999999 ]; then
        # `999999|默认源|` — the label is the field between the score and the URL.
        say "最快的是 $(printf '%s' "$best" | cut -d'|' -f2)，就用它。"
    fi
}

# ---------------------------------------------------------------------- node --

# The version a Node reports, as bare `24.19.0`, printed to stdout; nothing
# printed for one that would not answer. Split out of [`node_is_new_enough`]
# because the setup panel shows the number, not only the verdict, and listing
# every Node on the machine needs the string.
get_node_version() {
    printed=$("$1" --version 2>/dev/null) || return 1
    [ -n "$printed" ] || return 1

    # `v24.19.0`, and the `-nightly...` suffix a prerelease carries.
    printed=${printed#v}
    printed=${printed%%-*}
    printf '%s' "$printed"
}

node_is_new_enough() {
    printed=$(get_node_version "$1") || return 1
    version_ge "$printed" "$NODE_MINIMUM"
}

# The Node an update runs npm with, when the marker's pair is gone or was never
# written. It asks no version question, unlike everything the chooser drives: the
# minimum decides whether a Node is worth *installing dsh into*, and an update
# installs nothing — it replaces a dsh that is already here, with the npm beside whatever
# Node put it there. Refusing a Node a few releases short of the minimum would
# leave exactly that install permanently un-updatable, which is the case this
# whole path exists for.
find_any_node() {
    if [ -x "$NODE_DIR/bin/node" ]; then
        printf '%s' "$NODE_DIR/bin/node"
        return 0
    fi

    found=$(resolve node) || return 1
    printf '%s' "$found"
}

# What the server says the body will be, for the bar to divide by. Its own
# request, because neither curl nor wget will report a total and a body at once
# in a form worth parsing. A mirror that refuses HEAD — Aliyun does — answers 0,
# which costs the bar and nothing else.
content_length() {
    if have curl; then
        curl -fsIL --connect-timeout 15 "$1" 2>/dev/null
    else
        wget -q -S --spider "$1" 2>&1
    fi | tr -d '\r' | awk 'tolower($1) == "content-length:" { n = $2 } END { print n + 0 }'
}

# Download `$1` to `$2`, reporting how far along it is between `$3` and `$4` on
# the caller's scale. The transfer runs in the background and the file it is
# writing is measured once a second, which is the only progress either tool
# offers that is not a repainting bar on a terminal nobody is looking at.
fetch() {
    url=$1
    out=$2
    from=$3
    to=$4

    rm -f "$out"
    total=$(content_length "$url")

    if have curl; then
        curl -fsSL --connect-timeout 30 -o "$out" "$url" &
    elif have wget; then
        wget -q -O "$out" "$url" &
    else
        say '这台机器上既没有 curl 也没有 wget，无法下载。'
        return 1
    fi
    pid=$!

    while kill -0 "$pid" 2>/dev/null; do
        sleep 1
        if [ "$total" -gt 0 ] && [ -f "$out" ]; then
            got=$(wc -c < "$out" 2>/dev/null | tr -d ' ')
            report "$(awk -v f="$from" -v t="$to" -v g="${got:-0}" -v n="$total" \
                'BEGIN { printf "%.1f", f + (t - f) * g / n }')"
        fi
    done

    wait "$pid"
}

sha256_of() {
    if have shasum; then
        shasum -a 256 "$1" | awk '{ print $1 }'
    elif have sha256sum; then
        sha256sum "$1" | awk '{ print $1 }'
    fi
}

# Put a Node under `$NODE_DIR`, from the first mirror that answers with an
# archive whose hash matches what that same mirror published. Prints nothing;
# `$NODE_DIR/bin/node` is the result.
install_node() {
    case "$(uname -s)" in
        Darwin) os=darwin ;;
        Linux) os=linux ;;
        *) fail "不支持的系统：$(uname -s)。" ;;
    esac
    case "$(uname -m)" in
        arm64|aarch64) arch=arm64 ;;
        x86_64|amd64) arch=x64 ;;
        *) fail "不支持的处理器架构：$(uname -m)。" ;;
    esac

    if ! have shasum && ! have sha256sum; then
        fail '这台机器上没有 shasum 或 sha256sum，无法校验下载的 Node。'
    fi

    name="node-v$NODE_VERSION-$os-$arch"
    scratch=$(mktemp -d "${TMPDIR:-/tmp}/dsh-node-XXXXXX") || fail '无法创建临时目录。'

    # Fastest first; `$RANKED` is the declared order on a machine where nothing
    # could be measured.
    rank_mirrors "$name.tar.gz"
    mirrors=$RANKED

    # Fed by a here-document rather than a pipe, so the loop is not a subshell
    # and `installed` survives it.
    installed=0
    while IFS= read -r mirror; do
        base="$mirror/v$NODE_VERSION"
        host=$(printf '%s' "$mirror" | sed -e 's|^[a-z]*://||' -e 's|/.*$||')
        archive="$scratch/$name.tar.gz"
        sums="$scratch/SHASUMS256.txt"

        step "正在下载 Node $NODE_VERSION（$host）…" 0
        if ! fetch "$base/$name.tar.gz" "$archive" 0 30; then
            say "从 $mirror 下载 Node 失败。"
            continue
        fi

        step '正在校验 Node…' 32
        if ! fetch "$base/SHASUMS256.txt" "$sums" 32 33; then
            say "从 $mirror 取校验和失败。"
            continue
        fi

        wanted=$(awk -v file="$name.tar.gz" \
            '$2 == file || $2 == "*" file { print $1; exit }' "$sums")
        if [ -z "$wanted" ]; then
            say "这个源没有发布 $name.tar.gz 的校验和。"
            continue
        fi
        if [ "$(sha256_of "$archive")" != "$wanted" ]; then
            say '下载的 Node 校验和不匹配。'
            continue
        fi

        step '正在解压 Node…' 35
        if ! tar -xzf "$archive" -C "$scratch"; then
            say '解压 Node 失败。'
            continue
        fi

        # Replaced rather than merged: a half-unpacked tree from an earlier
        # attempt would otherwise survive underneath the new one.
        rm -rf "$NODE_DIR"
        mkdir -p "$APP_DIR"
        if ! mv "$scratch/$name" "$NODE_DIR"; then
            say '无法把 Node 移动到应用目录。'
            continue
        fi

        installed=1
        break
    done <<MIRRORS
$mirrors
MIRRORS

    rm -rf "$scratch"

    if [ "$installed" = 0 ]; then
        fail "无法下载 Node $NODE_VERSION。已尝试 nodejs.org 和几个国内镜像，都没有成功，通常是网络或代理的问题。"
    fi

    say "Node 已安装到 $NODE_DIR"
}

# ----------------------------------------------------------------------- npm --

# npm's own entry point next to `$1`, to be run through Node rather than through
# the `npm` shim: no dependency on how the machine happens to resolve `npm`, and
# the pair is what gets written down for the app to reuse.
find_npm() {
    dir=$(dirname "$1")
    for candidate in \
        "$dir/../lib/node_modules/npm/bin/npm-cli.js" \
        "$dir/node_modules/npm/bin/npm-cli.js"
    do
        if [ -f "$candidate" ]; then
            # Read again through `cd`, so the recorded path has no `..` in it —
            # the app compares it against what it finds on disk.
            printf '%s' "$(cd "$(dirname "$candidate")" && pwd)/npm-cli.js"
            return 0
        fi
    done
    return 1
}

# Where `npm install -g` will be told to put things.
#
# For a Node this script installed that is the Node directory. For the machine's
# own Node it is whatever npm has configured — unless that is somewhere only
# root can write, which is the usual case for a distribution's `/usr` Node, and
# then it is a prefix of our own under the app directory. Installing as the user
# who will run it is the whole point; asking for a password is not on the table.
find_prefix() {
    node=$1
    cli=$2

    case "$node" in
        "$NODE_DIR"/*) printf '%s' "$NODE_DIR"; return 0 ;;
    esac

    prefix=$("$node" "$cli" prefix -g 2>/dev/null)
    if [ -n "$prefix" ] && mkdir -p "$prefix/lib/node_modules" 2>/dev/null &&
        [ -w "$prefix/lib/node_modules" ] && mkdir -p "$prefix/bin" 2>/dev/null &&
        [ -w "$prefix/bin" ]
    then
        printf '%s' "$prefix"
        return 0
    fi

    printf '%s' "$APP_DIR/npm"
}

# One `npm install -g`, from one registry. npm's http log is read as it goes:
# every tarball that comes back moves the bar, which is the only progress signal
# npm offers that means anything.
#
# `$?` has to travel out of a pipeline, which sh has no `pipefail` for, so the
# installer writes it to a file the caller reads.
npm_install() {
    node=$1
    cli=$2
    prefix=$3
    registry=$4
    from=$5
    to=$6

    code_file="$APP_DIR/.npm-exit"
    mkdir -p "$APP_DIR"
    rm -f "$code_file"

    set -- "$cli" install -g --prefix "$prefix" --no-audit --no-fund --loglevel=http
    if [ -n "$registry" ]; then
        set -- "$@" "--registry=$registry"
    fi
    set -- "$@" "$PACKAGE@latest"

    {
        # stdin off the null device, not inherited: the caller's loop is reading
        # its registry list from a here-document, and a child that touched stdin
        # would eat the rest of it.
        #
        # PATH, because npm runs a dependency's `install` script through `sh -c`
        # and the ones that build something spell it `node ...` — resolved off
        # PATH, not from the Node running npm. A GUI launch inherits neither a
        # version manager's PATH nor, of course, the Node this script just
        # unpacked, so without this every package with a build step dies with
        # `sh: 1: node: not found`; koffi and node-pty both do, and npm then
        # rolls the whole install back.
        PATH="$(dirname "$node"):$PATH" "$node" "$@" 2>&1 < /dev/null
        printf '%s\n' "$?" > "$code_file"
    } | {
        # A subshell of its own: the count only has to live as long as the pipe,
        # and nothing after it reads the count.
        fetched=0
        while IFS= read -r line; do
            say "$line"
            case "$line" in
                *'npm http fetch GET 200 '*|*'npm http cache '*)
                    fetched=$((fetched + 1))
                    percent=$((from + (to - from) * fetched / PACKAGE_COUNT))
                    if [ "$percent" -gt "$PROGRESS_CEILING" ]; then
                        percent=$PROGRESS_CEILING
                    fi
                    report "$percent"
                    ;;
            esac
        done
    }

    code=$(cat "$code_file" 2>/dev/null)
    rm -f "$code_file"
    [ "${code:-1}" = 0 ]
}

# Install through the fastest registry that works.
install_package() {
    node=$1
    cli=$2
    prefix=$3
    from=$4
    to=$5

    rank_registries "$node" "$cli" "$from"
    sources=$RANKED

    while IFS= read -r source; do
        label=${source%%|*}
        url=${source#*|}

        step "正在安装 dsh（$label）…" "$from"
        if npm_install "$node" "$cli" "$prefix" "$url" "$from" "$to"; then
            say "dsh 安装完成（$label）。"
            return 0
        fi
        say "从 $label 安装失败，换下一个源重试。"
    done <<SOURCES
$sources
SOURCES

    return 1
}

# ---------------------------------------------------------------------- path --

# Nothing here writes to the user's PATH, and that is deliberate.
#
# The app does not need it: `dsh.rs` finds Node and dsh through `bootstrap.json`
# and puts them in front of the PATH it hands the child itself — see
# `search_path` and `apply_path` there. The only thing a PATH entry ever bought
# was a bare `dsh` working in the user's own terminal, and the price was steep:
# the directory that had to go on it is the Node directory, so it shadowed
# whatever `node`, `npm` and `npx` a version manager like nvm had put there, and
# a shim resolving `node` off PATH could then be paired with a Node of a
# different major version than the one its native modules were built for.
#
# It was also written and never taken back. `-Mode uninstall` has no caller on
# these platforms — the .deb's uninstall runs as root and neither the .dmg nor
# the .AppImage has one at all — so up to 0.1.2 the block went into the profile
# and stayed there forever.
#
# What replaces it is a line of output saying where dsh is, for a user who wants
# it in their terminal to act on themselves.

# The files a login or interactive shell of the user's actually reads.
#
# `~/.profile` always. `~/.zshrc` as well for a zsh user, since that is what
# macOS runs and a stock account has no `~/.profile` in the loop. `~/.bashrc`
# only if it already exists.
profile_files() {
    printf '%s\n' "$HOME/.profile"
    case "${SHELL:-}" in
        */zsh) printf '%s\n' "$HOME/.zshrc" ;;
    esac
    if [ -f "$HOME/.bashrc" ]; then
        printf '%s\n' "$HOME/.bashrc"
    fi
}

# Take out the block a version up to 0.1.2 left behind, and the one the
# interactive chooser briefly wrote back before this. Nothing writes one any
# more. Run on every mode that used to add one rather than only on uninstall,
# because on these platforms uninstall never runs: an upgrade is the one moment
# this is reachable at all.
remove_path() {
    profile_files | while IFS= read -r file; do
        [ -f "$file" ] || continue
        grep -qF "$BEGIN_MARK" "$file" || continue

        if awk -v begin="$BEGIN_MARK" -v end="$END_MARK" '
            $0 == begin { skip = 1 }
            skip == 0 { print }
            $0 == end { skip = 0 }
        ' "$file" > "$file.dsh-tmp" 2>/dev/null; then
            mv "$file.dsh-tmp" "$file"
            say "已把 dsh 的 PATH 设置移出 $file。"
        else
            rm -f "$file.dsh-tmp"
        fi
    done
}

# --------------------------------------------------------------------- modes --

# Whether the machine can already run dsh. The prefix from a previous run comes
# first: it is on the user's PATH but not necessarily on this process's, which
# inherited its environment before that line was ever written.
find_dsh() {
    if [ -n "$M_PREFIX" ] && [ -x "$M_PREFIX/bin/dsh" ]; then
        printf '%s' "$M_PREFIX/bin/dsh"
        return 0
    fi

    found=$(resolve dsh) || return 1
    printf '%s' "$found"
}

# The npm global prefix a dsh lives in, for `npm uninstall -g --prefix` to
# unpick. Prints nothing and fails when there is no prefix to point npm at.
#
# The marker's prefix first — that is where this script installed — and then the
# dsh on PATH, whose prefix is read off the layout `npm install -g` leaves: the
# shim in `<prefix>/bin`, the package under `<prefix>/lib/node_modules`. Anything
# else is not an npm global install, and npm cannot remove it.
find_dsh_prefix() {
    manifest="lib/node_modules/$PACKAGE/package.json"

    if [ -n "$M_PREFIX" ] && [ -f "$M_PREFIX/$manifest" ]; then
        printf '%s' "$M_PREFIX"
        return 0
    fi

    # `$1` when the caller already has one. `find_dsh` can cost a login shell,
    # and a caller that has already paid for it should not pay twice.
    found=${1:-}
    [ -n "$found" ] || found=$(find_dsh) || return 1
    prefix=$(cd "$(dirname "$found")/.." 2>/dev/null && pwd) || return 1
    [ -f "$prefix/$manifest" ] || return 1
    printf '%s' "$prefix"
}

update_all() {
    read_marker

    # The pair that installed dsh, if it is still there: any npm can write into
    # the prefix it is handed, but this one is known to work on this machine.
    node=$M_NODE_EXE
    cli=$M_NPM_CLI
    if [ ! -x "${node:-/nonexistent}" ] || [ ! -f "${cli:-/nonexistent}" ]; then
        node=$(find_any_node) || fail '这台机器上找不到 Node，无法更新 dsh。'
        cli=$(find_npm "$node") || fail "这个 Node 旁边没有 npm（$node）。"
        say "用 $node 更新 dsh。"
    fi

    # The prefix the dsh being replaced actually lives in — `-Prefix` from the
    # app, or the one this script installed into. Without either, the one an
    # install would pick, which is only right when dsh is already there.
    prefix=$PREFIX
    [ -n "$prefix" ] || prefix=$M_PREFIX
    [ -n "$prefix" ] || prefix=$(find_prefix "$node" "$cli")

    step '正在更新 dsh…' 0
    if ! install_package "$node" "$cli" "$prefix" 0 "$PROGRESS_CEILING"; then
        fail 'dsh 更新失败，默认源和几个备用镜像都没有成功。'
    fi
    step 'dsh 更新完成。' 100
}

uninstall_all() {
    read_marker

    # Only a Node of ours goes: one the machine already had is not this script's
    # to take, whatever the flags say. dsh has no such reservation — it is one
    # `npm install -g` either way, and the caller has just been asked about it by
    # name.
    #
    # Node is also a Node program's only way to run, so taking it away while
    # leaving dsh behind would leave a `dsh` command that cannot start.
    drop_node=0
    drop_dsh=0
    [ "$REMOVE_NODE" = 1 ] && [ "$M_NODE" = managed ] && drop_node=1
    { [ "$REMOVE_DSH" = 1 ] || [ "$drop_node" = 1 ]; } && drop_dsh=1

    if [ "$REMOVE_NODE" = 1 ] && [ "$drop_node" = 0 ]; then
        say 'Node 是你自己装的，不会动它。'
    fi

    if [ "$drop_dsh" = 1 ]; then
        prefix=$(find_dsh_prefix) || prefix=''

        # Both of these are deleted outright when the Node goes; see below.
        inside=0
        case "$prefix" in "$NODE_DIR" | "$NODE_DIR"/* | "$APP_DIR"/*) inside=1 ;; esac

        if [ -z "$prefix" ]; then
            say '找不到 dsh 装在哪里（不是 npm 全局安装？），跳过卸载 dsh。'
        elif [ "$drop_node" = 1 ] && [ "$inside" = 1 ]; then
            # It lives inside the directory about to be deleted, and asking npm
            # to walk 33k files first would only be slower.
            say 'dsh 装在即将删除的 Node 目录里，会随它一起删掉。'
        elif [ -x "${M_NODE_EXE:-/nonexistent}" ] && [ -f "${M_NPM_CLI:-/nonexistent}" ]; then
            # npm has to unpick its own tree, pointed at the prefix holding it
            # rather than at whatever this run would default to.
            [ "$M_DSH" = managed ] || say '这份 dsh 不是本应用装的，按你的选择一并卸载。'
            say "正在卸载 dsh（$prefix）…"
            "$M_NODE_EXE" "$M_NPM_CLI" uninstall -g --prefix "$prefix" \
                --loglevel=error "$PACKAGE" 2>&1 | while IFS= read -r line; do say "$line"; done
        else
            say '找不到可用的 npm，跳过卸载 dsh。'
        fi
    fi

    if [ "$drop_node" = 1 ]; then
        remove_path
        say '正在删除 Node 和 dsh…'
        rm -rf "$NODE_DIR"
        # Only ours: a prefix under `$APP_DIR` is one this script made, and
        # anywhere else belongs to the machine's own npm.
        case "$M_PREFIX" in
            "$APP_DIR"/*) rm -rf "$M_PREFIX" ;;
        esac
        rm -f "$MARKER"
        return 0
    fi

    if [ "$drop_dsh" = 1 ]; then
        M_DSH=''
        write_marker
    fi
}

# -------------------------------------------------------------- interactive --
#
# The four modes below are the chooser the app shows when a launch finds no dsh.
# `list` is the only one that does no installing: it walks every Node the machine
# has and prints what it found as one JSON line, for `setup.rs` to turn into a
# panel. The other three act on the choice the user made there.

# One candidate Node, recorded against a `seen` file so the same `node` is not
# listed twice. The realpath is the key — nvm's `current` symlink, a Homebrew
# `opt` link, and the Cellar directory itself all reach the same binary — while
# the path shown to the user stays the one it was found at. `$SEEN_FILE` is set
# by `find_all_nodes`; `$1` is the node binary, `$2` where it came from.
_consider() {
    [ -n "$1" ] || return 0
    [ -x "$1" ] || return 0
    _real=$(readlink -f "$1" 2>/dev/null || realpath "$1" 2>/dev/null || printf '%s' "$1")
    if grep -qxF "$_real" "$SEEN_FILE" 2>/dev/null; then
        return 0
    fi
    printf '%s\n' "$_real" >> "$SEEN_FILE"
    printf '%s\t%s\n' "$1" "$2"
}

# The user's PATH as a login interactive shell sees it, bounded to a few seconds
# in case an rc file hangs — the same shape `resolve` uses. A version manager
# like nvm adds itself in `~/.bashrc` or `~/.zshrc`; a GUI launch of this app
# inherits neither, so a Node installed that way is otherwise invisible to
# `find_all_nodes` even though a terminal on the same machine finds it fine.
login_shell_path() {
    _shell=${SHELL:-}
    [ -x "$_shell" ] || return 1

    _out=$(mktemp "${TMPDIR:-/tmp}/dsh-path-XXXXXX") || return 1
    # Single-quoted on purpose: `$PATH` has to expand inside the login shell
    # this starts, once its rc files have had their say, not here.
    # shellcheck disable=SC2016
    "$_shell" -ilc 'printf %s "$PATH"' >"$_out" 2>/dev/null &
    _pid=$!

    _n=0
    while kill -0 "$_pid" 2>/dev/null && [ "$_n" -lt 5 ]; do
        sleep 1
        _n=$((_n + 1))
    done
    if kill -0 "$_pid" 2>/dev/null; then
        kill "$_pid" 2>/dev/null
        wait "$_pid" 2>/dev/null
        rm -f "$_out"
        return 1
    fi
    wait "$_pid" 2>/dev/null

    _extra=$(cat "$_out" 2>/dev/null)
    rm -f "$_out"
    [ -n "$_extra" ] || return 1
    printf '%s' "$_extra"
}

# Every Node on the machine, each tagged with where it was found, one
# `exe<TAB>source` per line. The list is deliberately wide: a machine that
# switched version managers leaves the old one's Nodes behind, and the one the
# user wants may be any of them.
find_all_nodes() {
    SEEN_FILE=$(mktemp "${TMPDIR:-/tmp}/dsh-seen-XXXXXX") || return 0

    _consider "$NODE_DIR/bin/node" managed

    # PATH, as this process inherited it.
    _oldifs=$IFS
    IFS=:
    for _dir in $PATH; do
        IFS=$_oldifs
        [ -n "$_dir" ] && _consider "$_dir/node" path
    done
    IFS=$_oldifs
    # Plus whatever a login shell would add — nvm, fnm, asdf and friends.
    _extra=$(login_shell_path) || _extra=''
    if [ -n "$_extra" ]; then
        _oldifs=$IFS
        IFS=:
        for _dir in $_extra; do
            IFS=$_oldifs
            [ -n "$_dir" ] && _consider "$_dir/node" shell
        done
        IFS=$_oldifs
    fi

    # nvm keeps every installed version under its versions directory.
    _nvm_dir=${NVM_DIR:-$HOME/.nvm}
    if [ -d "$_nvm_dir/versions/node" ]; then
        for _d in "$_nvm_dir"/versions/node/v*; do
            [ -d "$_d" ] && _consider "$_d/bin/node" nvm
        done
    fi

    # fnm stores each version under its data directory.
    case "$(uname -s)" in
        Darwin) _fnm_root="$HOME/Library/Application Support/fnm/node-versions" ;;
        *) _fnm_root="${XDG_DATA_HOME:-$HOME/.local/share}/fnm/node-versions" ;;
    esac
    if [ -d "$_fnm_root" ]; then
        for _d in "$_fnm_root"/*/installation; do
            [ -d "$_d" ] && _consider "$_d/bin/node" fnm
        done
    fi

    # volta keeps Node images under its tools directory.
    if [ -d "$HOME/.volta/tools/image/node" ]; then
        for _d in "$HOME/.volta/tools/image/node"/*; do
            [ -d "$_d" ] && _consider "$_d/bin/node" volta
        done
    fi

    # asdf keeps installed Node versions under its installs directory.
    if [ -d "$HOME/.asdf/installs/nodejs" ]; then
        for _d in "$HOME/.asdf/installs/nodejs"/*; do
            [ -d "$_d" ] && _consider "$_d/bin/node" asdf
        done
    fi

    # Homebrew (mac): both the Apple Silicon and Intel prefixes, Cellar laid out
    # as node/<version>. The `opt/node` symlinks are on PATH already and dedupe
    # against these by realpath.
    for _base in /opt/homebrew /usr/local; do
        if [ -d "$_base/Cellar" ]; then
            for _d in "$_base"/Cellar/node*/[0-9]*; do
                [ -d "$_d" ] && _consider "$_d/bin/node" homebrew
            done
        fi
    done

    # System and snap installs, where nothing above found one.
    _consider /usr/bin/node system
    _consider /usr/local/bin/node system
    _consider /opt/node/bin/node system
    _consider /snap/bin/node snap

    rm -f "$SEEN_FILE"
}

# Escape a string for a JSON string literal: backslash and double quote are the
# only two a Unix path or a version can contain that JSON cares about.
json_escape() {
    printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g'
}

# The actual `npm prefix -g` for a Node — where dsh already is, or would be —
# without the writable fallback `find_prefix` adds. That fallback is right for
# *installing* (it picks somewhere this user can write), but for *detecting*
# whether a Node already has dsh it is wrong: a system Node whose prefix is
# `/usr/local` keeps its dsh there even though this user cannot write to it, and
# `find_prefix` would answer `$APP_DIR/npm` and report no dsh.
real_prefix() {
    _rp=$("$1" "$2" prefix -g 2>/dev/null) || _rp=''
    [ -n "$_rp" ] || _rp=$(dirname "$1")
    printf '%s' "$_rp"
}

# One Node's worth of the chooser's payload, as a single-line JSON object.
node_json() {
    _p=$(json_escape "$1")
    _v=$(json_escape "$2")
    _pr=$(json_escape "$4")
    _dv=$(json_escape "$6")
    _s=$(json_escape "$7")
    if [ -n "$_pr" ]; then _prjson="\"$_pr\""; else _prjson='null'; fi
    if [ -n "$_dv" ]; then _dvjson="\"$_dv\""; else _dvjson='null'; fi
    printf '{"path":"%s","version":"%s","meetsMinimum":%s,"prefix":%s,"hasDsh":%s,"dshVersion":%s,"source":"%s"}' \
        "$_p" "$_v" "$3" "$_prjson" "$5" "$_dvjson" "$_s"
}

# Print one JSON line: an array of every usable Node on the machine. Broken
# Nodes — ones that will not answer `--version` — are skipped, since the chooser
# has nothing to offer for one and no install can target it.
list_nodes() {
    _tmp=$(mktemp "${TMPDIR:-/tmp}/dsh-list-XXXXXX") || { printf '[]\n'; return; }
    find_all_nodes | while IFS='	' read -r _exe _src; do
        [ -n "$_exe" ] || continue
        _version=$(get_node_version "$_exe") || continue
        [ -n "$_version" ] || continue
        if version_ge "$_version" "$NODE_MINIMUM"; then _meets=true; else _meets=false; fi
        _cli=$(find_npm "$_exe") || _cli=''
        _prefix=''
        _hasdsh=false
        _dshver=''
        if [ -n "$_cli" ]; then
            _prefix=$(real_prefix "$_exe" "$_cli")
            _manifest="$_prefix/lib/node_modules/$PACKAGE/package.json"
            if [ -f "$_manifest" ]; then
                _hasdsh=true
                _dshver=$(sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$_manifest" | head -n 1)
            fi
        fi
        node_json "$_exe" "$_version" "$_meets" "$_prefix" "$_hasdsh" "$_dshver" "$_src"
        printf '\n'
    done > "$_tmp"
    _joined=$(paste -sd, "$_tmp" 2>/dev/null)
    rm -f "$_tmp"
    printf '[%s]\n' "$_joined"
}

# Adopt a Node that already has dsh: nothing to download, nothing installed,
# just a marker recording which one so the app can find it.
#
# The marker is a fallback, not an override. If the Node the user picked is the
# one their version manager currently points at, the app finds its dsh on PATH
# and never reads this; the marker is what makes a Node they picked but have not
# switched to reachable at all. See `search_path` in `dsh.rs`.
switch_node() {
    [ -n "$NODE_EXE" ] || fail 'switch 需要 -NodeExe。'
    [ -x "$NODE_EXE" ] || fail "找不到指定的 Node：$NODE_EXE"

    # The floor, checked here and not only in the panel that offered the button.
    # A dsh already installed into a Node below it still cannot run: dsh's direct
    # dependency commander@15 declares `>=22.12.0`, and its native modules (koffi,
    # node-pty) are built against one Node ABI. Adopting such a Node would write a
    # marker the app then boots from, and the failure would surface as a dsh that
    # will not start rather than as the version problem it is.
    node_is_new_enough "$NODE_EXE" || fail "这个 Node 的版本低于 dsh 需要的 $NODE_MINIMUM，无法用它运行 dsh。"
    cli=$(find_npm "$NODE_EXE") || fail "这个 Node 旁边没有 npm（$NODE_EXE）。"
    prefix=$(real_prefix "$NODE_EXE" "$cli")
    manifest="$prefix/lib/node_modules/$PACKAGE/package.json"
    [ -f "$manifest" ] || fail "这个 Node 里没有安装 dsh（$prefix），无法直接切换。"

    read_marker
    nodedir=$(dirname "$NODE_EXE")

    # Nothing goes into the user's shell profiles — see the note above
    # `remove_path`. What is left is taking out the block an earlier version of
    # this mode wrote.
    remove_path

    M_NODE_EXE=$NODE_EXE
    M_NPM_CLI=$cli
    M_PREFIX=$prefix
    M_PATH_ENTRY=''
    [ -n "$M_NODE" ] || M_NODE=system
    [ -n "$M_DSH" ] || M_DSH=system
    write_marker
    say "应用会使用 $nodedir 里的 Node 和 dsh。"
    say "终端里用的还是你的版本管理器当前指定的那个 Node，本应用不会去改它。"
}

# `npm install -g @deepseek-ai/dsh` into a Node the user picked, and record it in
# the marker. The mirror racing and progress reporting are the existing
# `install_package`'s; this is only the framing around it.
#
# Nothing is made "active" beyond that: the user's PATH is not written, so if
# this is the Node their version manager already points at, the app and their
# terminal now share one dsh, and if it is not, the marker is what lets the app
# reach it anyway.
install_dsh_into() {
    [ -n "$NODE_EXE" ] || fail 'install-dsh 需要 -NodeExe。'
    [ -x "$NODE_EXE" ] || fail "找不到指定的 Node：$NODE_EXE"

    # The floor, checked here and not only in the panel that offered the button.
    # A dsh already installed into a Node below it still cannot run: dsh's direct
    # dependency commander@15 declares `>=22.12.0`, and its native modules (koffi,
    # node-pty) are built against one Node ABI. Adopting such a Node would write a
    # marker the app then boots from, and the failure would surface as a dsh that
    # will not start rather than as the version problem it is.
    node_is_new_enough "$NODE_EXE" || fail "这个 Node 的版本低于 dsh 需要的 $NODE_MINIMUM，无法用它运行 dsh。"
    cli=$(find_npm "$NODE_EXE") || fail "这个 Node 旁边没有 npm（$NODE_EXE），无法安装 dsh。"
    prefix=$(find_prefix "$NODE_EXE" "$cli")

    read_marker
    M_NODE_EXE=$NODE_EXE
    M_NPM_CLI=$cli
    [ -n "$M_NODE" ] || M_NODE=system
    write_marker

    step '正在下载 dsh，约 185 MB，请耐心等待…' 0
    if ! install_package "$NODE_EXE" "$cli" "$prefix" 0 "$PROGRESS_CEILING"; then
        fail 'dsh 下载失败。已尝试默认源和 npmmirror、腾讯云、华为云三个镜像，都没有成功，通常是网络或代理的问题。'
    fi

    remove_path
    M_DSH=managed
    M_PREFIX=$prefix
    M_PATH_ENTRY=''
    write_marker
    step 'dsh 安装完成。' 100
}

# The no-Node-at-all case: download one of ours, then install dsh into it. The
# only mode that installs a Node, and it runs because the user pressed the button
# that says so — nothing here decides on its own that the machine needs one.
install_node_and_dsh() {
    read_marker
    # A previous switch's PATH block points at a Node the user is replacing in
    # their mind with our own. Nothing writes one any more; this takes out what
    # an earlier version left.
    remove_path

    say "没有可用的 Node，正在为你安装 Node $NODE_VERSION。"
    install_node
    node="$NODE_DIR/bin/node"
    cli=$(find_npm "$node") || fail "这个 Node 旁边没有 npm（$node）。"

    M_NODE_EXE=$node
    M_NPM_CLI=$cli
    M_NODE=managed
    write_marker

    prefix=$(find_prefix "$node" "$cli")
    step '正在下载 dsh，约 185 MB，请耐心等待…' 36
    if ! install_package "$node" "$cli" "$prefix" 36 "$PROGRESS_CEILING"; then
        fail 'dsh 下载失败。已尝试默认源和 npmmirror、腾讯云、华为云三个镜像，都没有成功，通常是网络或代理的问题。'
    fi

    M_DSH=managed
    M_PREFIX=$prefix
    M_PATH_ENTRY=''
    write_marker
    step 'dsh 安装完成。' 100
}

case "$MODE" in
    update) update_all ;;
    uninstall) uninstall_all ;;
    list) list_nodes ;;
    switch) switch_node ;;
    install-dsh) install_dsh_into ;;
    install-node) install_node_and_dsh ;;
esac

exit 0
