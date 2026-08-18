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
# own. dsh declares no `engines` field, so this is a judgement call rather than
# something read off the package.
$NodeMinimum = [version] '22.22.3'

# nodejs.org first, then the mirrors that carry the same layout — same paths,
# same SHASUMS256.txt — for the networks where the first one does not answer.
# Each of these was checked against `v$NodeVersion`, sums included; Tsinghua's
# `nodejs-release` was left out because it does not carry that version.
#
# Aliyun answers GET but refuses HEAD, which costs nothing here — `Fetch` only
# ever issues a GET — but is worth knowing before probing it by hand.
$NodeMirrors = @(
    'https://nodejs.org/dist'
    'https://registry.npmmirror.com/-/binary/node'
    'https://mirrors.aliyun.com/nodejs-release'
    'https://mirrors.huaweicloud.com/nodejs'
)

# `$null` first: whatever npm resolves on its own, which is the user's own
# `.npmrc` if they have one — a private mirror or a corporate proxy is there for
# a reason and is never overridden. The rest only come out once that has failed.
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
    ($State | ConvertTo-Json) | Out-File -LiteralPath $Marker -Encoding utf8
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

# Put a Node under `$NodeDir`, from the first mirror that answers with an
# archive whose hash matches what that same mirror published.
function Install-Node {
    $arch = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'arm64' } else { 'x64' }
    $name = "node-v$NodeVersion-win-$arch"
    $scratch = Join-Path ([IO.Path]::GetTempPath()) ('dsh-node-' + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Force -Path $scratch | Out-Null

    try {
        $installed = $false
        foreach ($mirror in $NodeMirrors) {
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

# Where `npm install -g` puts things. For the Node this script installs that is
# the Node directory itself, which is npm's default on Windows; for the
# machine's own Node it is whatever they have configured.
function Get-Prefix([string] $Exe, [string] $Cli) {
    try {
        $printed = & $Exe $Cli prefix -g
        if ($LASTEXITCODE -eq 0 -and $printed) { return ([string] $printed).Trim() }
    } catch {
        # Falls through to the directory npm would have defaulted to anyway.
    }
    return (Split-Path -Parent $Exe)
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
}

# Install `Spec` through the first registry that works.
function Install-Package([string] $Exe, [string] $Cli, [string] $Spec, [string] $Prefix, [double] $From, [double] $To) {
    foreach ($source in $Registries) {
        if (Invoke-NpmInstall $Exe $Cli $Spec $Prefix $source $From $To) {
            Say "dsh 安装完成（$($source.Label)）。"
            return $true
        }
        Say "从 $($source.Label) 安装失败，换下一个源重试。"
    }
    return $false
}

# --------------------------------------------------------------------- path --

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

# Put `Dir` on the user's own PATH — HKCU\Environment, never the machine's.
# Prepended, so that the dsh this installed is the one a bare `dsh` finds.
function Add-Path([string] $Dir) {
    $key = 'HKCU:\Environment'
    $current = (Get-ItemProperty -Path $key -Name Path -ErrorAction SilentlyContinue).Path
    if ($null -eq $current) { $current = '' }

    $entries = @($current.Split(';') | Where-Object { $_ -ne '' })
    if ($entries -contains $Dir) { return }

    # REG_EXPAND_SZ is what Windows' own environment editor writes, and what
    # keeps a `%USERPROFILE%` in somebody else's entry meaning what it says.
    Set-ItemProperty -Path $key -Name Path -Value ((@($Dir) + $entries) -join ';') -Type ExpandString
    Publish-Environment
    Say "已把 $Dir 加入 PATH。"
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
# `$env:Path` is rebuilt from the registry first: a Node installed a moment ago
# is on the user's PATH but not on this process's, which inherited its
# environment before any of that happened.
function Find-Dsh {
    Sync-Path

    $found = Get-Command dsh -CommandType Application -ErrorAction SilentlyContinue |
        Select-Object -First 1
    if ($found) { return $found.Source }
    return $null
}

function Install-All {
    $state = Read-Marker

    $node = Find-Node
    if ($node) {
        Say "检测到可用的 Node：$node"
        if (-not $state.ContainsKey('node')) { $state['node'] = 'system' }
    } else {
        Say "没有检测到 Node $NodeMinimum 或更高版本，正在为你安装。"
        $node = Install-Node
        $state['node'] = 'managed'
    }

    $cli = Find-Npm $node
    if (-not $cli) { Fail "这个 Node 旁边没有 npm（$node），无法安装 dsh。" }

    $state['nodeExe'] = $node
    $state['npmCli'] = $cli
    Write-Marker $state

    # A dsh that is already there stays exactly as it is, whoever put it there.
    # Installing a second copy of a 327 MB tree next to a working one is 327 MB
    # nobody asked for.
    $dsh = Find-Dsh
    if ($dsh) {
        Say "检测到系统里已有 dsh（$dsh），跳过安装。"
        if (-not $state.ContainsKey('dsh')) { $state['dsh'] = 'system' }
        Write-Marker $state
        return
    }

    Step '正在下载 dsh，约 185 MB，请耐心等待…' 36
    # No prefix: npm's own default is what `Get-Prefix` then writes down.
    if (-not (Install-Package $node $cli "$Package@latest" '' 36 $ProgressCeiling)) {
        Fail 'dsh 下载失败。已尝试默认源和 npmmirror、腾讯云、华为云三个镜像，都没有成功，通常是网络或代理的问题。'
    }

    $state['dsh'] = 'managed'
    $state['prefix'] = Get-Prefix $node $cli
    Write-Marker $state

    # The terminal gets the same dsh the app runs, and the same updates. For a
    # Node this script installed it is also what puts `node` and `npm` in reach.
    Add-Path $state['prefix']
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
function Find-DshPrefix([hashtable] $State) {
    $manifest = "node_modules\$Package\package.json"

    $recorded = [string] $State['prefix']
    if ($recorded -and (Test-Path -LiteralPath (Join-Path $recorded $manifest))) {
        return $recorded
    }

    $dsh = Find-Dsh
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
