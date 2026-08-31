#!/bin/sh
# Getting Node and dsh onto a macOS or Linux machine: the counterpart of
# `install-deps.ps1`, for the one caller that exists on those platforms —
# `src-tauri/src/dsh.rs`, when a launch finds nothing to run.
#
# There is no installer hook here, because neither a .dmg nor an .AppImage has
# one and a .deb's runs as root, which is the wrong user to install a per-user
# Node for. So on these platforms the first launch is what does it, with the
# progress on the loading page — the same path Windows falls back to when its
# installer could not reach the network.
#
# Nothing here needs root, and nothing outside the app's own data directory is
# written except one symlink into `~/.local/bin`.
#
# The app owns its runtime outright, which is what keeps the rest of this short:
#
#     <AppDir>/runtime/node/                  a pinned Node, ours alone
#     <AppDir>/runtime/node_modules/          dsh and pnpm, a *local* install
#     <AppDir>/bin/dsh                        the launcher, linked into PATH
#
# Nothing is detected and nothing is shared. A Node the machine already has is
# not reused, a dsh the user installed themselves is never touched, and npm's
# global prefix is not involved at all — so no version manager can move any of
# it, and every path above is a constant. There is no marker file: the old
# `bootstrap.json` is read once by `migrate_legacy` and never written again.
#
# The launcher is written here rather than left to npm because npm's generated
# shim resolves `node` off PATH, which is the exact coupling this layout exists
# to remove. `write_launcher` hard-codes ours.
#
# The flags are spelled the way `install-deps.ps1` spells them, because
# `src-tauri/src/dsh.rs` calls both with the same arguments:
#
#   sh install-deps.sh -Mode install|update|uninstall [-Progress]
#
# Output is a plain log. Pass `-Progress` and it also emits `::status <text>`,
# `::progress <percent>` and `::error <text>` lines for the app's loading page to
# parse — see `run` in `src-tauri/src/dsh.rs`. Everything goes to stdout: that
# caller reads stdout and throws stderr away.
#
# POSIX sh, no bashisms: on a .deb machine `/bin/sh` is dash.

set -u

# ------------------------------------------------------------------ options --

MODE=install
PROGRESS=0

while [ $# -gt 0 ]; do
    case "$1" in
        -Mode)
            [ $# -ge 2 ] || { echo "-Mode 后面要跟 install、update 或 uninstall" >&2; exit 2; }
            MODE=$2
            shift 2
            ;;
        -Progress) PROGRESS=1; shift ;;
        *) echo "未知参数：$1" >&2; exit 2 ;;
    esac
done

case "$MODE" in
    install|update|uninstall) ;;
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
# Everything below is a constant, and that is the point. `dsh.rs` derives the
# same paths from `app_local_data_dir()` without reading anything — see
# `runtime`, `node_dir` and `entry` there. The two have to agree.
RUNTIME_DIR="$APP_DIR/runtime"
NODE_DIR="$RUNTIME_DIR/node"
BIN_DIR="$APP_DIR/bin"

# npm's own entry point beside our Node. A constant now that the Node is always
# ours; the Unix tarballs put it under `lib`, where the Windows zip does not.
NPM_CLI="$NODE_DIR/lib/node_modules/npm/bin/npm-cli.js"

# Where the launcher is linked so a terminal can find it. Most Linux
# distributions already have this on PATH; macOS does not, which `link_launcher`
# says out loud rather than editing a shell profile to fix.
LINK_DIR="$HOME/.local/bin"

# What the old scheme left behind, for `migrate_legacy` to clear away.
LEGACY_NODE_DIR="$APP_DIR/node"
LEGACY_NPM_DIR="$APP_DIR/npm"
LEGACY_MARKER="$APP_DIR/bootstrap.json"

# Pinned rather than resolved from `latest-v24.x`, so that what a user gets is a
# visible commit here rather than whatever nodejs.org was serving that day.
NODE_VERSION='24.19.0'

# There is no minimum-version floor any more, and no version check to go with
# it. The floor existed to decide whether a Node the machine already had was
# good enough to install dsh with; nothing is asked of the machine's Node now,
# because nothing uses it. `$NODE_VERSION` above is what runs dsh, always.
#
# That also settles a coupling the floor could never have caught: dsh's native
# modules (koffi, node-pty) are built against one Node's ABI and will not load
# on another's. Pinning the Node pins the ABI for the life of the install.

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

