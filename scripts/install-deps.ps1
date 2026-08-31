#Requires -Version 5.1
# Getting Node and dsh onto the machine, for the two callers that need it:
# `src-tauri/installer-hooks.nsh` during install, and `src-tauri/src/dsh.rs`
# when a launch finds nothing to run. One implementation rather than two — the
# NSIS copy would be comparing SHA256 sums and unpacking zips by shelling out to
# certutil and tar, and the Rust copy would then do the whole thing again.
#
# Nothing here needs elevation, which is the point of downloading the standalone
# Node zip rather than running the official MSI. Everything goes under
# %LOCALAPPDATA%, and the only thing touched outside our own directory is
# HKCU\Environment's Path.
#
# The app owns its runtime outright, and that one decision is what keeps the
# rest of this short:
#
#     <AppDir>/runtime/node/                  a pinned Node, ours alone
#     <AppDir>/runtime/node_modules/          dsh and pnpm, a *local* install
#     <AppDir>/bin/dsh.cmd                    the only thing put on PATH
#
# Nothing is detected and nothing is shared. A Node the machine already has is
# not reused, a dsh the user installed themselves is never touched, and npm's
# global prefix is not involved at all — so no version manager can move any of
# it, and every path above is a constant derived from %LOCALAPPDATA% rather than
# something that has to be discovered and written down. There is no marker file:
# `bootstrap.json` is read once by `Migrate-Legacy` to clean up after the old
# scheme and never written again.
#
# The launcher is written here rather than left to npm because npm's generated
# `.cmd` shim falls back to whatever `node` is on PATH, which is the exact
# coupling this layout exists to remove. `Write-Launcher` hard-codes ours.
#
# `install` and `update` are the same npm command into the same directory, so
# `update` is three lines. `uninstall` deletes two directories and drops one
# PATH entry; it cannot touch anything of the user's, because it does not know
# how to name anything outside `<AppDir>`.
#
# Output is plain text for the installer's detail log. Pass `-Progress` and it
# also emits `::status <text>` and `::progress <percent>` lines for the app's
# loading page to parse, and switches stdout to UTF-8 — see `run` in
# `src-tauri/src/dsh.rs`. Without it stdout stays in the ANSI code page, which
# is what NSIS decodes `nsExec::ExecToLog` output as.
#
# This file must stay UTF-8 with a BOM. Windows PowerShell 5.1 reads a BOM-less
# file in the ANSI code page, which turns every message below into mojibake.

[CmdletBinding()]
param(
    # `install` gets the machine to a working dsh and is what both callers use.
    # `update` moves it to the newest release. `uninstall` removes the runtime
    # this script installed, which is the only thing it can name.
    #
    # There is no `-Prefix` any more: install, update and uninstall all act on
    # `<AppDir>/runtime`, which is a constant. The app used to have to work out
    # which prefix its dsh was in and pass it down; now there is one answer.
    [ValidateSet('install', 'update', 'uninstall')]
    [string] $Mode = 'install',

    # Emit machine-readable progress alongside the human log, in UTF-8.
    [switch] $Progress
)

$ErrorActionPreference = 'Stop'
# Invoke-WebRequest's own progress bar repaints the host on every chunk and can
# cost more than the transfer does. `Fetch` reports progress itself.
$ProgressPreference = 'SilentlyContinue'
# Windows PowerShell 5.1 still defaults to SSL3/TLS1.0 on some builds, which
# nodejs.org and every mirror below refuse.
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

if ($Progress) {
    [Console]::OutputEncoding = [Text.Encoding]::UTF8
}

$Package = '@deepseek-ai/dsh'

# The same directory Tauri resolves as `app_local_data_dir()` and
# `installer-hooks.nsh` builds out of `$LOCALAPPDATA` and `${BUNDLEID}`. All
# three have to agree.
$Identifier = 'ai.deepseek.dsh.desktop'
$AppDir = Join-Path $env:LOCALAPPDATA $Identifier

# Everything below is a constant, and that is the point. `dsh.rs` derives the
# same paths from `app_local_data_dir()` without reading anything — see
# `runtime`, `node_dir`, `node` and `entry` there. The two have to agree.
$RuntimeDir = Join-Path $AppDir 'runtime'
$NodeDir = Join-Path $RuntimeDir 'node'
$BinDir = Join-Path $AppDir 'bin'

# npm's own entry point beside our Node, run through Node rather than through
# the `npm.cmd` shim: no console window, and no dependency on how the machine
# happens to resolve `npm`. A constant now that the Node is always ours.
$NpmCli = Join-Path $NodeDir 'node_modules\npm\bin\npm-cli.js'

# What the old scheme left behind, for `Migrate-Legacy` to clear away: a Node
# under `<AppDir>\node`, an npm prefix under `<AppDir>\npm`, and the marker that
# recorded where dsh had ended up. Nothing else reads any of these.
$LegacyNodeDir = Join-Path $AppDir 'node'
$LegacyNpmDir = Join-Path $AppDir 'npm'
$LegacyMarker = Join-Path $AppDir 'bootstrap.json'

# Pinned rather than resolved from `latest-v24.x`, so that what a user gets is a
# visible commit here rather than whatever nodejs.org was serving that day.
$NodeVersion = '24.19.0'

# There is no minimum-version floor any more, and no version check to go with
# it. The floor existed to decide whether a Node the machine already had was
# good enough to install dsh with; nothing is asked of the machine's Node now,
# because nothing uses it. `$NodeVersion` above is what runs dsh, always.
#
# That also settles a coupling the floor could never have caught: dsh's native
# modules (koffi, node-pty) are built against one Node's ABI and will not load
# on another's. Pinning the Node pins the ABI for the life of the install.

# The mirrors that carry Node's own layout — same paths, same SHASUMS256.txt.
# Which one is used is decided by measuring them (see `Sort-Mirrors`); this order
# is only what a machine where nothing could be measured falls back to. Each was
# checked against `v$NodeVersion`, sums included; Tsinghua's `nodejs-release` was
# left out because it does not carry that version.
#
# Aliyun answers GET but refuses HEAD, which costs nothing here — `Fetch` and
# `Measure-Mirror` only ever issue a GET — but is worth knowing before probing it
# by hand.
$NodeMirrors = @(
    'https://nodejs.org/dist'
    'https://registry.npmmirror.com/-/binary/node'
    'https://mirrors.aliyun.com/nodejs-release'
    'https://mirrors.huaweicloud.com/nodejs'
)

