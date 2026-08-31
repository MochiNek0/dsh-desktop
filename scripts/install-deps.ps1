#Requires -Version 5.1
# Getting Node and dsh onto the machine, on behalf of `src-tauri/src/dsh.rs`.
# One implementation rather than two — a Rust copy would be comparing SHA256
# sums and unpacking Node zips all over again.
#
# Nothing here runs at install time any more. `src-tauri/installer-hooks.nsh`
# calls this script for `uninstall` and nothing else: what to install, and into
# which of the machine's Nodes, is a question the app asks on the first launch,
# where there is a window to ask it in. See `src-tauri/src/setup.rs`.
#
# Nothing here needs elevation, which is the point of downloading the standalone
# Node zip rather than running the official MSI. The zip goes under
# %LOCALAPPDATA%, `npm install -g` writes to a per-user prefix, and the only
# thing touched outside our own directory is HKCU\Environment's Path.
#
# No mode here picks a Node. `switch` and `install-dsh` are handed the one the
# user chose in the app's chooser, and `install-node` is the case where the user
# asked for a fresh one because nothing on the machine would do. Updating is a
# different matter: whoever installed the dsh, it is one `npm install -g` in some
# prefix, so `update` replaces it in place in the prefix it is actually in. What
# this script installed is written down in `bootstrap.json`, which is what keeps
# `uninstall` off a Node it did not put there.
#
# Output is plain text for the uninstaller's detail log. Pass `-Progress` and it
# also emits `::status <text>` and `::progress <percent>` lines for the app's
# loading page to parse, and switches stdout to UTF-8 — see `Bootstrap` in
# `src-tauri/src/dsh.rs`. Without it stdout stays in the ANSI code page, which
# is what NSIS decodes `nsExec::ExecToLog` output as.
#
# This file must stay UTF-8 with a BOM. Windows PowerShell 5.1 reads a BOM-less
# file in the ANSI code page, which turns every message below into mojibake.

[CmdletBinding()]
param(
    # `update` moves an existing dsh to the newest release. `uninstall` removes
    # what the switches below name, and only what this script installed.
    #
    # The four after them are the interactive setup the app's loading page drives
    # when a launch finds no dsh to run — see `setup.rs`. `list` enumerates every
    # Node on the machine and prints one JSON line of what it found, for the app
    # to turn into a chooser. `switch` adopts a Node that already has dsh:
    # nothing to download, nothing installed, just the marker. `install-dsh` runs
    # `npm install -g` against a Node the user picked. `install-node` is the
    # no-Node-at-all case: download one of ours, then install dsh into it.
    #
    # `uninstall-dsh` and `remove-node` are the same panel's undo, reachable from
    # the runtime menu item rather than only from a launch that cannot start.
    # Both are explicit where `uninstall`'s switches are not: `uninstall-dsh`
    # takes dsh out of the Node named by `-NodeExe`, whichever of the machine's
    # Nodes that is, and `remove-node` deletes the Node this script unpacked and
    # nothing else. `uninstall` works off the marker, so it can only ever act on
    # the one dsh the app happens to have recorded — fine for an uninstaller,
    # useless for a machine with a dsh in three Nodes.
    #
    # None of them writes the user's PATH. See the note above `Remove-Path`.
    #
    # There is no default. There used to be — `install`, a mode that picked a
    # Node itself and installed dsh into it — and a bare run of this script is
    # not a thing that should still be able to change the machine.
    [Parameter(Mandatory)]
    [ValidateSet('update', 'uninstall', 'list', 'switch', 'install-dsh', 'install-node',
        'uninstall-dsh', 'remove-node')]
    [string] $Mode,

    # `update` only: the npm global prefix holding the dsh to replace. The app
    # resolves it from the copy it is actually running — see `prefix_of` in
    # `src-tauri/src/dsh.rs` — because a dsh the user installed themselves sits
    # in their own prefix, not in the one this script would default to. Empty
    # falls back to the marker's prefix, and then to npm's own default.
    [string] $Prefix = '',

    # `switch`, `install-dsh` and `uninstall-dsh` only: the `node.exe` the user
    # chose in the runtime panel. Everything this script needs — npm, the prefix, the dsh that
    # is or will be there — hangs off it.
    [string] $NodeExe = '',

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

# The Node an update runs npm with, when the marker's pair is gone or was never
# written. It asks no version question, unlike everything the chooser drives: the
# minimum decides whether a Node is worth *installing dsh into*, and an update
# installs nothing — it replaces a dsh that is already here, with the npm beside
# whatever Node put it there. Refusing a Node a few releases short of the minimum
# would leave exactly that install permanently un-updatable, which is the case
# this whole path exists for.
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
    $text = Get-NodeVersion $Exe
    if (-not $text) { return $false }
    try {
        return ([version] $text) -ge $NodeMinimum
    } catch {
        return $false
    }
}

