#Requires -Version 5.1
# Getting Node and dsh onto the machine, for the two callers that need it:
# `src-tauri/installer-hooks.nsh` during install, and `src-tauri/src/dsh.rs`
# when a launch finds nothing to run. One implementation rather than two — the
# NSIS copy would be comparing SHA256 sums and unpacking zips by shelling out to
# certutil and tar, and the Rust copy would then do the whole thing again.
#
# Nothing here needs elevation, which is the point of downloading the standalone
# Node zip rather than running the official MSI. The zip goes under
# %LOCALAPPDATA%, `npm install -g` writes to a per-user prefix, and the only
# thing touched outside our own directory is HKCU\Environment's Path.
#
# A Node the machine already has is used as it is and never replaced; the same
# goes for a dsh already on PATH — installing a second copy beside it would be
# 327 MB nobody asked for. Updating one is a different matter: whoever installed
# it, it is one `npm install -g` in some prefix, so `update` replaces it in place
# in the prefix it is actually in. What this script installed is written down in
# `bootstrap.json`, which is what keeps `uninstall` off a Node it did not put
# there.
#
# Output is plain text for the installer's detail log. Pass `-Progress` and it
# also emits `::status <text>` and `::progress <percent>` lines for the app's
# loading page to parse, and switches stdout to UTF-8 — see `Bootstrap` in
# `src-tauri/src/dsh.rs`. Without it stdout stays in the ANSI code page, which
# is what NSIS decodes `nsExec::ExecToLog` output as.
#
# This file must stay UTF-8 with a BOM. Windows PowerShell 5.1 reads a BOM-less
# file in the ANSI code page, which turns every message below into mojibake.