# `$null` is whatever npm resolves on its own, which is the user's own `.npmrc`
# if they have one. Which of these is used is decided by measuring them (see
# `Sort-Registries`), with the one exception that keeps a `.npmrc` pointing
# somewhere of the user's own choosing: a private mirror or a corporate proxy is
# there for a reason, may be the only route out of the network at all, and so is
# never raced against anything.
#
# There is no Aliyun entry because there is no Aliyun registry left to point at:
# `mirrors.aliyun.com/npm/` 404s even for unscoped packages, and
# `npm.aliyun.com` does not resolve. npmmirror below is the Alibaba-run mirror
# that succeeded it.
$Registries = @(
    @{ Label = '默认源'; Url = $null }
    @{ Label = 'npmmirror'; Url = 'https://registry.npmmirror.com/' }
    @{ Label = '腾讯云'; Url = 'https://mirrors.cloud.tencent.com/npm/' }
    @{ Label = '华为云'; Url = 'https://mirrors.huaweicloud.com/repository/npm/' }
)

# What `$null` above resolves to when npm is left to itself and has nothing
# configured. Recognised rather than assumed: it is the difference between a
# default worth racing and a choice worth respecting.
$PublicRegistry = 'https://registry.npmjs.org/'

# How long one probe request may take before that source is written off. Two
# requests per source and four sources, so this is a quarter of the worst case a
# measurement can cost — and a source that cannot serve 5 KB of metadata inside it
# is not the one to start 185 MB with.
$ProbeTimeout = 4000

# How long to keep reading from a Node mirror before deciding how fast it is, and
# how much to read at most. The bytes are thrown away, so the cap is what bounds
# the cost: four mirrors on a fast connection spend a second or two and 8 MB to
# choose between 30 MB downloads that can differ by minutes.
$ProbeSeconds = 1.2
$ProbeBytes = 2MB

# How many packages a dsh install pulls, for the progress bar to divide by. npm
# reports nothing usable to drive one, so what fills the bar is how many
# tarballs have come back against how many are expected. Approximate by
# construction, and held short of the end rather than allowed to claim more than
# it knows.
$PackageCount = 600
$ProgressCeiling = 99

# ---------------------------------------------------------------- reporting --

# Write-Host rather than the output stream throughout: everything here is for a
# human to read, and a function's return value is the one thing it must not
# become.
function Say([string] $Text) {
    Write-Host $Text
}

# A line for the log and, when asked, a line for the loading page. A negative
# percentage puts the progress bar away.
function Step([string] $Text, [double] $Percent = -1) {
    if ($Text) {
        Say $Text
        if ($Progress) { Write-Host "::status $Text" }
    }
    if ($Progress) { Write-Host ('::progress {0:N1}' -f $Percent) }
}

function Report([double] $Percent) {
    if ($Progress) { Write-Host ('::progress {0:N1}' -f $Percent) }
}

function Fail([string] $Text) {
    Say "错误：$Text"
    if ($Progress) { Write-Host "::error $Text" }
    exit 1
}

# Run a native command, mirroring everything it prints into the log, and answer
# whether it succeeded.
#
# `$ErrorActionPreference` is dropped to Continue for the duration: PowerShell
# 5.1 wraps a native command's stderr in ErrorRecords when it is merged into the
# pipeline, and under Stop those become terminating errors for output that is
# only a progress log.
function Invoke-Native([string] $Exe, [string[]] $Arguments, [scriptblock] $OnLine) {
    $was = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        & $Exe @Arguments 2>&1 | ForEach-Object {
            $line = [string] $_
            if ($OnLine) { & $OnLine $line }
            Say $line
        }
        return ($LASTEXITCODE -eq 0)
    } finally {
        $ErrorActionPreference = $was
    }
}

# --------------------------------------------------------------- filesystem --

# Delete a directory tree, including the parts of it Windows' 260-character path
# limit puts out of reach. Answers whether the directory is gone; absent counts
# as gone.
#
# `Remove-Item -Recurse` cannot do this, and fails in the worst possible way.
# npm's tree runs well past MAX_PATH — `runtime\node_modules\…` measured 326
# characters on the machine this was written on — and PowerShell 5.1's provider
# throws `DirectoryNotFoundException` naming the leaf it lost, having deleted
# *nothing*. Every call site here had `-ErrorAction SilentlyContinue` on it, so
# that failure was invisible: `uninstall` printed "已删除" over a 360 MB tree it
# had not touched, and the next install layered a new one on top.
#
# The `\\?\` prefix turns off path parsing in the Win32 layer, and MAX_PATH with
# it. It requires a fully qualified path with no `.` or `..` components, which
# every caller here has — they are all joins onto `$env:LOCALAPPDATA` — and a
# different spelling for UNC, which `%LOCALAPPDATA%` is not on an ordinary
# machine but can be when a profile is redirected to a share.
function Remove-Tree([string] $Path) {
    if (-not (Test-Path -LiteralPath $Path)) { return $true }

    $extended = if ($Path.StartsWith('\\')) {
        '\\?\UNC\' + $Path.Substring(2)
    } else {
        '\\?\' + $Path
    }

    try {
        [IO.Directory]::Delete($extended, $true)
    } catch {
        # Reported rather than swallowed. A tree that will not go is usually a
        # file held open by a dsh that is still running, and the user can act on
        # that if they are told.
        Say "删除 $Path 失败：$($_.Exception.Message)"
    }

    return (-not (Test-Path -LiteralPath $Path))
}

# ------------------------------------------------------------------- marker --

