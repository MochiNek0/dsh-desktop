; Taking the dsh runtime back off on the way out, and clearing away what an
; older scheme left behind.
;
; Putting it *on* is not here any more. `resources/install-deps.ps1` does that
; work — fetch the pinned Node under %LOCALAPPDATA%, `npm install` dsh and pnpm
; beside it, write a launcher, put one directory on PATH — and the app is what
; calls it: `provision` in `src-tauri/src/dsh.rs`, at first launch, on all three
; platforms. This hook used to call it as well, which made Windows the only
; platform where the script ever ran a second time; see NSIS_HOOK_POSTINSTALL
; for what that cost.
;
; The script still ships in $INSTDIR, because the uninstaller is the one caller
; NSIS keeps: NSIS_HOOK_PREUNINSTALL takes a copy along and
; NSIS_HOOK_POSTUNINSTALL offers to run `-Mode uninstall` with it. Doing that
; deletion here instead would mean a second implementation of it in NSIS — the
; `\\?\` long-path handling npm's 30,000-file tree needs included — kept in step
; with the first one by hand.
;
; Nothing here needs elevation. The script writes under %LOCALAPPDATA% and to
; HKCU\Environment, both of which the current user owns.
;
; Wired up by `tauri.conf.json` under `bundle.windows.nsis.installerHooks`.
;
; This file must stay UTF-8 with a BOM, for two separate reasons. The generated
; installer is built with `Unicode true`, and without the BOM NSIS reads the
; Chinese messages below as ANSI. And UTF-16 — which makensis reads perfectly
; well, so a build says nothing about it — is a file with NUL bytes in it, which
; git treats as binary: the whole file stops appearing in diffs and nothing about
; it can be reviewed. It spent a while as UTF-16LE after an editor rewrote it,
; and the change that fixed a registry key nobody could see was reviewed as
; "Bin 13836 -> 44996 bytes". `npm run check:installer` now fails on either
; mistake, on every platform, whether or not it can find a compiler.

!include LogicLib.nsh
!include WinMessages.nsh
!include StrFunc.nsh
!include FileFunc.nsh

${Using:StrFunc} StrRep
${Using:StrFunc} UnStrRep

; ---------------------------------------------------------------------------
; What the installer is set in
; ---------------------------------------------------------------------------
;
; NSIS draws its pages in whatever font the language file names, and the stock
; English one names none: `English.nlf` has `-` for both the face and the size,
; which leaves the compiler's own default of MS Shell Dlg at 8pt. On Windows 11
; that resolves to Tahoma, and it is the loudest single reason a stock NSIS
; installer looks like it was built in 2003 -- every page, every button and the
; details listbox are set in a font no other window on the machine still uses.
;
; `SetFont` replaces it for the whole installer. It can be set here rather than
; fought over with the language file precisely because that file declares no
; font of its own -- one that did would still win, and none of the languages
; this installer is built with do.
;
; Segoe UI carries no CJK glyphs and the `DetailPrint` messages below are
; Chinese. Windows' font linking covers that, substituting Microsoft YaHei UI
; for those runs exactly as it does in every other dialog on the system.
;
; The other half of the installer's chrome is `BrandingText`, the line along
; the bottom. It cannot be set from here: the template writes its own
; `BrandingText "${COPYRIGHT}"` after this file is included, and the last one
; wins. Left empty, NSIS falls back to the language file's `^Branding`, which
; is "Nullsoft Install System %s" -- so the string comes from `bundle.copyright`
; in `tauri.conf.json` instead.
SetFont "Segoe UI" 9

; ---------------------------------------------------------------------------
; Where the app goes by default
; ---------------------------------------------------------------------------
;
; The template installs per user, so its default is `%LOCALAPPDATA%\dsh-desktop`
; — always on the system drive. That is the wrong default for this app: dsh and
; its Node come to a few hundred megabytes, a session's history grows without
; anybody pruning it, and the machines this runs on are routinely a small fast C:
; beside a large D:. So if there is a roomier fixed disk that is not the system
; drive, that is where the installer points first.
;
; Only the *default* moves. The directory page still shows, still lets the user
; type anywhere they like, and an existing installation still wins outright —
; see the guards in `DshPreferRoomierDrive`.
;
; ## Why `.onGUIInit`
;
; `$INSTDIR` is settled in the template's own `.onInit`, which is where this
; would naturally go — but NSIS allows exactly one `.onInit` per script and the
; template has it. `.onGUIInit` is the next callback to run, after `.onInit` and
; before the first page is drawn, and the template does not define it. So it is
; free, and it is early enough: nothing has been shown to the user yet.
;
; This is also why the check for "did the template already choose?" below is
; written the way it is. By the time this runs, `$INSTDIR` is never the raw
; placeholder any more.

; The subdirectory an install goes in, under whichever root wins. Matches the
; template's own layout, which appends the product name to `$LOCALAPPDATA`.
!define DSH_DIRNAME "dsh-desktop"

; How much room a drive has to have before it is worth suggesting, in megabytes.
; dsh alone is ~185 MB installed and Node another ~35 MB; the rest is headroom
; for a history that grows and an update that stages a second copy of the app
; beside the first.
!define DSH_NEEDED_MB 1500

Var DshSystemDrive
Var DshBestDrive
Var DshBestFreeMB

; Where the template records an existing installation, written out here rather
; than read from `${MANUPRODUCTKEY}`.
;
; That define is the obvious thing to use, and it does not work in a way that
; still compiles. The template includes this file at the very top, *before* it
; defines MANUFACTURER, PRODUCTNAME or MANUPRODUCTKEY -- the `!include` is the
; last line above its own first `!define`. `DshHasPreviousInstall` is expanded
; into `DshPreferRoomierDrive` just below, which is parsed there and then, so
; the define is still unknown; NSIS leaves the text as it stands, warns once
; per line ("unknown variable/constant"), and builds an installer that reads a
; registry key literally named ${MANUPRODUCTKEY}. Nothing has ever written one,
; so the guard below found nothing on every machine it has ever run on, and a
; reinstall was free to be moved to another drive -- which is the one thing it
; exists to prevent.
;
; So it is written out. It has to match `MANUPRODUCTKEY`, which the template
; builds as Software\<publisher>\<productName> from `tauri.conf.json`;
; `DshCheckKey` below fails the build if the two ever drift. The product name
; is already duplicated here as `DSH_DIRNAME`, for the same reason.
!define DSH_MANUPRODUCTKEY "Software\mochineko\dsh-desktop"

; Whether the user already has this app somewhere. `RestorePreviousInstallLocation`
; in the template reads the same key and has already applied it; this only asks
; whether it found anything, because moving a reinstall to a different drive
; would leave the old copy behind and orphan its uninstaller entry.
!macro DshHasPreviousInstall outvar
  ReadRegStr ${outvar} SHCTX "${DSH_MANUPRODUCTKEY}" ""
  ${If} ${outvar} == ""
    ; A machine-wide install from a `perMachine` build, or the other bitness.
    ReadRegStr ${outvar} HKLM "${DSH_MANUPRODUCTKEY}" ""
  ${EndIf}
!macroend

; The check that keeps the copy above honest.
;
; Inserted from `NSIS_HOOK_POSTINSTALL`, which the template expands from inside
; its install section -- hundreds of lines after it has defined
; `MANUPRODUCTKEY`. So by then the real value exists and the two can be
; compared, which is the whole reason the check lives there rather than beside
; the define it is checking.
;
; It emits no code. `!error` fails the compile, which is what a mismatch
; deserves: the alternative is an installer that has quietly stopped
; recognising its own previous installs, and nothing about that shows until a
; user ends up with two copies on two drives.
!macro DshCheckKey
  !if "${DSH_MANUPRODUCTKEY}" != "${MANUPRODUCTKEY}"
    !error "installer-hooks.nsh has DSH_MANUPRODUCTKEY=${DSH_MANUPRODUCTKEY} but the installer uses ${MANUPRODUCTKEY}. Make the copy match bundle.publisher and productName in tauri.conf.json."
  !endif
!macroend

; Point `$INSTDIR` at the roomiest non-system fixed drive, if there is one.
Function DshPreferRoomierDrive
  Push $0

  ; An install that already exists stays exactly where it is. The template put
  ; it in `$INSTDIR` a moment ago and second-guessing that would strand the old
  ; copy on the old drive.
  !insertmacro DshHasPreviousInstall $0
  ${If} $0 != ""
    Goto done
  ${EndIf}

  ; `/D=…` on the command line is the user telling us the answer already.
  ClearErrors
  ${GetOptions} $CMDLINE "/D=" $0
  ${IfNot} ${Errors}
    Goto done
  ${EndIf}

  StrCpy $DshBestDrive ""
  StrCpy $DshBestFreeMB 0
  ; `$WINDIR` is "C:\Windows"; the first two characters are the drive.
  StrCpy $DshSystemDrive $WINDIR 2

  ; Fixed disks only. `GetDrives` filters to those itself, so the callback never
  ; sees a CD-ROM, a network share or a RAM disk.
  ${GetDrives} "HDD" DshEachDrive

  ${If} $DshBestDrive != ""
    StrCpy $INSTDIR "$DshBestDrive${DSH_DIRNAME}"
  ${EndIf}

  done:
  Pop $0
FunctionEnd

; The callback `${GetDrives}` invokes once per fixed drive.
;
; Its contract, from FileFunc's own example: `$9` is the drive root ("D:\") and
; `$8` is its type. It must push a value — "StopGetDrives" to end the walk, and
; anything else to carry on.
Function DshEachDrive
  Push $7

  ${If} "$9" != "$DshSystemDrive\"
    ; Megabytes free. A drive that will not answer — BitLocker still locked, a
    ; card reader with no card — yields an empty string, and `> ${DSH_NEEDED_MB}`
    ; is false for it, so it is skipped rather than chosen with 0 bytes free.
    ${DriveSpace} "$9" "/D=F /S=M" $7
    ${If} $7 > ${DSH_NEEDED_MB}
    ${AndIf} $7 > $DshBestFreeMB
      StrCpy $DshBestFreeMB $7
      StrCpy $DshBestDrive "$9"
    ${EndIf}
  ${EndIf}

  Pop $7
  ; Keep walking.
  Push ""
FunctionEnd

; MUI owns `.onGUIInit` and calls this from inside it, which is why the hook is
; a define rather than a function of that name: declaring `.onGUIInit` here is a
; duplicate-function error at compile time.
;
; It runs after `.onInit` has settled `$INSTDIR` and before the first page is
; drawn, which is the window this needs.
!define MUI_CUSTOMFUNCTION_GUIINIT DshPreferRoomierDrive

; The bootstrap script, and where the uninstaller finds it once `$INSTDIR` is
; gone. See `NSIS_HOOK_PREUNINSTALL`.
!define DSH_SCRIPT "resources\install-deps.ps1"

; `-NonInteractive` because there is nobody at a console to answer: everything
; the script needs to ask is asked through NSIS, below. `-ExecutionPolicy
; Bypass` applies to this one invocation and changes no machine policy.
!define DSH_POWERSHELL "powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File"

; Tell everything already running that the environment changed. The script
; broadcasts this itself after touching PATH; this copy is for the entries the
; migration below removes.
!macro DshPathBroadcast
  SendMessage ${HWND_BROADCAST} ${WM_SETTINGCHANGE} 0 "STR:Environment" /TIMEOUT=5000
!macroend

; Take `dir` back off the user's own PATH. `un` is `Un` in the uninstaller,
; where StrFunc's copies of its functions answer to their own names.
;
; $0 the PATH   $1 the open key, then the kind to write back
; $2 the kind as read, then padded for the search   $3 its size, then the result
!macro DshPathDrop un dir
  ; What kind the value is and how many bytes it holds, asked of the registry
  ; directly. NSIS reports neither: `ReadRegStr` hands back a string and
  ; nothing else.
  ;
  ; The kind matters because the value has to go back as whatever it already
  ; was. `Write-UserPath` in `install-deps.ps1` preserves it too, and the two
  ; have to agree: both edit this one value, and an uninstall reaches it
  ; through either of them — the script when the user keeps the runtime, this
  ; macro when the script never ran. Writing the wrong kind is not fatal, but
  ; it decides whether a `%VAR%` in somebody else's PATH entry still expands,
  ; and it would leave the round trip byte-for-byte on only one of the two.
  ;
  ; The size matters because it is the only thing that tells the two failures
  ; below apart. A key that will not open, or no `Path` value at all, leaves
  ; both at 0.
  StrCpy $1 0
  StrCpy $2 0
  StrCpy $3 0
  System::Call 'ADVAPI32::RegOpenKeyExW(i 0x80000001, w "Environment", i 0, i 0x20019, *i .r1)'
  System::Call 'ADVAPI32::RegQueryValueExW(i r1, w "Path", i 0, *i .r2, i 0, *i .r3)'
  System::Call 'ADVAPI32::RegCloseKey(i r1)'

  ; NSIS strings stop at NSIS_MAX_STRLEN — 1024 in the compiler Tauri bundles —
  ; and a `Path` that long does not come back cut short. It does not come back
  ; at all: `ReadRegStr` answers with an empty string and sets the error flag.
  ; Measured on that build, 1023 characters read back whole and 1024 read back
  ; as "".
  ;
  ; So there is no partial read to defend against. This used to skip any PATH
  ; over 1000 characters, which guarded a truncation that never happens while
  ; silently leaving the entry behind on a PATH of 1000 to 1023 characters —
  ; one that rewrites perfectly well.
  ClearErrors
  ReadRegStr $0 HKCU "Environment" "Path"
  ${If} ${Errors}
    ; Too long to read, so too long to edit. Say so: leaving a stale `dsh` on
    ; someone's PATH is bad, and leaving it there without a word is worse.
    ; `$3` keeps this off the screen of a user who simply has no `Path` of
    ; their own, which is not worth a line.
    ;
    ; Whether the entry is actually there cannot be confirmed from here — the
    ; value is precisely what could not be read — so this asks rather than
    ; asserts. An uninstall inserts this macro three times, and a machine in
    ; this state sees one line per directory.
    ${If} $3 > 0
      DetailPrint "PATH 超过 1023 个字符，安装程序无法安全地修改它。请手动检查「用户环境变量 Path」，如果有这一项就删掉：${dir}"
    ${EndIf}
  ${Else}
    ; Keep the kind, because `$2` is needed for the search.
    StrCpy $1 $2
    ; Padded at both ends so the first and last entries match like any other.
    StrCpy $2 ";$0;"
    ${${un}StrRep} $3 $2 ";${dir};" ";"
    ${If} $3 != $2
      ; Drop the semicolons the search needed back off both ends.
      StrCpy $3 $3 "" 1
      StrCpy $3 $3 -1
      ${If} $3 == ""
        DeleteRegValue HKCU "Environment" "Path"
      ${ElseIf} $1 = 1
        ; REG_SZ.
        WriteRegStr HKCU "Environment" "Path" $3
      ${Else}
        ; REG_EXPAND_SZ, and the fallback for a probe that answered nothing: it
        ; is what Windows itself creates a user `Path` as, and the safer of the
        ; two guesses, since it expands a `%VAR%` that a REG_SZ would leave
        ; broken.
        WriteRegExpandStr HKCU "Environment" "Path" $3
      ${EndIf}
      !insertmacro DshPathBroadcast
      DetailPrint "已移除旧版本留下的 PATH 条目：${dir}"
    ${EndIf}
  ${EndIf}
!macroend

; What an install before this one left behind.
;
; Until now the app shipped its own Node, installed dsh as a private tree under
; `%LOCALAPPDATA%\${BUNDLEID}\dsh`, and put a `dsh` command of its own in
; `$INSTDIR\bin` on PATH. None of that exists any more, and the shim is worse
; than merely dead: it is still on PATH under the same name as the real `dsh`
; this now installs, pointing at a tree that is about to be deleted.
;
; $R0 the old dsh tree   $R5 the old shim directory
!macro DshMigrate
  ${If} ${FileExists} "$R5\dsh.cmd"
  ${OrIf} ${FileExists} "$R5\dsh"
    DetailPrint "正在清理旧版本的 dsh 命令…"
    Delete "$R5\dsh.cmd"
    Delete "$R5\dsh"
    RMDir "$R5"
    !insertmacro DshPathDrop "" "$R5"
  ${EndIf}

  ${If} ${FileExists} "$R0\node_modules\@deepseek-ai\dsh\lib\bin.js"
    DetailPrint "正在删除旧版本自带的 dsh（约 327 MB）…"
    RMDir /r "$R0"
  ${EndIf}

  ; The old update check's two notes to itself, about a tree that is now gone.
  RMDir /r "$R0.next"
  RMDir /r "$R0.partial"
  RMDir /r "$R0.old"
  Delete "$LOCALAPPDATA\${BUNDLEID}\dsh-checked"
  Delete "$LOCALAPPDATA\${BUNDLEID}\dsh-skipped"
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ; Compile-time only, and here because this is the first thing the template
  ; expands out of this file after it has defined `MANUPRODUCTKEY`.
  !insertmacro DshCheckKey

  ; $1 through $3 are not used directly here, but `DshMigrate` reaches
  ; `DshPathDrop`, which works in them.
  Push $0
  Push $1
  Push $2
  Push $3
  Push $R0
  Push $R5

  ; The same directory `src-tauri/src/dsh.rs` resolves through Tauri's
  ; `app_local_data_dir()` — `%LOCALAPPDATA%\<identifier>`. The three of them —
  ; here, there, and `install-deps.ps1` — have to agree.
  StrCpy $R0 "$LOCALAPPDATA\${BUNDLEID}\dsh"
  StrCpy $R5 "$INSTDIR\bin"

  !insertmacro DshMigrate

  ; And that is all this hook does now.
  ;
  ; It used to run `${DSH_SCRIPT} -Mode install` here, which made this the only
  ; platform where the script re-ran on an upgrade — and so the only platform a
  ; bumped `NODE_VERSION`, a rewritten launcher, or a new migration ever reached.
  ; macOS and Linux have no installer to hang that off, so a runtime there was
  ; built once at first launch and never touched again.
  ;
  ; The app does it instead, on all three, from `provision` in
  ; `src-tauri/src/dsh.rs`: it knows which version built the runtime it is
  ; looking at, so it can tell "already done" from "done by an older release" —
  ; which an installer hook, running before the app has ever started, cannot.
  ;
  ; The script still ships in `$INSTDIR`; `NSIS_HOOK_PREUNINSTALL` takes it
  ; along so the uninstaller can run `-Mode uninstall` with it.
  ;
  ; The visible cost is that the first launch after installing is the one that
  ; downloads Node and dsh, rather than the installer's progress pane. It has
  ; the loading page and the same `::progress` lines to report on, and it is
  ; what every macOS and Linux install has always done.

  Pop $R5
  Pop $R0
  Pop $3
  Pop $2
  Pop $1
  Pop $0
!macroend

; The uninstaller runs the same script, and by the time it does `$INSTDIR` is
; gone — `NSIS_HOOK_POSTUNINSTALL` runs after the app's own files are deleted —
; so the script is taken along now, into the temporary directory NSIS cleans up
; on its own.
;
; `bootstrap.json` used to be carried along beside it, and restored afterwards
; if the "delete app data" checkbox had taken it, because the marker was the
; only record of which Node was ours and which npm could remove dsh. There is no
; marker any more and nothing to preserve: everything the uninstaller removes is
; at a path it can name outright.
;
; This hook runs ahead of the generated `CheckIfAppIsRunning`, so it may well be
; running for an uninstall the user is about to abort. Copying one file costs
; nothing in that case.
!macro NSIS_HOOK_PREUNINSTALL
  InitPluginsDir
  ${If} ${FileExists} "$INSTDIR\${DSH_SCRIPT}"
    CopyFiles /SILENT "$INSTDIR\${DSH_SCRIPT}" "$PLUGINSDIR\install-deps.ps1"
  ${EndIf}
!macroend

; Removing Node and dsh runs last, not first.
;
; A hook in `NSIS_HOOK_PREUNINSTALL` would run before the generated
; `CheckIfAppIsRunning`, and so would tear down what the running app is
; executing out of and only then offer to abort. By the time this one runs the
; app's own files are already gone and there is nothing left to change its mind
; about.
!macro NSIS_HOOK_POSTUNINSTALL
  Push $0
  Push $1
  Push $2
  Push $3
  Push $R0
  Push $R1
  Push $R4
  Push $R5

  ; Not every run of this uninstaller is a removal, and the two that are not
  ; both end with the app still installed:
  ;
  ; `$UpdateMode` is the app updater. And an upgrade the user runs by hand takes
  ; the uninstaller with it — the reinstall page's default for a newer version
  ; is "uninstall first", which the installer performs by launching
  ; `"$INSTDIR\uninstall.exe" … _?=$INSTDIR`. `_?=` is the one case where NSIS
  ; does not copy the uninstaller to a temporary directory before running it, so
  ; that path is the path where `$EXEDIR` is still `$INSTDIR`.
  ;
  ; Neither is a moment to throw away a Node and a 327 MB dsh the reinstall
  ; would only have to fetch again — and certainly not one to ask about any of
  ; it in the middle of an update.
  ${If} $UpdateMode = 1
  ${OrIf} $EXEDIR == $INSTDIR
    DetailPrint "这是更新或重装，保留已安装的 Node 和 dsh。"
    Goto uninstall_done
  ${EndIf}

  ; The old private tree, if an install from before this scheme left one and the
  ; upgrade path above never ran.
  StrCpy $R5 "$INSTDIR\bin"
  StrCpy $R0 "$LOCALAPPDATA\${BUNDLEID}\dsh"
  !insertmacro DshPathDrop "Un" "$R5"
  RMDir /r "$R0"
  RMDir /r "$R0.next"
  RMDir /r "$R0.partial"
  RMDir /r "$R0.old"
  ; `$INSTDIR` itself: the generated uninstaller already tried `RMDir` on it and
  ; failed if the old shim directory was still holding it open.
  RMDir "$R5"
  RMDir "$INSTDIR"

  ; One question, where there used to be two.
  ;
  ; The old pair had to ask about dsh and about Node separately, because dsh
  ; might have been the user's own — installed globally with their npm, into
  ; their prefix — while only a Node under `%LOCALAPPDATA%` was ever ours. That
  ; distinction was read out of `bootstrap.json`, and getting it wrong meant an
  ; uninstaller running `npm uninstall -g` against a dsh it had never installed.
  ;
  ; Nothing outside `%LOCALAPPDATA%\${BUNDLEID}` is ours any more, and
  ; everything inside it is. So there is one thing to remove, it is named by a
  ; path rather than by a marker, and a dsh the user installed themselves cannot
  ; be reached from here at all.
  ${If} ${Silent}
    Goto uninstall_data
  ${EndIf}
  ${IfNot} ${FileExists} "$LOCALAPPDATA\${BUNDLEID}\runtime\node\node.exe"
    Goto uninstall_data
  ${EndIf}
  ${IfNot} ${FileExists} "$PLUGINSDIR\install-deps.ps1"
    DetailPrint "找不到卸载脚本，dsh 运行时保持原样。"
    Goto uninstall_data
  ${EndIf}

  MessageBox MB_YESNO|MB_ICONQUESTION|MB_DEFBUTTON2 "是否同时删除本应用安装的 dsh 运行时？$\r$\n$\r$\n它装在 $LOCALAPPDATA\${BUNDLEID}，包含一个专用的 Node 和 dsh 本体，约 360 MB。$\r$\n$\r$\n你自己安装的 Node 和 dsh 不受影响。" IDNO uninstall_data

  SetDetailsPrint both
  DetailPrint "正在清理…"
  SetDetailsPrint lastused
  nsExec::ExecToLog '${DSH_POWERSHELL} "$PLUGINSDIR\install-deps.ps1" -Mode uninstall'
  Pop $0
  ${If} $0 != "0"
    DetailPrint "清理时出错（退出码 $0）。"
  ${EndIf}

  uninstall_data:
  ; The launcher directory is gone — removed just now, or taken out from under us
  ; by the template's "delete app data" checkbox, which has already had its turn
  ; at `%LOCALAPPDATA%\${BUNDLEID}` by the time this hook runs — so its PATH
  ; entry now points at nothing and comes off. A runtime the user chose to keep
  ; still has its launcher, and keeps its entry.
  ;
  ; The script drops the same entry itself, and dropping it twice is a no-op.
  ; This is for the paths where the script never ran: a silent uninstall, or the
  ; checkbox having deleted the runtime before we could ask about it.
  ${IfNot} ${FileExists} "$LOCALAPPDATA\${BUNDLEID}\bin\dsh.cmd"
    !insertmacro DshPathDrop "Un" "$LOCALAPPDATA\${BUNDLEID}\bin"
  ${EndIf}
  ; And the entry the pre-runtime scheme left, for a machine upgrading straight
  ; past it into an uninstall.
  !insertmacro DshPathDrop "Un" "$LOCALAPPDATA\${BUNDLEID}\node"

  ; `$DSH_HOME` is the user's own: settings, profiles, conversation history.
  ; None of it is ours to throw away without being told to, and a reinstall
  ; picks up exactly where they left off if it stays.
  ReadEnvStr $R4 "DSH_HOME"
  ${If} $R4 == ""
    StrCpy $R4 "$PROFILE\.dsh"
  ${EndIf}

  ; Never in silent mode: there is nobody there to ask, and the answer this
  ; would have to assume is the destructive one.
  ${If} ${Silent}
    Goto uninstall_done
  ${EndIf}
  ${IfNot} ${FileExists} "$R4\*.*"
    Goto uninstall_done
  ${EndIf}

  MessageBox MB_YESNO|MB_ICONQUESTION|MB_DEFBUTTON2 "是否同时删除 dsh 的配置和会话数据？$\r$\n$\r$\n$R4$\r$\n$\r$\n选「否」将保留这些数据，重新安装后可以接着用。" IDNO uninstall_done
  DetailPrint "正在删除 $R4…"
  RMDir /r "$R4"

  uninstall_done:
  ; Nothing to put back. This label used to restore a `bootstrap.json` the hook
  ; had copied aside so the uninstall script could read it, then delete it again;
  ; there is no marker now, so every path through this macro simply lands here.

  Pop $R5
  Pop $R4
  Pop $R1
  Pop $R0
  Pop $3
  Pop $2
  Pop $1
  Pop $0
!macroend