[CmdletBinding()]
param(
    # `install` gets the machine to a working dsh and is what both callers use.
    # `update` moves an existing one to the newest release. `uninstall` removes
    # what the switches below name, and only what this script installed.
    [ValidateSet('install', 'update', 'uninstall')]
    [string] $Mode = 'install',

    # `update` only: the npm global prefix holding the dsh to replace. The app
    # resolves it from the copy it is actually running — see `prefix_of` in
    # `src-tauri/src/dsh.rs` — because a dsh the user installed themselves sits
    # in their own prefix, not in the one this script would default to. Empty
    # falls back to the marker's prefix, and then to npm's own default.
    [string] $Prefix = '',

    # `uninstall` only. Node cannot go without dsh going too: dsh is a Node
    # program, and leaving it behind would leave a command that cannot run.
    [switch] $RemoveDsh,
    [switch] $RemoveNode,

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
$NodeDir = Join-Path $AppDir 'node'
$Marker = Join-Path $AppDir 'bootstrap.json'

# Pinned rather than resolved from `latest-v24.x`, so that what a user gets is a
# visible commit here rather than whatever nodejs.org was serving that day.
$NodeVersion = '24.19.0'

# What an existing Node has to be for us to use it instead of installing our
# own. dsh declares no `engines` field itself, but its direct dependency
# commander@15 does — `>=22.12.0` — so anything under that will not run dsh at
# all. Kept a little above that floor rather than pinned to it exactly.
#
# Has to match `NODE_MINIMUM` in `install-deps.sh`: this decides whether a
# machine downloads 30 MB of Node it did not need, and the two answering
# differently means the same Node is fine on one platform and not on another.
$NodeMinimum = [version] '22.19.0'

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

# ------------------------------------------------------------------- marker --

# What this script installed, so that `uninstall` can leave alone what it did
# not. Absent until something is actually installed — a machine that already had
# both Node and dsh gets nothing written and nothing removed.
function Read-Marker {
    if (-not (Test-Path -LiteralPath $Marker)) { return @{} }
    try {
        $json = Get-Content -LiteralPath $Marker -Raw -Encoding UTF8
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

function Write-Marker([hashtable] $State) {
    New-Item -ItemType Directory -Force -Path $AppDir | Out-Null
    # Not `Out-File -Encoding utf8`. This runs under Windows PowerShell 5.1 —
    # `powershell.exe`, which is what both the NSIS hook and the app invoke it
    # with — where that spelling means UTF-8 *with* a BOM, and `serde_json` on
    # the reading end refuses to parse one. The app fell back to an empty
    # bootstrap and reported no dsh at all on exactly the machines that have no
    # other way to find it: the ones this script installed a Node onto, which is
    # nothing on any PATH. See `marker` in `dsh.rs`, which strips a BOM off the
    # files this already wrote.
    $json = $State | ConvertTo-Json
    [IO.File]::WriteAllText($Marker, $json, (New-Object Text.UTF8Encoding $false))
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

# The Node this run will use: ours if a previous run installed one, otherwise
# whatever is on PATH — and either way only if it is new enough to be worth it.
function Find-Node {
    $ours = Join-Path $NodeDir 'node.exe'
    if ((Test-Path -LiteralPath $ours) -and (Test-NodeVersion $ours)) { return $ours }

    $found = Get-Command node -CommandType Application -ErrorAction SilentlyContinue |
        Select-Object -First 1
    if ($found -and (Test-NodeVersion $found.Source)) { return $found.Source }

    return $null
}

# The Node an update runs npm with, when the marker's pair is gone or was never
# written. Unlike `Find-Node` this asks no version question: the minimum decides
# whether the machine needs a Node of ours *installed*, and an update installs
# nothing — it replaces a dsh that is already here, with the npm beside whatever
# Node put it there. Refusing a Node a few releases short of the minimum would
# leave exactly that install permanently un-updatable, which is the case this
# whole path exists for.
function Find-AnyNode {
    $ours = Join-Path $NodeDir 'node.exe'
    if (Test-Path -LiteralPath $ours) { return $ours }

    Sync-Path
    $found = Get-Command node -CommandType Application -ErrorAction SilentlyContinue |
        Select-Object -First 1
    if ($found) { return $found.Source }

    return $null
}

function Test-NodeVersion([string] $Exe) {
    try {
        $printed = & $Exe --version
    } catch {
        return $false
    }
    if ($LASTEXITCODE -ne 0 -or -not $printed) { return $false }

    # `v24.19.0`, and the `-pre` suffix a nightly carries, which [version] would
    # choke on.
    $text = ([string] $printed).Trim().TrimStart('v').Split('-')[0]
    try {
        return ([version] $text) -ge $NodeMinimum
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
                if (Test-Path -LiteralPath $NodeDir) {
                    Remove-Item -LiteralPath $NodeDir -Recurse -Force
                }
                New-Item -ItemType Directory -Force -Path $AppDir | Out-Null
                Move-Item -LiteralPath (Join-Path $scratch $name) -Destination $NodeDir

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
        Remove-Item -LiteralPath $scratch -Recurse -Force -ErrorAction SilentlyContinue
    }

    Say "Node 已安装到 $NodeDir"
    return (Join-Path $NodeDir 'node.exe')
}

# ---------------------------------------------------------------------- npm --

# npm's own entry point next to `$Exe`, to be run through Node rather than
# through the `npm.cmd` shim: no console window, and no dependency on how the
# machine happens to resolve `npm`.
function Find-Npm([string] $Exe) {
    $cli = Join-Path (Split-Path -Parent $Exe) 'node_modules\npm\bin\npm-cli.js'
    if (Test-Path -LiteralPath $cli) { return $cli }
    return $null
}

# Where `npm install -g` puts things, for a Node whose prefix is the machine's
# own business — the user's `.npmrc` decides, and asking npm is the only way to
# learn it.
#
# Not used for a Node this script installed: `npm prefix -g` there answers with
# whatever the user configured, not with the Node directory beside it. See
# `Get-ManagedPrefix`.
function Get-Prefix([string] $Exe, [string] $Cli) {
    try {
        $printed = & $Exe $Cli prefix -g
        if ($LASTEXITCODE -eq 0 -and $printed) { return ([string] $printed).Trim() }
    } catch {
        # Falls through to the directory npm would have defaulted to anyway.
    }
    return (Split-Path -Parent $Exe)
}

# Where a `-g` install goes for the Node *this script* unpacked: the Node
# directory itself, which is npm's own default on Windows when nothing overrides
# it — and the point is that something usually does.
#
# `npm prefix -g` cannot answer this. It reports the configured prefix, and a
# machine running nvm almost always has one: nvm-for-windows works by pointing
# `C:\Program Files\nodejs` at the version in use, and a `prefix=` in the user's
# `.npmrc` (or one inherited from an earlier Node) sends every global install
# there. Letting npm default meant dsh being installed *by* the Node we just
# unpacked and *into* nvm's tree — a directory nvm repoints on the next
# `nvm use`, so the recorded prefix stopped resolving and the app reported no
# dsh at all. It also paired dsh's native modules (koffi, node-pty) with one
# Node major while leaving them to be loaded by another.
#
# So this is asserted rather than asked. The Node is ours, it is not on anyone's
# PATH, and nothing else has an opinion about what belongs beside it.
function Get-ManagedPrefix([string] $Exe) {
    return (Split-Path -Parent $Exe)
}

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

# One `npm install -g`, from one registry, into `$Prefix` when one is given and
# npm's own default when it is not. npm's http log is read as it goes: every
# tarball that comes back moves the bar, which is the only progress signal npm
# offers that means anything.
function Invoke-NpmInstall([string] $Exe, [string] $Cli, [string] $Spec, [string] $Prefix, $Source, [double] $From, [double] $To) {
    $arguments = @($Cli, 'install', '-g', '--no-audit', '--no-fund', '--loglevel=http')
    if ($Prefix) { $arguments += "--prefix=$Prefix" }
    if ($Source.Url) { $arguments += "--registry=$($Source.Url)" }
    $arguments += $Spec

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
    $was = $env:Path
    $env:Path = (Split-Path -Parent $Exe) + [IO.Path]::PathSeparator + $env:Path
    try {
        return (Invoke-Native $Exe $arguments {
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

# Install `Spec` through the fastest registry that works.
function Install-Package([string] $Exe, [string] $Cli, [string] $Spec, [string] $Prefix, [double] $From, [double] $To) {
    foreach ($source in (Sort-Registries $Exe $Cli $From)) {
        if (Invoke-NpmInstall $Exe $Cli $Spec $Prefix $source $From $To) {
            Say "dsh 安装完成（$($source.Label)）。"
            return $true
        }
        Say "从 $($source.Label) 安装失败，换下一个源重试。"
    }
    return $false
}

# --------------------------------------------------------------------- path --

# Nothing here puts anything on the user's PATH any more, and that is
# deliberate.
#
# The app does not need it: `dsh.rs` finds Node and dsh through `bootstrap.json`
# and puts them in front of the PATH it hands the child itself — see
# `search_path` and `apply_path` there. The only thing a PATH entry ever bought
# was a bare `dsh` working in the user's own terminal, and the price was the
# Node directory going on the front of it, shadowing whatever `node`, `npm` and
# `npx` a version manager had put there and letting a shim be paired with a Node
# of a different major version than its native modules were built for.
#
# `Remove-Path` stays, for the entry versions up to 0.1.2 added.

# Rebuild `$env:Path` from the registry. Whatever the installer just did — a Node
# unpacked, a prefix prepended — is on the user's PATH and not on this process's,
# which inherited its environment before any of it happened. The same goes for
# the app, which the installer launches and which then runs this script.
function Sync-Path {
    $machine = [Environment]::GetEnvironmentVariable('Path', 'Machine')
    $user = [Environment]::GetEnvironmentVariable('Path', 'User')
    $env:Path = (@($machine, $user, $env:Path) | Where-Object { $_ }) -join ';'
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

function Remove-Path([string] $Dir) {
    $key = 'HKCU:\Environment'
    $current = (Get-ItemProperty -Path $key -Name Path -ErrorAction SilentlyContinue).Path
    if (-not $current) { return }

    $entries = @($current.Split(';') | Where-Object { $_ -ne '' -and $_ -ne $Dir })
    Set-ItemProperty -Path $key -Name Path -Value ($entries -join ';') -Type ExpandString
    Publish-Environment
    Say "已把 $Dir 移出 PATH。"
}

# -------------------------------------------------------------------- modes --

# Whether the machine can already run dsh, and where from.
#
# The prefix a previous run recorded comes first, because nothing puts it on the
# user's PATH any more — without this a dsh this script installed would be
# invisible to the next run, which would then install it again.
#
# Then the user's own PATH, rebuilt from the registry: a dsh they installed
# themselves sits in npm's default prefix, which npm's own installer put there,
# and a Node unpacked a moment ago is on that PATH but not on this process's.
function Find-Dsh([hashtable] $State) {
    $recorded = [string] $State['prefix']
    if ($recorded) {
        $shim = Join-Path $recorded 'dsh.cmd'
        if (Test-Path -LiteralPath $shim) { return $shim }
    }

    Sync-Path

    $found = Get-Command dsh -CommandType Application -ErrorAction SilentlyContinue |
        Select-Object -First 1
    if ($found) { return $found.Source }
    return $null
}

function Install-All {
    $state = Read-Marker

    # The entry a version up to 0.1.2 prepended, taken back off. Only ours: a
    # prefix like %APPDATA%\npm is on that PATH because npm's own installer put
    # it there, and `Add-Path` left it alone for exactly that reason.
    $recorded = [string] $state['prefix']
    if ($recorded -and $recorded.StartsWith($NodeDir, 'OrdinalIgnoreCase')) {
        Remove-Path $recorded
    }

    # dsh first, and a Node only if there turns out to be something to install
    # it with. A Node is not a thing this app wants on the machine for its own
    # sake — it is what `npm install -g` needs — so a machine that already has a
    # dsh, whoever put it there, needs no Node of ours and gets none.
    #
    # The pairing matters as much as the download. dsh's native modules — koffi
    # and node-pty — are built against one Node's ABI and refuse to load on
    # another's, so the dsh that is already here has to be run with the Node it
    # was installed with, not with one this script chose.
    $dsh = Find-Dsh $state
    if ($dsh) {
        Say "检测到系统里已有 dsh（$dsh），跳过安装。"
        if (-not $state.ContainsKey('dsh')) { $state['dsh'] = 'system' }

        # Written down because the app cannot find this on its own: without a
        # prefix in the marker `dsh.rs` looks for a dsh it cannot see, concludes
        # there is none, and runs this script again on every launch.
        if (-not $state['prefix']) {
            $found = Find-DshPrefix $state $dsh
            if ($found) { $state['prefix'] = $found }
        }

        # The Node beside it — that is the one its native modules were built
        # for. `Find-AnyNode` asks no version question, and rightly: the floor
        # decides whether a Node has to be installed, and here none does.
        $node = [string] $state['nodeExe']
        $cli = [string] $state['npmCli']
        if (-not ($node -and $cli -and (Test-Path -LiteralPath $node) -and (Test-Path -LiteralPath $cli))) {
            $node = ''
            $beside = if ($state['prefix']) { Join-Path ([string] $state['prefix']) 'node.exe' } else { '' }
            if ($beside -and (Test-Path -LiteralPath $beside)) {
                $node = $beside
            } else {
                $node = Find-AnyNode
            }
            if ($node) {
                $cli = Find-Npm $node
                if ($cli) {
                    $state['nodeExe'] = $node
                    $state['npmCli'] = $cli
                    if (-not $state.ContainsKey('node')) { $state['node'] = 'system' }
                }
            }
        }

        Write-Marker $state
        return
    }

    $node = Find-Node
    if ($node) {
        Say "检测到可用的 Node：$node"
        if (-not $state.ContainsKey('node')) { $state['node'] = 'system' }
    } else {
        Say "没有检测到 Node $NodeMinimum 或更高版本，正在为你安装。"
        $node = Install-Node
        $state['node'] = 'managed'
    }

    # Ours or the machine's — asked of the Node in hand, not of whether this run
    # installed one. `Find-Node` returns the Node a *previous* run unpacked
    # before it ever looks at PATH, so a repair, or a retry after the 185 MB dsh
    # download failed, arrives here holding a Node of ours with `Install-Node`
    # never called. Asking the run instead sent exactly those installs to npm's
    # default prefix — the nvm tree `Get-ManagedPrefix` exists to stay out of.
    #
    # The path rather than `$state['node']`: both `Find-Node` and `Install-Node`
    # answer with `$NodeDir\node.exe` for ours, and this stays right on a
    # machine whose marker was lost.
    $managed = (Split-Path -Parent $node) -ieq $NodeDir

    $cli = Find-Npm $node
    if (-not $cli) { Fail "这个 Node 旁边没有 npm（$node），无法安装 dsh。" }

    $state['nodeExe'] = $node
    $state['npmCli'] = $cli
    Write-Marker $state

    # A Node of ours gets an explicit prefix beside it; the machine's own Node
    # keeps npm's default, which is the user's configured prefix and the right
    # answer for a Node they manage. See `Get-ManagedPrefix` for why the first
    # case cannot be left to npm.
    $prefix = if ($managed) { Get-ManagedPrefix $node } else { '' }

    Step '正在下载 dsh，约 185 MB，请耐心等待…' 36
    if (-not (Install-Package $node $cli "$Package@latest" $prefix 36 $ProgressCeiling)) {
        Fail 'dsh 下载失败。已尝试默认源和 npmmirror、腾讯云、华为云三个镜像，都没有成功，通常是网络或代理的问题。'
    }

    $state['dsh'] = 'managed'
    # What was installed into, not what npm would report: with `--prefix` above
    # the two differ exactly on the machines this matters for.
    $state['prefix'] = if ($prefix) { $prefix } else { Get-Prefix $node $cli }
    Write-Marker $state

    # Said rather than done: the app runs this dsh through the marker and needs
    # nothing on PATH, and a user who wants it in their own terminal can decide
    # for themselves whether to put it there.
    Say "dsh 已安装到 $($state['prefix'])\dsh.cmd"
    Say "想在终端里直接用 dsh，把 $($state['prefix']) 加进你的 PATH 即可。"
    Step 'dsh 安装完成。' 100
}

function Update-All {
    $state = Read-Marker

    # The pair that installed dsh, if it is still there: any npm can write into
    # the prefix it is handed, but this one is known to work on this machine.
    $node = $state['nodeExe']
    $cli = $state['npmCli']
    if (-not ($node -and $cli -and (Test-Path -LiteralPath $node) -and (Test-Path -LiteralPath $cli))) {
        $node = Find-AnyNode
        if (-not $node) { Fail '这台机器上找不到 Node，无法更新 dsh。' }
        $cli = Find-Npm $node
        if (-not $cli) { Fail "这个 Node 旁边没有 npm（$node）。" }
        Say "用 $node 更新 dsh。"
    }

    # The prefix the dsh being replaced actually lives in — `-Prefix` from the
    # app, or the one this script installed into. Without one, npm's default,
    # which is only the right answer when it is also where dsh already is.
    $prefix = $Prefix
    if (-not $prefix) { $prefix = [string] $state['prefix'] }

    Step '正在更新 dsh…' 0
    if (-not (Install-Package $node $cli "$Package@latest" $prefix 0 $ProgressCeiling)) {
        Fail 'dsh 更新失败，默认源和几个备用镜像都没有成功。'
    }
    Step 'dsh 更新完成。' 100
}

# The npm global prefix a dsh lives in, for `npm uninstall -g --prefix` to
# unpick, or `$null` if there is nothing here to point npm at.
#
# The marker's prefix first — that is where this script installed — and then the
# dsh on PATH, whose prefix is read off the layout `npm install -g` leaves on
# Windows: the shim in the prefix, the package under the `node_modules` beside
# it. Anything else is not an npm global install, and npm cannot remove it.
function Find-DshPrefix([hashtable] $State, [string] $Dsh) {
    $manifest = "node_modules\$Package\package.json"

    $recorded = [string] $State['prefix']
    if ($recorded -and (Test-Path -LiteralPath (Join-Path $recorded $manifest))) {
        return $recorded
    }

    # `$Dsh` when the caller already has one, rather than looking twice.
    $dsh = $Dsh
    if (-not $dsh) { $dsh = Find-Dsh $State }
    if ($dsh) {
        $dir = Split-Path -Parent $dsh
        if (Test-Path -LiteralPath (Join-Path $dir $manifest)) { return $dir }
    }

    return $null
}

function Uninstall-All {
    $state = Read-Marker

    # Only a Node of ours goes: one the machine already had is not this
    # uninstaller's to take, whatever the answer was. dsh has no such
    # reservation — it is one `npm install -g` either way, and the user has just
    # been asked about it by name.
    #
    # Node is also a Node program's only way to run, so taking it away while
    # leaving dsh behind would leave a `dsh` command that cannot start.
    $dropNode = $RemoveNode -and ($state['node'] -eq 'managed')
    $dropDsh = $RemoveDsh -or $dropNode

    if ($RemoveNode -and -not $dropNode) {
        Say 'Node 是你自己装的，不会动它。'
    }

    if ($dropDsh) {
        $prefix = Find-DshPrefix $state

        if (-not $prefix) {
            Say '找不到 dsh 装在哪里（不是 npm 全局安装？），跳过卸载 dsh。'
        } elseif ($dropNode -and $prefix.StartsWith($NodeDir, 'OrdinalIgnoreCase')) {
            # It lives inside the directory about to be deleted, and asking npm
            # to walk 33k files first would only be slower.
            Say 'dsh 装在即将删除的 Node 目录里，会随它一起删掉。'
        } else {
            # npm has to unpick its own tree, and it has to be pointed at the
            # prefix holding it rather than at whatever this run would default to.
            $node = $state['nodeExe']
            $cli = $state['npmCli']
            if ($node -and $cli -and (Test-Path -LiteralPath $node) -and (Test-Path -LiteralPath $cli)) {
                if ($state['dsh'] -ne 'managed') {
                    Say '这份 dsh 不是本应用装的，按你的选择一并卸载。'
                }
                Say "正在卸载 dsh（$prefix）…"
                Invoke-Native $node @($cli, 'uninstall', '-g', "--prefix=$prefix", '--loglevel=error', $Package) $null | Out-Null
            } else {
                Say '找不到可用的 npm，跳过卸载 dsh。'
            }
        }
    }

    if ($dropNode) {
        if ($state['prefix']) { Remove-Path $state['prefix'] }
        Say '正在删除 Node 和 dsh…'
        Remove-Item -LiteralPath $NodeDir -Recurse -Force -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath $Marker -Force -ErrorAction SilentlyContinue
        return
    }

    if ($dropDsh) {
        $state.Remove('dsh')
        Write-Marker $state
    }
}

switch ($Mode) {
    'install' { Install-All }
    'update' { Update-All }
    'uninstall' { Uninstall-All }
}

exit 0
