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
# Nothing here needs root. Node goes under the app's own data directory, npm
# writes to a prefix inside it, and the only thing touched outside is a marked
# block appended to the user's shell profile.
#
# A Node the machine already has is used as it is and never replaced; the same
# goes for a dsh already on PATH — installing a second copy beside it would be
# 327 MB nobody asked for. Updating one is a different matter: whoever installed
# it, it is one `npm install -g` in some prefix, so `update` replaces it in place
# in the prefix it is actually in. What this script installed is written down in
# `bootstrap.json`, which is what keeps `uninstall` off a Node it did not put
# there.
#
# The flags are spelled the way `install-deps.ps1` spells them, because
# `src-tauri/src/dsh.rs` calls both with the same arguments:
#
#   sh install-deps.sh -Mode install|update|uninstall [-Prefix <dir>]
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

MODE=install
PREFIX=''
REMOVE_DSH=0
REMOVE_NODE=0
PROGRESS=0

while [ $# -gt 0 ]; do
    case "$1" in
        -Mode)
            [ $# -ge 2 ] || { echo "-Mode 后面要跟 install、update 或 uninstall" >&2; exit 2; }
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
        -RemoveDsh) REMOVE_DSH=1; shift ;;
        -RemoveNode) REMOVE_NODE=1; shift ;;
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
NODE_DIR="$APP_DIR/node"
MARKER="$APP_DIR/bootstrap.json"

# Pinned rather than resolved from `latest-v24.x`, so that what a user gets is a
# visible commit here rather than whatever nodejs.org was serving that day.
NODE_VERSION='24.19.0'

# What an existing Node has to be for us to use it instead of installing our
# own. dsh declares no `engines` field itself, but its direct dependency
# commander@15 does — `>=22.12.0` — so anything under that will not run dsh at
# all. Kept a little above that floor rather than pinned to it exactly.
NODE_MINIMUM='22.19.0'

# nodejs.org first, then the mirrors that carry the same layout — same paths,
# same SHASUMS256.txt — for the networks where the first one does not answer.
NODE_MIRRORS='https://nodejs.org/dist
https://registry.npmmirror.com/-/binary/node
https://mirrors.aliyun.com/nodejs-release
https://mirrors.huaweicloud.com/nodejs'

# An empty URL first: whatever npm resolves on its own, which is the user's own
# `.npmrc` if they have one — a private mirror or a corporate proxy is there for
# a reason and is never overridden. The rest only come out once that has failed.
REGISTRIES='默认源|
npmmirror|https://registry.npmmirror.com/
腾讯云|https://mirrors.cloud.tencent.com/npm/
华为云|https://mirrors.huaweicloud.com/repository/npm/'

# How many packages a dsh install pulls, for the progress bar to divide by. npm
# reports nothing usable to drive one, so what fills the bar is how many
# tarballs have come back against how many are expected. Approximate by
# construction, and held short of the end rather than allowed to claim more than
# it knows.
PACKAGE_COUNT=600
PROGRESS_CEILING=99

# The block appended to the user's shell profile, and how it is found again to
# take it back out.
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
}