# The version a Node reports, as bare `24.19.0` — or `$null` for one that would
# not answer. Split out of [`Test-NodeVersion`] because the setup panel shows the
# number, not only the yes/no of whether it is new enough, and because listing
# every Node on the machine needs the string rather than the verdict.
function Get-NodeVersion([string] $Exe) {
    try {
        $printed = & $Exe --version 2>$null
    } catch {
        return $null
    }
    if ($LASTEXITCODE -ne 0 -or -not $printed) { return $null }

    # `v24.19.0`, and the `-pre` suffix a nightly carries, which [version] would
    # choke on.
    return ([string] $printed).Trim().TrimStart('v').Split('-')[0]
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

# Where a `-g` install for `$Exe` already lives — the reading counterpart of the
# two above, and the one every caller that is not about to install should ask.
#
# A Node this script unpacked answers from `Get-ManagedPrefix`, and every other
# Node from npm. Asking npm for both was a quiet bug in `list` mode: on a machine
# whose `.npmrc` sets a prefix, the app's own Node reported that prefix, found no
# dsh under it, and the panel offered to install a dsh into the one Node that
# already had one.
function Get-NodePrefix([string] $Exe, [string] $Cli) {
    if ($Exe.StartsWith($NodeDir, [StringComparison]::OrdinalIgnoreCase)) {
        return (Get-ManagedPrefix $Exe)
    }
    return (Get-Prefix $Exe $Cli)
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
# `Remove-Path` stays, and now has two things to clean up: the entry versions up
# to 0.1.2 added, and the one the interactive chooser briefly added back before
# this. Nothing adds one any more.

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
    if (-not $Dir) { return }
    $key = 'HKCU:\Environment'
    $current = (Get-ItemProperty -Path $key -Name Path -ErrorAction SilentlyContinue).Path
    if (-not $current) { return }

    $entries = @($current.Split(';') | Where-Object { $_ -ne '' -and $_ -ne $Dir })
    if ($entries.Count -eq ($current.Split(';') | Where-Object { $_ -ne '' }).Count) { return }

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

    # Only what this script installed goes, and that is now true of dsh as well
    # as of Node. It was not: `-RemoveDsh` uninstalled whatever the marker
    # pointed at, so a user who had installed dsh themselves and let this app
    # adopt it was asked one vague question at uninstall time and lost their own
    # global package to it. The prompt could not have told them which case they
    # were in either, because the answer is in the marker and NSIS does not read
    # it.
    #
    # Node was always guarded this way. Node is also a Node program's only way to
    # run, so taking ours away has to take the dsh inside it too — that dsh is
    # ours by definition, it is in our directory.
    $dropNode = $RemoveNode -and ($state['node'] -eq 'managed')
    $dropDsh = ($RemoveDsh -and ($state['dsh'] -eq 'managed')) -or $dropNode

    if ($RemoveNode -and -not $dropNode) {
        Say 'Node 是你自己装的，不会动它。'
    }
    if ($RemoveDsh -and -not $dropDsh) {
        Say 'dsh 是你自己装的，不会动它。要卸载请用：npm uninstall -g @deepseek-ai/dsh'
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
                Say "正在卸载 dsh（$prefix）…"
                Invoke-Native $node @($cli, 'uninstall', '-g', "--prefix=$prefix", '--loglevel=error', $Package) $null | Out-Null
            } else {
                Say '找不到可用的 npm，跳过卸载 dsh。'
            }
        }
    }

    if ($dropNode) {
        if ($state['prefix']) { Remove-Path $state['prefix'] }
        # The Node a `switch` or `install-node` put on PATH. `prefix` is usually
        # the same directory on Windows, but a `switch` records the Node's own
        # directory here and the two are worth taking off independently.
        if ($state['pathEntry']) { Remove-Path $state['pathEntry'] }
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

# -------------------------------------------------------------- interactive --
#
# The four modes below are the chooser the app shows when a launch finds no dsh.
# `list` is the only one that does no installing: it walks every Node the machine
# has and prints what it found as one JSON line, for `setup.rs` to turn into a
# panel. The other three act on the choice the user made there.

# The real path of a Node, following the junction nvm-windows puts its "current"
# behind, so the symlink on PATH and the version directory under NVM_HOME are
# recognised as the same Node rather than listed twice in the chooser.
function Resolve-NodeExe([string] $Exe) {
    try {
        $dir = Split-Path -Parent $Exe
        $dirItem = Get-Item -LiteralPath $dir -Force -ErrorAction Stop
        $target = $dirItem.Target
        if ($target) {
            if ($target -is [array]) { $target = $target[0] }
            return Join-Path ([string]$target) (Split-Path -Leaf $Exe)
        }
    } catch {}
    return $Exe
}

# Every Node on the machine, each tagged with where it was found. The list is
# deliberately wide: a machine that switched version managers leaves the old
# one's Nodes behind, and the one the user wants may be any of them.
function Find-AllNodes {
    $raw = New-Object System.Collections.ArrayList

    [void]$raw.Add(([pscustomobject]@{ Exe = (Join-Path $NodeDir 'node.exe'); Source = 'managed' }))

    Sync-Path
    foreach ($dir in ($env:Path -split ';')) {
        if (-not $dir) { continue }
        [void]$raw.Add(([pscustomobject]@{ Exe = (Join-Path $dir 'node.exe'); Source = 'path' }))
    }

    # nvm-windows keeps every installed version side by side under NVM_HOME.
    $nvmHome = $env:NVM_HOME
    if (-not $nvmHome) {
        try { $nvmHome = (Get-ItemProperty -Path 'HKCU:\Environment' -Name NVM_HOME -ErrorAction SilentlyContinue).NVM_HOME } catch {}
    }
    if ($nvmHome -and (Test-Path -LiteralPath $nvmHome)) {
        Get-ChildItem -LiteralPath $nvmHome -Directory -ErrorAction SilentlyContinue |
            Where-Object { $_.Name -like 'v*' } | ForEach-Object {
                [void]$raw.Add(([pscustomobject]@{ Exe = (Join-Path $_.FullName 'node.exe'); Source = 'nvm' }))
            }
    }

    # fnm stores each version under its data directory.
    $fnmRoot = Join-Path $env:LOCALAPPDATA 'fnm\node-versions'
    if (Test-Path -LiteralPath $fnmRoot) {
        Get-ChildItem -LiteralPath $fnmRoot -Directory -ErrorAction SilentlyContinue | ForEach-Object {
            [void]$raw.Add(([pscustomobject]@{ Exe = (Join-Path $_.FullName 'installation\node.exe'); Source = 'fnm' }))
        }
    }

    # volta keeps Node images under its tools directory.
    $voltaRoot = Join-Path $env:LOCALAPPDATA 'Volta\tools\image\node'
    if (Test-Path -LiteralPath $voltaRoot) {
        Get-ChildItem -LiteralPath $voltaRoot -Directory -ErrorAction SilentlyContinue | ForEach-Object {
            [void]$raw.Add(([pscustomobject]@{ Exe = (Join-Path $_.FullName 'node.exe'); Source = 'volta' }))
        }
    }

    # scoop: nodejs, nodejs-lts, and the odd -beta, each under apps\<name>\current.
    $scoop = $env:SCOOP
    if (-not $scoop) { $scoop = Join-Path $env:USERPROFILE 'scoop' }
    $scoopApps = Join-Path $scoop 'apps'
    if (Test-Path -LiteralPath $scoopApps) {
        Get-ChildItem -LiteralPath $scoopApps -Directory -ErrorAction SilentlyContinue |
            Where-Object { $_.Name -like 'nodejs*' } | ForEach-Object {
                [void]$raw.Add(([pscustomobject]@{ Exe = (Join-Path $_.FullName 'current\node.exe'); Source = 'scoop' }))
            }
    }

    # The official installer's two homes, and a per-user install.
    foreach ($base in @(
            'C:\Program Files\nodejs'
            'C:\Program Files (x86)\nodejs'
            (Join-Path $env:LOCALAPPDATA 'Programs\nodejs')
        )) {
        [void]$raw.Add(([pscustomobject]@{ Exe = (Join-Path $base 'node.exe'); Source = 'installer' }))
    }

    # Deduplicate by the resolved path: nvm's current symlink, the PATH entry,
    # and the version directory itself can all reach the same `node.exe`, and a
    # chooser that offers the same thing twice is one the user has to reason
    # past. The displayed path stays as-found; only the key is resolved.
    $seen = @{}
    $unique = New-Object System.Collections.ArrayList
    foreach ($c in $raw) {
        if (-not $c.Exe) { continue }
        if (-not (Test-Path -LiteralPath $c.Exe)) { continue }
        $real = Resolve-NodeExe $c.Exe
        $key = $real.ToLowerInvariant()
        if ($seen.ContainsKey($key)) { continue }
        $seen[$key] = $true
        [void]$unique.Add(([pscustomobject]@{ Exe = $c.Exe; Source = $c.Source }))
    }

    return $unique
}

# One Node's worth of what the chooser needs to show and to decide: its version,
# whether dsh can run on it, where a `-g` install would land, and whether dsh is
# already there. Printed as one JSON line; see `enumerate` in `setup.rs`.
function List-Nodes {
    [Console]::OutputEncoding = [Text.Encoding]::UTF8
    $nodes = Find-AllNodes
    $out = New-Object System.Collections.ArrayList

    foreach ($n in $nodes) {
        $version = Get-NodeVersion $n.Exe
        if (-not $version) { continue }

        $meets = $false
        try { $meets = ([version]$version) -ge $NodeMinimum } catch {}

        $cli = Find-Npm $n.Exe
        $prefix = $null
        $hasDsh = $false
        $dshVersion = $null
        if ($cli) {
            try { $prefix = Get-NodePrefix $n.Exe $cli } catch { $prefix = $null }
            if ($prefix) {
                $manifest = Join-Path $prefix "node_modules\$Package\package.json"
                if (Test-Path -LiteralPath $manifest) {
                    $hasDsh = $true
                    try {
                        $m = Get-Content -LiteralPath $manifest -Raw -Encoding UTF8 | ConvertFrom-Json
                        $dshVersion = $m.version
                    } catch {}
                }
            }
        }

        [void]$out.Add(([pscustomobject]@{
                path         = $n.Exe
                version      = $version
                meetsMinimum = [bool]$meets
                prefix       = $prefix
                hasDsh       = [bool]$hasDsh
                dshVersion   = $dshVersion
                source       = $n.Source
            }))
    }

    if ($out.Count -eq 0) {
        Write-Output '[]'
        return
    }
    $json = $out | ConvertTo-Json -Compress -Depth 5
    if ($out.Count -eq 1) { $json = '[' + $json + ']' }
    Write-Output $json
}

# Adopt a Node that already has dsh: nothing to download, nothing installed,
# just a marker recording which one so the app can find it.
#
# The marker is a fallback, not an override. If the Node the user picked is the
# one their version manager currently points at, the app finds its dsh on PATH
# and never reads this; the marker is what makes a Node they picked but have not
# switched to reachable at all. See `search_path` in `dsh.rs`.
function Switch-Node {
    if (-not $NodeExe) { Fail 'switch 需要 -NodeExe。' }
    if (-not (Test-Path -LiteralPath $NodeExe)) { Fail "找不到指定的 Node：$NodeExe" }

    # The floor, checked here and not only in the panel that offered the button.
    # A dsh already installed into a Node below it still cannot run: dsh's direct
    # dependency commander@15 declares `>=22.12.0`, and its native modules (koffi,
    # node-pty) are built against one Node ABI. Adopting such a Node would write a
    # marker the app then boots from, and the failure would surface as a dsh that
    # will not start rather than as the version problem it is.
    if (-not (Test-NodeVersion $NodeExe)) {
        Fail "这个 Node 的版本低于 dsh 需要的 $NodeMinimum，无法用它运行 dsh。"
    }

    $cli = Find-Npm $NodeExe
    if (-not $cli) { Fail "这个 Node 旁边没有 npm（$NodeExe）。" }
    $prefix = Get-NodePrefix $NodeExe $cli
    $manifest = Join-Path $prefix "node_modules\$Package\package.json"
    if (-not (Test-Path -LiteralPath $manifest)) {
        Fail "这个 Node 里没有安装 dsh（$prefix），无法直接切换。"
    }

    $state = Read-Marker
    $nodeDir = Split-Path -Parent $NodeExe

    # Nothing goes onto the user's PATH — see the note above `Remove-Path`. What
    # is left is taking off the entry an earlier version of this mode put there,
    # which on Windows never worked anyway: the user PATH is searched after the
    # machine PATH, so the entry sat behind nvm-for-windows' own symlink and the
    # terminal went on resolving `node` exactly as before.
    Remove-Path ([string]$state['pathEntry'])
    $state.Remove('pathEntry')

    $state['nodeExe'] = $NodeExe
    $state['npmCli'] = $cli
    $state['prefix'] = $prefix
    if (-not $state.ContainsKey('dsh')) { $state['dsh'] = 'system' }
    if (-not $state.ContainsKey('node')) { $state['node'] = 'system' }
    Write-Marker $state

    Say "应用会使用 $nodeDir 里的 Node 和 dsh。"
    Say "终端里用的还是你的版本管理器当前指定的那个 Node，本应用不会去改它。"
}

# `npm install -g @deepseek-ai/dsh` into a Node the user picked, and record it in
# the marker. The mirror racing and progress reporting are the existing
# `Install-Package`'s; this is only the framing around it.
#
# Nothing is made "active" beyond that: the user's PATH is not written, so if
# this is the Node their version manager already points at, the app and their
# terminal now share one dsh, and if it is not, the marker is what lets the app
# reach it anyway.
function Install-DshInto {
    if (-not $NodeExe) { Fail 'install-dsh 需要 -NodeExe。' }
    if (-not (Test-Path -LiteralPath $NodeExe)) { Fail "找不到指定的 Node：$NodeExe" }

    # The floor, checked here and not only in the panel that offered the button.
    # A dsh already installed into a Node below it still cannot run: dsh's direct
    # dependency commander@15 declares `>=22.12.0`, and its native modules (koffi,
    # node-pty) are built against one Node ABI. Adopting such a Node would write a
    # marker the app then boots from, and the failure would surface as a dsh that
    # will not start rather than as the version problem it is.
    if (-not (Test-NodeVersion $NodeExe)) {
        Fail "这个 Node 的版本低于 dsh 需要的 $NodeMinimum，无法用它运行 dsh。"
    }

    $cli = Find-Npm $NodeExe
    if (-not $cli) { Fail "这个 Node 旁边没有 npm（$NodeExe），无法安装 dsh。" }
    $prefix = Get-NodePrefix $NodeExe $cli

    $state = Read-Marker
    $state['nodeExe'] = $NodeExe
    $state['npmCli'] = $cli
    if (-not $state.ContainsKey('node')) { $state['node'] = 'system' }
    Write-Marker $state

    Step '正在下载 dsh，约 185 MB，请耐心等待…' 0
    if (-not (Install-Package $NodeExe $cli "$Package@latest" $prefix 0 $ProgressCeiling)) {
        Fail 'dsh 下载失败。已尝试默认源和 npmmirror、腾讯云、华为云三个镜像，都没有成功，通常是网络或代理的问题。'
    }

    Remove-Path ([string]$state['pathEntry'])
    $state.Remove('pathEntry')

    $state['dsh'] = 'managed'
    $state['prefix'] = $prefix
    Write-Marker $state
    Step 'dsh 安装完成。' 100
}

# The no-Node-at-all case: download one of ours, then install dsh into it. This
# is what `install` mode does once it has decided a Node has to be installed;
# split out so the chooser's "install a fresh Node" button is explicit about not
# reusing anything it found.
function Install-NodeAndDsh {
    $state = Read-Marker

    # A previous switch's PATH entry points at a Node the user is replacing in
    # their mind with our own. Nothing adds one any more; this takes off what an
    # earlier version left.
    Remove-Path ([string]$state['pathEntry'])
    $state.Remove('pathEntry')

    Say "没有可用的 Node，正在为你安装 Node $NodeVersion。"
    $node = Install-Node
    $cli = Find-Npm $node
    if (-not $cli) { Fail "这个 Node 旁边没有 npm（$node）。" }

    $state['nodeExe'] = $node
    $state['npmCli'] = $cli
    $state['node'] = 'managed'
    Write-Marker $state

    # `Get-ManagedPrefix`, not `Get-Prefix`: this Node is one we just unpacked,
    # and `npm prefix -g` there answers with whatever the user's `.npmrc` says —
    # on a machine running nvm that is nvm's own tree, so dsh would be installed
    # *by* our Node and *into* theirs. See the note above `Get-ManagedPrefix`.
    $prefix = Get-ManagedPrefix $node
    Step '正在下载 dsh，约 185 MB，请耐心等待…' 36
    if (-not (Install-Package $node $cli "$Package@latest" $prefix 36 $ProgressCeiling)) {
        Fail 'dsh 下载失败。已尝试默认源和 npmmirror、腾讯云、华为云三个镜像，都没有成功，通常是网络或代理的问题。'
    }

    $state['dsh'] = 'managed'
    $state['prefix'] = $prefix
    Write-Marker $state
    Step 'dsh 安装完成。' 100
}

# Take dsh out of one Node the user named, and nothing else.
#
# `uninstall -RemoveDsh` cannot do this. It reads the marker, so the only dsh it
# can reach is the one the app recorded; a machine can have one in every Node it
# has, and the panel lists them all.
function Uninstall-DshFrom {
    if (-not $NodeExe) { Fail 'uninstall-dsh 需要 -NodeExe。' }
    if (-not (Test-Path -LiteralPath $NodeExe)) { Fail "找不到指定的 Node：$NodeExe" }

    $cli = Find-Npm $NodeExe
    if (-not $cli) { Fail "这个 Node 旁边没有 npm（$NodeExe）。" }
    $prefix = Get-NodePrefix $NodeExe $cli

    $manifest = Join-Path $prefix "node_modules\$Package\package.json"
    if (-not (Test-Path -LiteralPath $manifest)) {
        Fail "这个 Node 里没有安装 dsh（$prefix）。"
    }

    Step "正在卸载 dsh（$prefix）…" 0
    Invoke-Native $NodeExe @($cli, 'uninstall', '-g', "--prefix=$prefix", '--loglevel=error', $Package) $null | Out-Null

    # A marker pointing at the dsh just removed describes nothing. Cleared rather
    # than left for `search_path` to fall back onto and find a gap.
    $state = Read-Marker
    if ([string]$state['prefix'] -eq $prefix) {
        $state.Remove('prefix')
        $state.Remove('dsh')
        Write-Marker $state
    }
    Step 'dsh 已卸载。' 100
}

# Delete the Node this script unpacked, and the dsh inside it.
#
# Only ever ours. `$NodeDir` is a directory this script created and nothing else
# writes to; a Node the machine already had is not this app's to remove, from
# this mode or any other.
function Remove-ManagedNode {
    if (-not (Test-Path -LiteralPath $NodeDir)) {
        Fail '本应用没有安装过 Node。'
    }

    Step '正在删除应用安装的 Node 和 dsh…' 0
    Remove-Item -LiteralPath $NodeDir -Recurse -Force -ErrorAction SilentlyContinue

    # The marker is only wiped when it was describing the Node that just went.
    # A user who has since moved onto a Node of their own keeps their answer.
    $state = Read-Marker
    if (([string]$state['nodeExe']).StartsWith($NodeDir, [StringComparison]::OrdinalIgnoreCase)) {
        # The fallback prefix `tool_prefix` writes into is ours too, and it only
        # exists because this Node needed somewhere writable.
        Remove-Item -LiteralPath (Join-Path $AppDir 'npm') -Recurse -Force -ErrorAction SilentlyContinue
        Remove-Path ([string]$state['pathEntry'])
        Remove-Item -LiteralPath $Marker -Force -ErrorAction SilentlyContinue
    }
    Step '已删除。' 100
}

switch ($Mode) {
    'update' { Update-All }
    'uninstall' { Uninstall-All }
    'list' { List-Nodes }
    'switch' { Switch-Node }
    'install-dsh' { Install-DshInto }
    'install-node' { Install-NodeAndDsh }
    'uninstall-dsh' { Uninstall-DshFrom }
    'remove-node' { Remove-ManagedNode }
}

exit 0