# `resolve`, `marker_field` and `version_ge` are gone with what used them.
#
# `resolve` asked `command -v`, and then asked a login interactive shell the
# same question when that came up empty — a whole `$SHELL -ilc` with a five
# second watchdog, because a version manager puts itself in `~/.bashrc` and a
# GUI launch inherits neither that nor anything else a terminal would have.
# Nothing needs it now: the app never looks for the machine's node or dsh.
#
# `marker_field` read `bootstrap.json`, and `version_ge` compared a found Node
# against a minimum. There is no marker and no floor; see `migrate_legacy` and
# the note where `NODE_MINIMUM` used to be.

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

# Whether our Node is unpacked and runnable. The whole of what used to be
# `find_node`, `find_any_node` and `node_is_new_enough`: there is one Node this
# script will ever use, at one path, so the only question left is whether it is
# there.
node_ready() {
    [ -x "$NODE_DIR/bin/node" ] && [ -f "$NPM_CLI" ]
}

# Whether the Node in `$NODE_DIR` is the pinned one. A `$NODE_VERSION` bump in a
# new release of this app has to replace it, and the unpacked tree no longer
# carries its version in the path — `install_node` renames it — so the version is
# stamped beside it instead.
node_current() {
    [ -f "$NODE_DIR/.dsh-node-version" ] || return 1
    [ "$(cat "$NODE_DIR/.dsh-node-version" 2>/dev/null)" = "$NODE_VERSION" ]
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

    # Whether there is a tree here to replace, read before anything deletes
    # it. A replacement invalidates more than the Node; see the end of this
    # function.
    replacing=0
    [ -e "$NODE_DIR" ] && replacing=1

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
        mkdir -p "$RUNTIME_DIR"
        if ! mv "$scratch/$name" "$NODE_DIR"; then
            say '无法把 Node 移动到应用目录。'
            continue
        fi

        # What `node_current` reads. The unpacked tree carries its version only
        # in the directory name the archive came with, and that name is gone the
        # moment it is renamed to `$NODE_DIR`.
        printf '%s' "$NODE_VERSION" > "$NODE_DIR/.dsh-node-version"

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

    # The Node these were built against is gone, so they have to go too.
    #
    # Nothing else would notice. `npm install` decides from the lockfile and
    # the tree that what it was asked for is already there, and dsh's native
    # modules — koffi, node-pty — are then loaded by ABI at require time, a
    # long way from here and with nothing to connect the failure back to a
    # Node version that changed. Both ship prebuilds covering several ABIs,
    # so this would usually survive; a package that fell back to compiling
    # would not, and "usually" is not a thing to leave in the boot path.
    #
    # Only on a replacement. A first install has nothing here to throw away.
    if [ "$replacing" = 1 ]; then
        say '自带的 Node 版本变了，正在清掉按上一版装好的依赖…'
        rm -rf "$RUNTIME_DIR/node_modules" "$RUNTIME_DIR/package-lock.json"
    fi
}

# ----------------------------------------------------------------------- npm --

# `find_npm` and `find_prefix` are gone. Both answered the same question — which
# npm, and where will it put things — and both existed because the answer
# depended on which Node was in hand and what the user's `.npmrc` said about it.
#
# There is no global install any more. dsh and pnpm go into `$RUNTIME_DIR` as an
# ordinary *local* install, so the destination is `--prefix` and nothing else:
# not npm's configured prefix, not a fallback for when that turns out to be
# root-owned, and not `npm prefix -g`, which on a machine running nvm answers
# with a directory nvm moves out from under it. `$NPM_CLI` is a constant.

# The `package.json` npm wants at the root of a local install. Without one npm
# warns on every run and walks *up* looking for a project to install into,
# which from the app data directory could find anything at all.
write_runtime_manifest() {
    mkdir -p "$RUNTIME_DIR" || return 1
    [ -f "$RUNTIME_DIR/package.json" ] && return 0
    printf '%s\n' '{"name":"dsh-desktop-runtime","version":"1.0.0","private":true}' \
        > "$RUNTIME_DIR/package.json"
}