write_marker() {
    mkdir -p "$APP_DIR" || return 1

    JSON=''
    json_field node "$M_NODE"
    json_field nodeExe "$M_NODE_EXE"
    json_field npmCli "$M_NPM_CLI"
    json_field prefix "$M_PREFIX"
    json_field dsh "$M_DSH"

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

# ---------------------------------------------------------------------- node --

node_is_new_enough() {
    printed=$("$1" --version 2>/dev/null) || return 1
    [ -n "$printed" ] || return 1

    # `v24.19.0`, and the `-nightly...` suffix a prerelease carries.
    printed=${printed#v}
    printed=${printed%%-*}
    version_ge "$printed" "$NODE_MINIMUM"
}

# The Node this run will use: ours if a previous run installed one, otherwise
# whatever is on PATH — and either way only if it is new enough to be worth it.
# Prints nothing when there is none.
find_node() {
    if [ -x "$NODE_DIR/bin/node" ] && node_is_new_enough "$NODE_DIR/bin/node"; then
        printf '%s' "$NODE_DIR/bin/node"
        return 0
    fi

    found=$(resolve node) || return 1
    if [ -n "$found" ] && node_is_new_enough "$found"; then
        printf '%s' "$found"
        return 0
    fi

    return 1
}

# The Node an update runs npm with, when the marker's pair is gone or was never
# written. Unlike `find_node` this asks no version question: the minimum decides
# whether the machine needs a Node of ours *installed*, and an update installs
# nothing — it replaces a dsh that is already here, with the npm beside whatever
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
$NODE_MIRRORS
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
        "$node" "$@" 2>&1 < /dev/null
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

# Install through the first registry that works.
install_package() {
    node=$1
    cli=$2
    prefix=$3
    from=$4
    to=$5

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
$REGISTRIES
SOURCES

    return 1
}

# ---------------------------------------------------------------------- path --

# The files a login or interactive shell of the user's actually reads.
#
# `~/.profile` always, created if it is not there. `~/.zshrc` as well for a zsh
# user, since that is what macOS runs and a stock account has no `~/.profile` in
# the loop. `~/.bashrc` only if it already exists — writing `~/.bash_profile`
# would stop bash reading `~/.profile` at all, which is somebody else's setup to
# break.
profile_files() {
    printf '%s\n' "$HOME/.profile"
    case "${SHELL:-}" in
        */zsh) printf '%s\n' "$HOME/.zshrc" ;;
    esac
    if [ -f "$HOME/.bashrc" ]; then
        printf '%s\n' "$HOME/.bashrc"
    fi
}

# Put `$1` on the user's PATH, so that the dsh this installed is the one a bare
# `dsh` finds in their terminal — the same copy the app runs, and the same
# updates. Prepended for the same reason `install-deps.ps1` prepends it.
add_path() {
    dir=$1

    profile_files | while IFS= read -r file; do
        if [ -f "$file" ] && grep -qF "$BEGIN_MARK" "$file"; then
            continue
        fi
        # The single quotes keep `$PATH` a literal for the shell that will read
        # this line, rather than baking in the one we happen to have now — which
        # is exactly what SC2016 is there to warn about, and exactly what is
        # wanted here.
        # shellcheck disable=SC2016
        printf '\n%s\nexport PATH="%s:$PATH"\n%s\n' "$BEGIN_MARK" "$dir" "$END_MARK" >> "$file" 2>/dev/null ||
            say "无法写入 $file，终端里的 PATH 需要你自己加。"
    done

    say "已把 $dir 加入 PATH（新开的终端生效）。"
}

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

    found=$(find_dsh) || return 1
    prefix=$(cd "$(dirname "$found")/.." 2>/dev/null && pwd) || return 1
    [ -f "$prefix/$manifest" ] || return 1
    printf '%s' "$prefix"
}

install_all() {
    read_marker

    if node=$(find_node); then
        say "检测到可用的 Node：$node"
        [ -n "$M_NODE" ] || M_NODE=system
    else
        say "没有检测到 Node $NODE_MINIMUM 或更高版本，正在为你安装。"
        install_node
        node="$NODE_DIR/bin/node"
        M_NODE=managed
    fi

    cli=$(find_npm "$node") || fail "这个 Node 旁边没有 npm（$node），无法安装 dsh。"

    M_NODE_EXE=$node
    M_NPM_CLI=$cli
    write_marker

    # A dsh that is already there stays exactly as it is, whoever put it there.
    # Installing a second copy of a 327 MB tree next to a working one is 327 MB
    # nobody asked for.
    if dsh=$(find_dsh); then
        say "检测到系统里已有 dsh（$dsh），跳过安装。"
        [ -n "$M_DSH" ] || M_DSH=system
        write_marker
        return 0
    fi

    prefix=$(find_prefix "$node" "$cli")

    step '正在下载 dsh，约 185 MB，请耐心等待…' 36
    if ! install_package "$node" "$cli" "$prefix" 36 "$PROGRESS_CEILING"; then
        fail 'dsh 下载失败。已尝试默认源和 npmmirror、腾讯云、华为云三个镜像，都没有成功，通常是网络或代理的问题。'
    fi

    M_DSH=managed
    M_PREFIX=$prefix
    write_marker

    add_path "$prefix/bin"
    step 'dsh 安装完成。' 100
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

case "$MODE" in
    install) install_all ;;
    update) update_all ;;
    uninstall) uninstall_all ;;
esac

exit 0