# The old `bootstrap.json`, read for the one thing still worth knowing: whether
# the Node under `<AppDir>\node` was installed by us and so is ours to delete.
# `Migrate-Legacy` is the only caller, and after it runs the file is gone.
#
# Nothing writes a marker any more. It existed because a dsh could be anywhere
# and its location had to be remembered; every path is a constant now, so there
# is nothing to remember — and the whole class of bugs that came with reading it
# back goes with it. The worst of those is worth naming, because it is why this
# reader passes `-Encoding UTF8` and the Rust side used to strip a BOM: PS 5.1's
# `Out-File -Encoding utf8` writes a BOM, and `serde_json` refuses to parse one,
# so the app read an empty marker and reported no dsh at all on exactly the
# machines that had no other way to find it.
function Read-LegacyMarker {
    if (-not (Test-Path -LiteralPath $LegacyMarker)) { return @{} }
    try {
        $json = Get-Content -LiteralPath $LegacyMarker -Raw -Encoding UTF8
        $read = @{}
        # PS 5.1's ConvertFrom-Json has no -AsHashtable.
        foreach ($field in (ConvertFrom-Json $json).PSObject.Properties) {
            $read[$field.Name] = $field.Value
        }
        return $read
    } catch {
        return @{}
    }
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
# are. Node is one 30 MB archive, where bandwidth is the whole story, so a
# mirror is judged by how fast it actually serves that archive. dsh is 600
# requests of which its own tarball is 32 KB, where the answer time of each
# request is the story, so a registry is judged by how quickly it serves a fixed
# small workload rather than by a bandwidth figure that would not predict much.

# One probe request, read to the end and handed back. Nothing is written to disk
# and nothing is cached: the timing is the result, and the body is only read
# because a response nobody reads is not a response that has arrived.
function Read-Probe([string] $Url) {
    $request = [Net.HttpWebRequest]::Create($Url)
    $request.Timeout = $ProbeTimeout
    $request.ReadWriteTimeout = $ProbeTimeout
    $request.UserAgent = 'dsh-desktop-installer'

    $reply = $request.GetResponse()
    try {
        $reader = [IO.StreamReader]::new($reply.GetResponseStream())
        try {
            return $reader.ReadToEnd()
        } finally {
            $reader.Dispose()
        }
    } finally {
        $reply.Dispose()
    }
}

# How long a registry takes over a fixed workload: dsh's own metadata, and then
# the tarball that metadata names. Both are small — the dsh package is 32 KB, the
# 185 MB is its dependencies — so what comes out is how quickly this source
# answers, which for an install that makes 600 requests of exactly this shape is
# the number that matters. Around 40 KB per source, against the several MB a
# bandwidth test would have cost.
#
# Seconds, or `$null` for a source that failed or timed out — either way not one
# to start with.
function Measure-Source([string] $Url) {
    $base = $Url.TrimEnd('/') + '/'
    $clock = [Diagnostics.Stopwatch]::StartNew()
    try {
        $meta = Read-Probe "$base$Package/latest"
        # `dist.tarball` as this source itself published it: every mirror rewrites
        # that field onto its own host or CDN, which is where npm would then go,
        # so following it measures the path the install actually takes.
        #
        # Resolved against the registry, because Huawei's is a path rather than a
        # URL — `/@deepseek-ai/dsh/-/dsh-0.1.0-rc.7.tgz` — which npm reads
        # relative to the registry it asked, and so does this.
        #
        # The leading slash comes off first, or the registry's own path would come
        # off with it: rooted at the host, Huawei's answers 200 with an 11 KB page
        # that is not the tarball, and a probe measuring that is not measuring the
        # same workload as the others. An absolute URL is unaffected and stays
        # exactly as it was published.
        $tarball = (ConvertFrom-Json $meta).dist.tarball
        if (-not $tarball) { return $null }
        Read-Probe ([Uri]::new([Uri] $base, $tarball.TrimStart('/'))).AbsoluteUri | Out-Null
    } catch {
        return $null
    }
    return $clock.Elapsed.TotalSeconds
}

# How fast a Node mirror serves the very archive that is about to be downloaded,
# in bytes per second. The read is abandoned after `$ProbeSeconds` or
# `$ProbeBytes`, whichever comes first, so a fast mirror is measured on a couple
# of megabytes and a slow one costs a second.
#
# Connecting counts against the mirror — the clock starts before the request —
# but the read window does not, so a mirror that answers slowly and then serves
# quickly is marked down rather than written off.
#
# `$null` for a mirror that never answered.
function Measure-Mirror([string] $Url) {
    $clock = [Diagnostics.Stopwatch]::StartNew()
    try {
        $request = [Net.HttpWebRequest]::Create($Url)
        $request.Timeout = $ProbeTimeout
        $request.ReadWriteTimeout = $ProbeTimeout
        $request.UserAgent = 'dsh-desktop-installer'
        $reply = $request.GetResponse()
    } catch {
        return $null
    }

    $answered = $clock.Elapsed.TotalSeconds
    try {
        $stream = $reply.GetResponseStream()
        $buffer = [byte[]]::new(128 * 1024)
        $done = 0L

        while ($done -lt $ProbeBytes -and
            ($clock.Elapsed.TotalSeconds - $answered) -lt $ProbeSeconds) {
            $read = $stream.Read($buffer, 0, $buffer.Length)
            if ($read -le 0) { break }
            $done += $read
        }

        if ($done -le 0) { return $null }
        return $done / $clock.Elapsed.TotalSeconds
    } catch {
        return $null
    } finally {
        # Abandoned mid-body, so the connection is dropped rather than left to
        # read out the remaining 30 MB of an archive nobody wanted.
        $request.Abort()
        $reply.Dispose()
    }
}

# A speed a human reads without counting decimal places: MB/s once there is more
# than one of them, and KB/s below that, where `0.0 MB/s` would be the whole
# answer for every mirror worth ranking against each other.
function Format-Speed([double] $BytesPerSecond) {
    if ($BytesPerSecond -ge 1MB) {
        return ('{0:N1} MB/s' -f ($BytesPerSecond / 1MB))
    }
    return ('{0:N0} KB/s' -f ($BytesPerSecond / 1KB))
}

# The Node mirrors in the order to try them, fastest first, with the ones that
# never answered at the back — still there, because a mirror that refuses a probe
# can be the one that ends up serving the file.
function Sort-Mirrors([string] $Name) {
    Step '正在测试 Node 镜像的速度…' 0

    # `Order` is what keeps mirrors that measured the same — the ones that
    # measured nothing at all, usually — in the order they are declared above.
    # Sort-Object is not a stable sort before PowerShell 6.2, and without a second
    # key it shuffles them.
    $order = 0
    $timed = foreach ($mirror in $NodeMirrors) {
        # Not `$host`, which is PowerShell's own read-only variable for the shell
        # it is running in.
        $where = [Uri]::new($mirror).Host
        $speed = Measure-Mirror "$mirror/v$NodeVersion/$Name.zip"
        if ($null -eq $speed) {
            Say "$where：太慢或连不上"
        } else {
            Say "$where：约 $(Format-Speed $speed)"
        }
        $entry = [pscustomobject]@{ Mirror = $mirror; Speed = $speed; Order = $order }
        $order++
        $entry
    }

    $sorted = @($timed | Sort-Object `
            -Property @{ Expression = { if ($null -eq $_.Speed) { -1 } else { $_.Speed } }; Descending = $true },
        @{ Expression = 'Order'; Descending = $false })
    if ($null -ne $sorted[0].Speed) {
        Say "最快的是 $([Uri]::new($sorted[0].Mirror).Host)，就用它。"
    }
    return @($sorted | ForEach-Object { $_.Mirror })
}

# The registries in the order to try them, fastest first.
#
# Not measured at all when npm is configured to a registry of the user's own:
# that one is used first whatever it would have scored, and racing three mirrors
# it is going to beat anyway would only add its own timeouts to an install on a
# network where they are all blocked.
function Sort-Registries([string] $Exe, [string] $Cli, [double] $At) {
    $configured = Get-Registry $Exe $Cli
    if ($configured -and $configured.TrimEnd('/') -ne $PublicRegistry.TrimEnd('/')) {
        Say "检测到你自己配置的 npm 源（$configured），优先用它，不参与测速。"
        return $Registries
    }

    Step '正在测试各个源的速度…' $At

    $order = 0
    $timed = foreach ($source in $Registries) {
        # The default source is npm's own, and npm's own is the public registry —
        # the branch above is what handles it being anything else.
        $url = if ($source.Url) { $source.Url } else { $PublicRegistry }
        $seconds = Measure-Source $url
        if ($null -eq $seconds) {
            Say "$($source.Label)：太慢或连不上"
        } else {
            Say ('{0}：{1:N1} 秒' -f $source.Label, $seconds)
        }
        $entry = [pscustomobject]@{ Source = $source; Seconds = $seconds; Order = $order }
        $order++
        $entry
    }

    # A source that could not be measured goes to the back rather than out: the
    # measurement decides what to start with, not what is allowed to work. `Order`
    # is the tie-break that keeps those in the order declared above; see
    # `Sort-Mirrors` for why it has to be spelled out.
    $sorted = @($timed | Sort-Object `
            -Property @{ Expression = { if ($null -eq $_.Seconds) { [double]::MaxValue } else { $_.Seconds } } },
        Order)
    if ($null -ne $sorted[0].Seconds) {
        Say "最快的是 $($sorted[0].Source.Label)，就用它。"
    }
    return @($sorted | ForEach-Object { $_.Source })
}

# --------------------------------------------------------------------- node --

# Whether our Node is already unpacked and runnable. The whole of what used to
# be `Find-Node`, `Find-AnyNode` and `Test-NodeVersion`: there is one Node this
# script will ever use, at one path, so the only question left is whether it is
# there.
#
# The version is not re-checked. `$NodeDir` is written by `Install-Node` and by
# nothing else, and a pinned version bump replaces the directory wholesale — see
# `Test-NodeCurrent`, which is what notices that.
function Test-NodeReady {
    return (Test-Path -LiteralPath (Join-Path $NodeDir 'node.exe')) -and
           (Test-Path -LiteralPath $NpmCli)
}

# Whether the Node in `$NodeDir` is the pinned one. A `$NodeVersion` bump in a
# new release of this app has to replace it, and the unpacked tree records what
# it is in the `version` field of npm's own manifest — no process to start.
function Test-NodeCurrent {
    $manifest = Join-Path $NodeDir 'node_modules\npm\package.json'
    if (-not (Test-Path -LiteralPath $manifest)) { return $false }
    # Node's `.zip` unpacks as `node-v<version>-win-<arch>`, and `Install-Node`
    # renames it to `$NodeDir`, so the version is not in the path any more. It
    # is written beside it instead, by `Install-Node`, for exactly this.
    $stamp = Join-Path $NodeDir '.dsh-node-version'
    if (-not (Test-Path -LiteralPath $stamp)) { return $false }
    try {
        return ((Get-Content -LiteralPath $stamp -Raw).Trim() -eq $NodeVersion)
    } catch {
        return $false
    }
}

# Download `Url` to `Path`, reporting how far along it is between `From` and
# `To` on the caller's scale. A stream copy rather than
# `Invoke-WebRequest -OutFile` so that there is something to report at all.
function Fetch([string] $Url, [string] $Path, [double] $From, [double] $To) {
    $request = [Net.HttpWebRequest]::Create($Url)
    $request.Timeout = 30000
    $request.UserAgent = 'dsh-desktop-installer'

    $reply = $request.GetResponse()
    $stream = $reply.GetResponseStream()
    $file = [IO.File]::Create($Path)
    try {
        $total = $reply.ContentLength
        $buffer = [byte[]]::new(128 * 1024)
        $done = 0L
        $reported = [DateTime]::UtcNow

        while (($read = $stream.Read($buffer, 0, $buffer.Length)) -gt 0) {
            $file.Write($buffer, 0, $read)
            $done += $read

            # Once a second. The report goes through a pipe and out to a
            # webview, and a repaint per 128 KB chunk is a repaint every few
            # milliseconds.
            if ($total -gt 0 -and ([DateTime]::UtcNow - $reported).TotalSeconds -ge 1) {
                $reported = [DateTime]::UtcNow
                Report ($From + ($To - $From) * ($done / $total))
            }
        }
    } finally {
        $file.Dispose()
        $stream.Dispose()
        $reply.Dispose()
    }
}

# Put a Node under `$NodeDir`, from the fastest mirror that answers with an
# archive whose hash matches what that same mirror published.
function Install-Node {
    # Whether there is a tree here to replace, read before anything deletes it.
    # A replacement invalidates more than the Node; see below.
    $replacing = Test-Path -LiteralPath $NodeDir

    $arch = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'arm64' } else { 'x64' }
    $name = "node-v$NodeVersion-win-$arch"
    $scratch = Join-Path ([IO.Path]::GetTempPath()) ('dsh-node-' + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Force -Path $scratch | Out-Null

    try {
        $installed = $false
        foreach ($mirror in (Sort-Mirrors $name)) {
            $base = "$mirror/v$NodeVersion"
            $zip = Join-Path $scratch "$name.zip"

            try {
                Step "正在下载 Node $NodeVersion（$([Uri]::new($mirror).Host)）…" 0
                Fetch "$base/$name.zip" $zip 0 30

                Step '正在校验 Node…' 32
                $sums = Join-Path $scratch 'SHASUMS256.txt'
                Fetch "$base/SHASUMS256.txt" $sums 32 33

                $wanted = Get-Content -LiteralPath $sums |
                    Where-Object { $_ -match "\s\*?$([regex]::Escape("$name.zip"))\s*$" } |
                    ForEach-Object { $_.Split(' ')[0] } |
                    Select-Object -First 1
                if (-not $wanted) { throw "这个源没有发布 $name.zip 的校验和" }

                $have = (Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash
                if ($have -ne $wanted.Trim().ToUpperInvariant()) {
                    throw '下载的 Node 校验和不匹配'
                }

                Step '正在解压 Node…' 35
                # System32's bsdtar reads zips and is several times faster than
                # Expand-Archive on an archive of this many small files. Named
                # outright because a bare `tar` finds Git for Windows' GNU tar
                # first on some machines, and that one cannot read a zip.
                $tar = Join-Path $env:SystemRoot 'System32\tar.exe'
                & $tar -xf $zip -C $scratch | Out-Null
                if ($LASTEXITCODE -ne 0) { throw "解压失败（退出码 $LASTEXITCODE）" }

                # Replaced rather than merged: a half-unpacked tree from an
                # earlier attempt would otherwise survive underneath the new one.
                if (-not (Remove-Tree $NodeDir)) {
                    throw "无法删除旧的 Node 目录（$NodeDir）"
                }
                New-Item -ItemType Directory -Force -Path $RuntimeDir | Out-Null
                Move-Item -LiteralPath (Join-Path $scratch $name) -Destination $NodeDir

                # What `Test-NodeCurrent` reads. The unpacked tree carries the
                # version only in the directory name the archive came with, and
                # that name is gone the moment it is renamed to `$NodeDir`.
                [IO.File]::WriteAllText(
                    (Join-Path $NodeDir '.dsh-node-version'),
                    $NodeVersion,
                    (New-Object Text.UTF8Encoding $false))

                $installed = $true
                break
            } catch {
                Say "从 $mirror 获取 Node 失败：$($_.Exception.Message)"
            }
        }

        if (-not $installed) {
            Fail "无法下载 Node $NodeVersion。已尝试 nodejs.org 和几个国内镜像，都没有成功，通常是网络或代理的问题。"
        }
    } finally {
        Remove-Tree $scratch | Out-Null
    }

    Say "Node 已安装到 $NodeDir"

    # The Node these were built against is gone, so they have to go too.
    #
    # Nothing else would notice. `npm install` decides from the lockfile and the
    # tree that the packages it was asked for are already there, and dsh's
    # native modules — koffi, node-pty — are then loaded by ABI at require time,
    # a long way from here and with nothing to connect the failure back to a
    # Node version that changed. Both ship prebuilds covering several ABIs, so
    # this would usually survive; a package that fell back to compiling would
    # not, and "usually" is not a thing to leave in the boot path.
    #
    # Only on a replacement. A first install has nothing here to throw away.
    if ($replacing) {
        Say '自带的 Node 版本变了，正在清掉按上一版装好的依赖…'
        Remove-Tree (Join-Path $RuntimeDir 'node_modules') | Out-Null
        Remove-Item -LiteralPath (Join-Path $RuntimeDir 'package-lock.json') `
            -Force -ErrorAction SilentlyContinue
    }

    return (Join-Path $NodeDir 'node.exe')
}

# ---------------------------------------------------------------------- npm --

# `Find-Npm`, `Get-Prefix` and `Get-ManagedPrefix` are gone. All three answered
# the same question — where will this npm put a global install — and all three
# existed because the answer depended on which Node was in hand and what the
# user's `.npmrc` said about it.
#
# There is no global install any more. dsh and pnpm go into `<AppDir>/runtime`
# as an ordinary *local* install, so the destination is `--prefix` and nothing
# else: not npm's configured prefix, not the directory beside the Node, not
# whatever nvm repointed since. `npm prefix -g` is never asked, which is just as
# well — on a machine running nvm-for-windows it answers with a directory nvm
# moves out from under it, which is how a recorded prefix stopped resolving and
# the app reported no dsh at all.
#
# The local layout is also the same on every platform — `<runtime>/node_modules`
# — where a `-g` install is not. `shim_dir`, `package_root` and `root_of` in
# `dsh.rs` were three functions telling those two layouts apart; none survives.

# What npm resolves `registry` to, which is the user's own `.npmrc` if they have
# one. Empty when npm cannot be asked, which `Sort-Registries` reads as nothing
# configured.
function Get-Registry([string] $Exe, [string] $Cli) {
    try {
        $printed = & $Exe $Cli config get registry
        if ($LASTEXITCODE -eq 0 -and $printed) { return ([string] $printed).Trim() }
    } catch {
        # Same answer as npm printing nothing: race the defaults.
    }
    return ''
}

# One `npm install` into `$RuntimeDir`, from one registry. npm's http log is
# read as it goes: every tarball that comes back moves the bar, which is the
# only progress signal npm offers that means anything.
#
# Local rather than `-g`, which is the whole change. `--prefix` on a local
# install names the directory holding `package.json` and `node_modules`, so the
# packages land at `<runtime>/node_modules/<name>` on every platform and npm's
# global prefix — the user's, configured in their `.npmrc`, moved by their
# version manager — is not consulted and not written to.
function Invoke-NpmInstall([string[]] $Specs, $Source, [double] $From, [double] $To) {
    $arguments = @($NpmCli, 'install', '--no-audit', '--no-fund', '--loglevel=http',
                   "--prefix=$RuntimeDir")
    if ($Source.Url) { $arguments += "--registry=$($Source.Url)" }
    $arguments += $Specs

    Step "正在安装 dsh（$($Source.Label)）…" $From

    # Script scope, and reset here rather than left over: a retry against the
    # next registry starts its count from zero, the same as its bar does.
    $script:fetched = 0

    # npm runs a dependency's `install` script through the shell, and the ones
    # that build something spell it `node ...` — resolved off PATH, not from the
    # Node running npm. Both callers start with a PATH that predates this Node:
    # the installer hook runs before anything was unpacked, and the app inherits
    # whatever it was launched with. Without this every package with a build step
    # dies with `node` not found and npm rolls the whole install back.
    #
    # Ours goes on the *front*, so a build step that spells it `node` gets the
    # Node its output will be loaded by. That is what keeps koffi and node-pty
    # matched to the ABI that will run them.
    $was = $env:Path
    $node = Join-Path $NodeDir 'node.exe'
    $env:Path = $NodeDir + [IO.Path]::PathSeparator + $env:Path
    try {
        return (Invoke-Native $node $arguments {
                param($line)
                # `fetch GET 200` is a package coming off the network, `cache` one
                # npm already had. Counting both keeps the bar moving on a machine
                # that has installed dsh before — an update, or a reinstall.
                if ($line -match 'npm http (fetch GET 200|cache) ') {
                    $script:fetched++
                    Report ([Math]::Min($From + ($To - $From) * ($script:fetched / $PackageCount), $ProgressCeiling))
                }
            })
    } finally {
        $env:Path = $was
    }
}

# Install `$Specs` through the fastest registry that works.
#
# dsh and pnpm go in one command rather than two. They used to be installed by
# different callers into different prefixes — dsh here, pnpm from `plugins.rs`
# with a prefix it had to work out for itself — which is how they could end up
# in two places, or how pnpm could end up somewhere the app then could not find.
# One install into one directory cannot do either.
function Install-Package([string[]] $Specs, [double] $From, [double] $To) {
    foreach ($source in (Sort-Registries (Join-Path $NodeDir 'node.exe') $NpmCli $From)) {
        if (Invoke-NpmInstall $Specs $source $From $To) {
            Say "dsh 安装完成（$($source.Label)）。"
            return $true
        }
        Say "从 $($source.Label) 安装失败，换下一个源重试。"
    }
    return $false
}

# The `package.json` npm wants at the root of a local install. Without one npm
# warns on every run and, worse, walks *up* looking for a project to install
# into — which from `%LOCALAPPDATA%` could find anything at all.
#
# `private` keeps it from ever being publishable by accident, and the absence of
# a `dependencies` block is deliberate: what is installed is decided by the
# command line, so there is no second copy of the package list to drift.
function Write-RuntimeManifest {
    New-Item -ItemType Directory -Force -Path $RuntimeDir | Out-Null
    $manifest = Join-Path $RuntimeDir 'package.json'
    if (Test-Path -LiteralPath $manifest) { return }
    $json = '{"name":"dsh-desktop-runtime","version":"1.0.0","private":true}'
    [IO.File]::WriteAllText($manifest, $json, (New-Object Text.UTF8Encoding $false))
}

# ----------------------------------------------------------------- launcher --

# dsh's entry point inside the runtime, read off the `bin` field of the package
# npm just installed. `$null` if the package is not there or names no `dsh`.
#
# npm writes the field either way round — `"bin": "./cli.js"` when the package
# has one binary named after itself, or `"bin": { "dsh": "./cli.js" }` when it
# names them — and dsh has used both spellings across releases, so both are
# read rather than one being assumed.
function Get-DshEntry {
    # `$Package` is npm's spelling, with a forward slash. Everything below joins
    # it onto Windows paths and hands the result to a batch file, so it is
    # normalised once here rather than at each use.
    $dir = Join-Path $RuntimeDir ('node_modules\' + ($Package -replace '/', '\'))
    $manifest = Join-Path $dir 'package.json'
    if (-not (Test-Path -LiteralPath $manifest)) { return $null }

    try {
        $bin = (Get-Content -LiteralPath $manifest -Raw -Encoding UTF8 | ConvertFrom-Json).bin
    } catch {
        return $null
    }
    if (-not $bin) { return $null }

    $relative = if ($bin -is [string]) { $bin } else { $bin.dsh }
    if (-not $relative) { return $null }

    # `./cli.js` and `cli.js` both, and forward slashes throughout — npm's own
    # spelling, which `Join-Path` on Windows is happy to take but the batch file
    # below is not.
    $relative = ([string] $relative).TrimStart('.', '/', '\') -replace '/', '\'
    $entry = Join-Path $dir $relative
    if (-not (Test-Path -LiteralPath $entry)) { return $null }
    return $entry
}

# The `dsh` the user's terminal gets: a batch file of our own, naming our Node
# and our dsh by absolute path.
#
# npm generates a shim of its own at `<runtime>/node_modules/.bin/dsh.cmd`, and
# putting *that* on PATH would undo the entire point of this layout. Its body is
#
#     @IF EXIST "%~dp0\node.exe" (...) ELSE ( node ... )
#
# and in our tree the `IF` fails, so every invocation would take the `ELSE` and
# resolve `node` off the user's PATH — the coupling to whatever Node a version
# manager last selected, reintroduced at the one point that is exposed to the
# user. So the launcher is written here and the npm shim is never referenced.
#
# `%~dp0..` rather than an absolute path: `<AppDir>` contains the user's name,
# and a batch file that works no matter where the tree is moved costs nothing.
# The Unix counterpart in `install-deps.sh` *does* hard-code absolute paths,
# because the file it writes is reached through a symlink and `$0` would resolve
# against the link's directory rather than the launcher's.
#
# It also sets PATH, which does not contradict any of the above: naming Node and
# dsh absolutely is what keeps *this* invocation off the user's PATH, and the
# three directories prepended below are what keep everything dsh goes on to
# spawn off it too. dsh forwards every plugin install to pnpm and finds it by
# name — `pnpm not found on PATH` is its own error message — so a terminal
# without this line runs the user's pnpm on the user's Node, builds a plugin's
# native modules against that ABI, and hands dsh a `.node` its own Node cannot
# load. That failure lands at plugin-load time, nowhere near the install that
# caused it.
#
# The launcher's own directory goes first, ahead of the Node's. On Windows that
# Node directory is npm's global prefix, so an `npm i -g` typed into a terminal
# the app opened lands its shim there — and a `dsh` shim ahead of this launcher
# would answer in that terminal and nowhere else. The launcher directory holds
# one file, which is not named `node`, `npm` or `pnpm`, so leading with it costs
# the rest of this nothing. The order matches `child_path` in
# `src-tauri/src/dsh.rs`, so the terminal and the window run the same thing.
function Write-Launcher {
    $entry = Get-DshEntry
    if (-not $entry) { return $false }

    New-Item -ItemType Directory -Force -Path $BinDir | Out-Null

    # Relative to `$BinDir`, which is `<AppDir>\bin`, so one level up.
    $nodeDir = 'runtime\node'
    $modulesBin = 'runtime\node_modules\.bin'
    $relative = $entry.Substring($AppDir.Length).TrimStart('\')

    # `setlocal` so the PATH below lasts exactly as long as this invocation. A
    # .cmd run from an interactive cmd.exe edits that session's own environment
    # otherwise, and the entry would still be there long after dsh had exited.
    $body = @"
@echo off
rem Generated by dsh-desktop's install-deps.ps1. Edits are lost on the next
rem install or update. Every path here is ours and none depends on PATH.
setlocal
set "PATH=%~dp0;%~dp0..\$nodeDir;%~dp0..\$modulesBin;%PATH%"
"%~dp0..\$nodeDir\node.exe" "%~dp0..\$relative" %*
"@

    # ASCII: a batch file is read in the console's code page, and every byte
    # written here is ASCII anyway. A BOM would be echoed as stray characters
    # before the first command runs.
    [IO.File]::WriteAllText((Join-Path $BinDir 'dsh.cmd'), $body, [Text.Encoding]::ASCII)
    Say "已写入 $BinDir\dsh.cmd"
    return $true
}

# --------------------------------------------------------------------- path --

# One directory goes on the user's PATH — `<AppDir>\bin`, holding the single
# launcher above — and it goes on the *front*.
#
# It used to go on the end, to leave a dsh the user installed themselves in
# charge of their own terminal. What that actually produced was two dsh
# installations sharing one `$DSH_HOME`: the other one wins every `dsh` typed
# into a shell, it is not the copy the app updates, and it runs on whatever Node
# it was installed with — so a plugin installed from the app, against the pinned
# Node's ABI, fails to load when that other dsh is the one loading it. Being
# polite about PATH order bought a failure that looks like a broken plugin.
#
# Prepending is narrow here in a way it would not have been before. The
# directory holds exactly one file, so the only command it can shadow is a
# `dsh`; the machine's `node`, `npm`, `npx` and `pnpm` are untouched. That was
# the failing of the entry versions up to 0.1.2 added, which put the whole Node
# directory on the front of PATH and broke whatever version manager the user was
# running.
#
# It is also reversible without touching anything that is not ours: `-Mode
# uninstall` takes this one entry back off and whatever answered `dsh` before
# answers again. No file outside `<AppDir>` is read, written or deleted.
#
# What it cannot do is win against a dsh on the *machine* PATH. Windows composes
# a process's PATH as the machine's entries followed by the user's, and only
# HKCU is writable without elevation — so a `dsh` in `C:\Program Files\nodejs`
# or in nvm4w's symlink directory still answers first, and nothing this script
# is allowed to do changes that.
#
# The path never changes, so this is written once and is still correct after any
# number of updates — and after any number of `nvm use`.

# The user's PATH exactly as stored, unexpanded.
#
# Not `Get-ItemProperty`, and not `[Environment]::GetEnvironmentVariable`: both
# expand `REG_EXPAND_SZ`, so a PATH containing `%USERPROFILE%\bin` reads back
# with the variable already substituted. Writing that value back would freeze
# the expansion into the user's environment permanently — the entry would still
# work today and break the moment anything about the profile moved. The registry
# API is the only way to ask for the raw string.
function Read-UserPath {
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $false)
    if (-not $key) { return '' }
    try {
        return [string] $key.GetValue('Path', '', 'DoNotExpandEnvironmentNames')
    } finally {
        $key.Close()
    }
}

# Write it back with the kind it already had.
#
# Not a hard-coded `ExpandString`. Windows creates `Path` as `REG_EXPAND_SZ`,
# but plenty of machines have a `REG_SZ` one — anything that ever wrote it with
# `setx`, an installer, or `Set-ItemProperty` without `-Type` leaves it that
# way, and this developer's own machine is one of them. Rewriting the kind is a
# change to the user's environment that nobody asked for and that appending one
# directory has no business making: on a `REG_SZ` PATH the promotion would
# suddenly start expanding any literal `%FOO%` an entry contained.
#
# A `Path` that does not exist yet is created as `REG_EXPAND_SZ`, which is what
# Windows itself would have made it.
function Write-UserPath([string] $Value) {
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $true)
    if (-not $key) { throw '无法写入 HKCU\Environment' }
    try {
        if (-not $Value) {
            $key.DeleteValue('Path', $false)
            return
        }

        $kind = 'ExpandString'
        try {
            $existing = $key.GetValueKind('Path')
            if ($existing -eq 'String') { $kind = 'String' }
        } catch {
            # No value there yet; the default above is the one to create.
        }
        $key.SetValue('Path', $Value, $kind)
    } finally {
        $key.Close()
        Publish-Environment
    }
}

# Tell everything already running that the environment changed. Without it the
# new PATH reaches nothing until the next sign-in — and the app the installer is
# about to launch is one of the things that would miss it.
function Publish-Environment {
    if (-not ('Dsh.Env' -as [type])) {
        Add-Type -Namespace Dsh -Name Env -MemberDefinition @'
[System.Runtime.InteropServices.DllImport("user32.dll", SetLastError = true, CharSet = System.Runtime.InteropServices.CharSet.Auto)]
public static extern System.IntPtr SendMessageTimeout(System.IntPtr hWnd, uint Msg, System.IntPtr wParam, string lParam, uint fuFlags, uint uTimeout, out System.UIntPtr lpdwResult);
'@ -ErrorAction SilentlyContinue
    }

    try {
        $unused = [UIntPtr]::Zero
        # HWND_BROADCAST, WM_SETTINGCHANGE, SMTO_ABORTIFHUNG, five seconds.
        [Dsh.Env]::SendMessageTimeout([IntPtr] 0xffff, 0x1a, [IntPtr]::Zero, 'Environment', 2, 5000, [ref] $unused) | Out-Null
    } catch {
        # Costs a relogin before the terminal sees the new PATH, nothing more.
    }
}

# Put `$Dir` at the front of the user's PATH, once.
#
# Every other copy of it comes out, so an entry an older version appended is
# moved rather than duplicated — matched trimmed and case-insensitively, because
# Windows paths are not case-sensitive and an entry the user has retyped by hand
# may carry a trailing slash or a stray space. Getting that wrong leaves a
# second copy behind on every update, and PATH only ever grows.
#
# The write is skipped when the result would be the string that is already
# there, so the common case — the entry already first, from the last install —
# neither writes the registry nor broadcasts a change to every window on the
# desktop.
function Add-Path([string] $Dir) {
    # The read is inside the try with the write. A `Read-UserPath` that threw
    # would otherwise take the whole script down before `Fail` could say what
    # went wrong — and PATH is the one thing here that is not worth dying over,
    # because the app runs dsh by absolute path and never consults it.
    try {
        $current = Read-UserPath
        $entries = @($current.Split(';') | Where-Object { $_ -ne '' })

        $others = @($entries | Where-Object { $_.Trim().TrimEnd('\') -ine $Dir.TrimEnd('\') })
        $wanted = (@($Dir) + $others) -join ';'

        if ($wanted -eq $current) {
            Say "$Dir 已经在 PATH 最前面了。"
            return $true
        }

        Write-UserPath $wanted
    } catch {
        Say "无法把 $Dir 写入 PATH：$($_.Exception.Message)"
        Say "dsh 仍然可以在应用里正常使用；想在终端里用，手动把这个目录加进 PATH 即可。"
        return $false
    }
    Say "已把 $Dir 放到 PATH 最前面，新开的终端里 dsh 就是应用自带的这一份了。"
    return $true
}

function Remove-Path([string] $Dir) {
    try {
        $current = Read-UserPath
        if (-not $current) { return }

        $kept = @($current.Split(';') |
            Where-Object { $_ -ne '' -and $_.Trim().TrimEnd('\') -ine $Dir.TrimEnd('\') })
        # Nothing matched, so nothing is written. This runs on every install to
        # clear away the old scheme's entries, and on most machines there are
        # none — rewriting PATH to the value it already has would broadcast a
        # change that did not happen.
        $had = @($current.Split(';') | Where-Object { $_ -ne '' })
        if ($kept.Count -eq $had.Count) { return }

        Write-UserPath ($kept -join ';')
    } catch {
        Say "无法把 $Dir 移出 PATH：$($_.Exception.Message)"
        return
    }
    Say "已把 $Dir 移出 PATH。"
}

# -------------------------------------------------------------------- modes --

# Clear away what the old scheme left behind in `<AppDir>`: a Node under `node`,
# an npm prefix under `npm`, and the `bootstrap.json` that recorded where dsh
# had ended up.
#
# Everything named here is inside our own application data directory, so all of
# it is ours by construction and none of it needs the marker's permission to go.
# The marker is read for one thing the paths cannot tell us — the PATH entry
# versions up to 0.1.2 prepended, which named the prefix rather than `<AppDir>`.
#
# The old Node is deleted rather than moved into the new tree. It would have
# saved a 30 MB download, but a `-g --prefix` install put dsh *inside* that
# directory too, so moving it wholesale would carry 327 MB of dead weight into
# the runtime and picking Node's own files back out would mean knowing the
# contents of Node's zip. One download beats that.
#
# Run on every install rather than only the first: on an upgrade this is the one
# moment it is reachable at all, and it costs nothing when there is nothing to
# do.
function Migrate-Legacy {
    $stale = @($LegacyNodeDir, $LegacyNpmDir) | Where-Object { Test-Path -LiteralPath $_ }
    if (-not $stale -and -not (Test-Path -LiteralPath $LegacyMarker)) { return }

    Say '正在清理旧版本留下的运行时…'

    # The entry 0.1.2 and earlier added. `Remove-Path` is a no-op when it is not
    # there, which on most machines it is not.
    #
    # Only a prefix inside `<AppDir>` — the marker records where dsh was
    # installed, and on a machine that already had a Node of its own that is the
    # *user's* npm prefix, `%APPDATA%\npm`. That directory is on their PATH
    # because npm's own installer put it there, it holds every other global
    # command they have, and taking it off would break all of them.
    $recorded = [string] (Read-LegacyMarker)['prefix']
    if ($recorded -and $recorded.StartsWith("$AppDir\", 'OrdinalIgnoreCase')) {
        Remove-Path $recorded
    }
    Remove-Path $LegacyNodeDir

    foreach ($dir in $stale) {
        # Not fatal. A leftover tree costs disk and nothing else — the new
        # runtime does not read from it — so a machine that cannot delete it
        # still gets a working install, and `Remove-Tree` has already said why.
        Remove-Tree $dir | Out-Null
    }
    Remove-Item -LiteralPath $LegacyMarker -Force -ErrorAction SilentlyContinue
}

function Install-All {
    Migrate-Legacy

    if ((Test-NodeReady) -and (Test-NodeCurrent)) {
        Say "已有可用的 Node $NodeVersion（$NodeDir），跳过下载。"
        Report 35
    } else {
        Install-Node | Out-Null
    }

    Write-RuntimeManifest

    # Both in one command. pnpm used to be installed separately by `plugins.rs`,
    # into a prefix it had to work out for itself; installing it here means it
    # lands beside dsh by construction and the app never has to look for it.
    Step '正在下载 dsh，约 185 MB，请耐心等待…' 36
    if (-not (Install-Package @("$Package@latest", 'pnpm@latest') 36 $ProgressCeiling)) {
        Fail 'dsh 下载失败。已尝试默认源和 npmmirror、腾讯云、华为云三个镜像，都没有成功，通常是网络或代理的问题。'
    }

    if (-not (Write-Launcher)) {
        Fail 'dsh 装好了，但在它的 package.json 里找不到入口，无法生成 dsh 命令。'
    }

    # The one thing that reaches outside `<AppDir>`, and the only reason the
    # user's terminal knows about any of this. A failure here is not fatal: the
    # app runs dsh by absolute path and does not need PATH at all.
    Add-Path $BinDir | Out-Null

    Step 'dsh 安装完成。' 100
}

# The same npm command into the same directory as `install`. There is no prefix
# to resolve and no dsh to find first, which is the whole of what this used to
# be about.
function Update-All {
    if (-not (Test-NodeReady)) {
        Fail '运行时还没有装好，无法更新。重启应用会重新安装。'
    }

    Write-RuntimeManifest

    Step '正在更新 dsh…' 0
    if (-not (Install-Package @("$Package@latest") 0 $ProgressCeiling)) {
        Fail 'dsh 更新失败，默认源和几个备用镜像都没有成功。'
    }

    # Rewritten because a release is free to move its entry point, and the
    # launcher names it directly rather than going through npm's shim.
    Write-Launcher | Out-Null

    Step 'dsh 更新完成。' 100
}

# Delete our runtime and take our entry back off PATH.
#
# No npm involved: unpicking a tree package by package would have npm walk 33k
# files to arrive at the same place `Remove-Item` reaches in one call. And
# nothing here can name a file outside `<AppDir>`, so a dsh or a Node the user
# installed themselves is not reachable from this function even in principle —
# which is the difference from the version this replaces, where the uninstaller
# would `npm uninstall -g` a dsh it had never installed.
function Uninstall-All {
    Remove-Path $BinDir

    Say '正在删除 dsh 运行时…'

    $stuck = @()
    foreach ($dir in @($RuntimeDir, $BinDir, $LegacyNodeDir, $LegacyNpmDir)) {
        if (-not (Remove-Tree $dir)) { $stuck += $dir }
    }
    Remove-Item -LiteralPath $LegacyMarker -Force -ErrorAction SilentlyContinue

    # Said only when it is true. The version this replaces printed it
    # unconditionally, over a tree `Remove-Item` had silently failed to touch.
    if ($stuck.Count -gt 0) {
        Say "以下目录没能删掉，通常是 dsh 还在运行；退出后手动删除即可："
        foreach ($dir in $stuck) { Say "  $dir" }
        exit 1
    }

    Say '已删除。你自己安装的 Node 和 dsh 没有被改动。'
}

switch ($Mode) {
    'install' { Install-All }
    'update' { Update-All }
    'uninstall' { Uninstall-All }
}

exit 0