# One `npm install -g`, from one registry. npm's http log is read as it goes:
# every tarball that comes back moves the bar, which is the only progress signal
# npm offers that means anything.
#
# `$?` has to travel out of a pipeline, which sh has no `pipefail` for, so the
# installer writes it to a file the caller reads.
npm_install() {
    specs=$1
    registry=$2
    from=$3
    to=$4

    node="$NODE_DIR/bin/node"
    code_file="$APP_DIR/.npm-exit"
    mkdir -p "$APP_DIR"
    rm -f "$code_file"

    # Local rather than `-g`, which is the whole change. `--prefix` on a local
    # install names the directory holding `package.json` and `node_modules`, so
    # the packages land at `$RUNTIME_DIR/node_modules/<name>` — the same layout
    # the Windows side gets, and npm's global prefix is not consulted at all.
    set -- "$NPM_CLI" install --prefix "$RUNTIME_DIR" --no-audit --no-fund --loglevel=http
    if [ -n "$registry" ]; then
        set -- "$@" "--registry=$registry"
    fi
    # Unquoted on purpose: `$specs` is our own space-separated list and each
    # word is one package spec.
    # shellcheck disable=SC2086
    set -- "$@" $specs

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
        PATH="$NODE_DIR/bin:$PATH" "$node" "$@" 2>&1 < /dev/null
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

# Install `$1` — a space-separated list of specs — through the fastest registry
# that works.
#
# dsh and pnpm go in one command rather than two. They used to be installed by
# different callers into different prefixes, which is how they could end up in
# two places or how pnpm could end up somewhere the app then could not find. One
# install into one directory cannot do either.
install_package() {
    specs=$1
    from=$2
    to=$3

    rank_registries "$NODE_DIR/bin/node" "$NPM_CLI" "$from"
    sources=$RANKED

    while IFS= read -r source; do
        label=${source%%|*}
        url=${source#*|}

        step "正在安装 dsh（$label）…" "$from"
        if npm_install "$specs" "$url" "$from" "$to"; then
            say "dsh 安装完成（$label）。"
            return 0
        fi
        say "从 $label 安装失败，换下一个源重试。"
    done <<SOURCES
$sources
SOURCES

    return 1
}

# ------------------------------------------------------------------ launcher --

# dsh's entry point inside the runtime, read off the `bin` field of the package
# npm just installed. Prints nothing and fails when there is none.
#
# npm writes the field either way round — `"bin": "./cli.js"` when the package
# has one binary named after itself, or `"bin": { "dsh": "./cli.js" }` when it
# names them — and dsh has used both spellings across releases, so both are read
# rather than one being assumed. `bin_field` in `src-tauri/src/dsh.rs` parses the
# same two shapes; the two have to agree.
dsh_entry() {
    dir="$RUNTIME_DIR/node_modules/$PACKAGE"
    [ -f "$dir/package.json" ] || return 1

    # The object form first, since it is the more specific match: a `"dsh":`
    # key can only be the named-binary spelling. Falling through to the string
    # form would otherwise match the first `"bin"` value it saw.
    relative=$(sed -n 's/.*"bin"[[:space:]]*:[[:space:]]*{[^}]*"dsh"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' \
        "$dir/package.json" | head -n 1)
    if [ -z "$relative" ]; then
        relative=$(sed -n 's/.*"bin"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' \
            "$dir/package.json" | head -n 1)
    fi
    [ -n "$relative" ] || return 1

    relative=${relative#./}
    [ -f "$dir/$relative" ] || return 1
    printf '%s' "$dir/$relative"
}

# The `dsh` the user's terminal gets: a shell script of our own, naming our Node
# and our dsh by absolute path.
#
# npm generates a shim at `$RUNTIME_DIR/node_modules/.bin/dsh`, and putting that
# on PATH would undo the entire point of this layout — it resolves `node` off
# PATH, which is the coupling we are removing. So the launcher is written here
# and the npm shim is never referenced.
#
# Absolute paths rather than `$0`-relative ones, because `link_launcher` below
# reaches this file through a symlink and `$0` would resolve against the link's
# directory instead of this one.
#
# It also sets PATH, which does not contradict any of the above: naming Node and
# dsh absolutely is what keeps *this* invocation off the user's PATH, and the
# three directories prepended are what keep everything dsh goes on to spawn off
# it too. dsh forwards every plugin install to pnpm and finds it by name —
# `pnpm not found on PATH` is its own error message — so a terminal without that
# line runs the user's pnpm on the user's Node, builds a plugin's native modules
# against that ABI, and hands dsh a `.node` its own Node cannot load. That
# failure lands at plugin-load time, nowhere near the install that caused it.
#
# The launcher's own directory goes first, ahead of the Node's. On Windows that
# Node directory is npm's global prefix, so an `npm i -g` typed into a terminal
# the app opened lands its shim there — and a `dsh` shim ahead of this launcher
# would answer in that terminal and nowhere else. The launcher directory holds
# one file, which is not named `node`, `npm` or `pnpm`, so leading with it costs
# the rest of this nothing. The order matches `child_path` in
# `src-tauri/src/dsh.rs`, so the terminal and the window run the same thing.
write_launcher() {
    entry=$(dsh_entry) || return 1
    mkdir -p "$BIN_DIR" || return 1

    cat > "$BIN_DIR/dsh" <<LAUNCHER
#!/bin/sh
# Generated by dsh-desktop's install-deps.sh. Edits are lost on the next
# install or update. Every path here is ours and none depends on PATH.
PATH="$BIN_DIR:$NODE_DIR/bin:$RUNTIME_DIR/node_modules/.bin:\$PATH"
export PATH
exec "$NODE_DIR/bin/node" "$entry" "\$@"
LAUNCHER

    chmod 755 "$BIN_DIR/dsh" || return 1
    say "已写入 $BIN_DIR/dsh"
}

# ---------------------------------------------------------------------- path --

# One symlink into `~/.local/bin`, and no edits to any file of the user's.
#
# Versions up to 0.1.2 appended a marked block to `~/.profile`, `~/.zshrc` and
# `~/.bashrc`, which was wrong twice over: the directory it added is the Node
# directory, so it shadowed whatever `node`, `npm` and `npx` a version manager
# had put there — and `-Mode uninstall` has no caller on these platforms, so the
# block went in and stayed forever. `clean_profiles` below takes those back out.
#
# A symlink has neither problem. It exposes exactly one command, it is removed
# by deleting one file, and it does not touch a shell profile at all. What it
# cannot do is put the directory on PATH when it is not there already — common
# on macOS — so that case is said out loud rather than fixed behind the user's
# back.
link_launcher() {
    [ -x "$BIN_DIR/dsh" ] || return 1

    if ! mkdir -p "$LINK_DIR" 2>/dev/null; then
        say "无法创建 $LINK_DIR，跳过。想在终端里用 dsh，把 $BIN_DIR 加进 PATH 即可。"
        return 1
    fi

    # `-f` so a link left by an earlier install is replaced rather than
    # refused; the target may have moved between releases.
    if ! ln -sf "$BIN_DIR/dsh" "$LINK_DIR/dsh" 2>/dev/null; then
        say "无法在 $LINK_DIR 创建链接。想在终端里用 dsh，把 $BIN_DIR 加进 PATH 即可。"
        return 1
    fi

    # Asked of the login shell's PATH rather than this process's: a GUI launch
    # inherits an environment that says nothing about what a terminal will have.
    case ":${PATH}:" in
        *":$LINK_DIR:"*)
            say "dsh 已经可以在终端里直接使用。"
            return 0
            ;;
    esac

    say "已把 dsh 链接到 $LINK_DIR/dsh。"
    say "这个目录不在你的 PATH 上（macOS 默认如此）。想在终端里直接用 dsh，把下面这行加进 ~/.zshrc 或 ~/.profile："
    say "  export PATH=\"\$HOME/.local/bin:\$PATH\""
}

# Take the link back out. Only ours: `readlink` has to agree that it points at
# our launcher, so a `dsh` the user put there themselves is left alone.
unlink_launcher() {
    [ -L "$LINK_DIR/dsh" ] || return 0
    target=$(readlink "$LINK_DIR/dsh" 2>/dev/null)
    [ "$target" = "$BIN_DIR/dsh" ] || return 0
    rm -f "$LINK_DIR/dsh"
    say "已移除 $LINK_DIR/dsh。"
}

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

# Take out the block a version up to 0.1.2 left behind. Run on every install
# rather than only on uninstall, because on these platforms uninstall never
# runs: an upgrade is the one moment this is reachable at all.
clean_profiles() {
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

# Clear away what the old scheme left in `$APP_DIR`: a Node under `node`, an npm
# prefix under `npm`, the `bootstrap.json` that recorded where dsh had ended up,
# and any block a version up to 0.1.2 wrote into a shell profile.
#
# Everything named here is inside our own application data directory, so all of
# it is ours by construction and none of it needs the marker's permission to go —
# the marker is deleted unread. The profile block is found by its own `BEGIN_MARK`
# rather than by the prefix it named, so nothing outside `$APP_DIR` has to be
# looked up either.
#
# The old Node is deleted rather than moved into the new tree. It would have
# saved a 30 MB download, but a `-g --prefix` install put dsh *inside* that
# directory too, so moving it wholesale would carry 327 MB of dead weight into
# the runtime and picking Node's own files back out would mean knowing the
# contents of Node's tarball. One download beats that.
#
# Run on every install rather than only the first: on these platforms uninstall
# never runs at all, so an upgrade is the one moment this is reachable.
migrate_legacy() {
    clean_profiles

    if [ ! -e "$LEGACY_NODE_DIR" ] && [ ! -e "$LEGACY_NPM_DIR" ] &&
        [ ! -f "$LEGACY_MARKER" ]
    then
        return 0
    fi

    say '正在清理旧版本留下的运行时…'
    rm -rf "$LEGACY_NODE_DIR" "$LEGACY_NPM_DIR"
    rm -f "$LEGACY_MARKER"
}

install_all() {
    migrate_legacy

    if node_ready && node_current; then
        say "已有可用的 Node $NODE_VERSION（$NODE_DIR），跳过下载。"
        report 35
    else
        install_node
    fi

    write_runtime_manifest || fail "无法写入 $RUNTIME_DIR/package.json。"

    # Both in one command. pnpm used to be installed separately by `plugins.rs`,
    # into a prefix it had to work out for itself; installing it here means it
    # lands beside dsh by construction and the app never has to look for it.
    step '正在下载 dsh，约 185 MB，请耐心等待…' 36
    if ! install_package "$PACKAGE@latest pnpm@latest" 36 "$PROGRESS_CEILING"; then
        fail 'dsh 下载失败。已尝试默认源和 npmmirror、腾讯云、华为云三个镜像，都没有成功，通常是网络或代理的问题。'
    fi

    if ! write_launcher; then
        fail 'dsh 装好了，但在它的 package.json 里找不到入口，无法生成 dsh 命令。'
    fi

    # The one thing that reaches outside `$APP_DIR`. A failure is not fatal: the
    # app runs dsh by absolute path and does not need PATH at all.
    link_launcher || true

    step 'dsh 安装完成。' 100
}

# The same npm command into the same directory as `install`. There is no prefix
# to resolve and no dsh to find first, which is the whole of what this used to
# be about.
update_all() {
    node_ready || fail '运行时还没有装好，无法更新。重启应用会重新安装。'

    write_runtime_manifest || fail "无法写入 $RUNTIME_DIR/package.json。"

    step '正在更新 dsh…' 0
    if ! install_package "$PACKAGE@latest" 0 "$PROGRESS_CEILING"; then
        fail 'dsh 更新失败，默认源和几个备用镜像都没有成功。'
    fi

    # Rewritten because a release is free to move its entry point, and the
    # launcher names it directly rather than going through npm's shim.
    write_launcher || true

    step 'dsh 更新完成。' 100
}

# Delete our runtime and take our link back out.
#
# No npm involved: unpicking a tree package by package would have npm walk 33k
# files to arrive at the same place `rm -rf` reaches in one call. And nothing
# here can name a file outside `$APP_DIR` except the symlink, which is removed
# only after `readlink` confirms it points at our launcher — so a dsh or a Node
# the user installed themselves is not reachable from this function even in
# principle. That is the difference from the version this replaces, where the
# uninstaller would `npm uninstall -g` a dsh it had never installed.
#
# There are no `-RemoveDsh` / `-RemoveNode` switches any more. They existed to
# ask which half of a shared installation to take, and nothing is shared now.
uninstall_all() {
    unlink_launcher
    clean_profiles

    say '正在删除 dsh 运行时…'

    # Checked rather than assumed. `rm -rf` on these platforms has none of the
    # path-length trouble the Windows side had to work around, but it can still
    # fail on a permission or a busy file — and reporting success over a tree
    # that is still there is the one outcome worth ruling out.
    stuck=''
    for dir in "$RUNTIME_DIR" "$BIN_DIR" "$LEGACY_NODE_DIR" "$LEGACY_NPM_DIR"; do
        [ -e "$dir" ] || continue
        rm -rf "$dir"
        [ -e "$dir" ] && stuck="$stuck
  $dir"
    done
    rm -f "$LEGACY_MARKER"

    if [ -n "$stuck" ]; then
        say '以下目录没能删掉，通常是 dsh 还在运行；退出后手动删除即可：'
        printf '%s\n' "$stuck"
        exit 1
    fi

    say '已删除。你自己安装的 Node 和 dsh 没有被改动。'
}

case "$MODE" in
    install) install_all ;;
    update) update_all ;;
    uninstall) uninstall_all ;;
esac

exit 0
