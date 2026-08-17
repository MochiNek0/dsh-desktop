; Getting the machine to a working dsh, and taking it back off on the way out.
;
; Neither is implemented here. `resources/install-deps.ps1` does the work —
; detect Node, install one under %LOCALAPPDATA% if the machine has none, then
; `npm install -g @deepseek-ai/dsh` — and this file is the installer's half of
; calling it. The app calls the same script when a launch finds no dsh; see
; `src-tauri/src/dsh.rs`. Doing it here in NSIS as well would mean a second
; implementation of SHA256 verification and zip extraction on top of certutil
; and tar, kept in step with the first one by hand.
;
; The wait is put inside the installer's progress on purpose: an install pulls
; 185 MB of dsh and possibly 35 MB of Node on top, and that is a wait a user
; expects from an installer and does not expect from a window they just opened.
; When it fails the install says so rather than quietly finishing without the
; thing it runs — but it is no longer the only chance, because the app can now
; run the same script itself.
;
; Nothing here needs elevation. The script writes under %LOCALAPPDATA% and to
; HKCU\Environment, both of which the current user owns.
;
; Wired up by `tauri.conf.json` under `bundle.windows.nsis.installerHooks`.
; This file must stay UTF-8 with a BOM — the generated installer is built with
; `Unicode true`, and without the BOM NSIS reads the messages below as ANSI.

!include LogicLib.nsh
!include WinMessages.nsh
!include StrFunc.nsh

${Using:StrFunc} StrRep
${Using:StrFunc} UnStrRep

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
; $0 the PATH   $1 its length   $2 padded for the search   $3 what gets written
!macro DshPathDrop un dir
  ReadRegStr $0 HKCU "Environment" "Path"

  ; NSIS strings stop at 1024 characters, and `ReadRegStr` truncates to fit
  ; without saying so. Writing that back would take everything past the cut with
  ; it, so a PATH anywhere near the limit is left exactly as it is.
  StrLen $1 $0
  ${If} $1 < 1000
    ; Padded at both ends so the first and last entries match like any other.
    StrCpy $2 ";$0;"
    ${${un}StrRep} $3 $2 ";${dir};" ";"
    ${If} $3 != $2
      ; Drop the semicolons the search needed back off both ends.
      StrCpy $3 $3 "" 1
      StrCpy $3 $3 -1
      ${If} $3 == ""
        DeleteRegValue HKCU "Environment" "Path"
      ${Else}
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

  SetDetailsPrint both
  DetailPrint "正在检查 Node 和 dsh…"
  SetDetailsPrint lastused

  ; Everything the script does — what it found, what it decided to install, and
  ; npm's own http log while it runs — goes into the details pane, which is the
  ; only thing moving during a download that takes minutes.
  nsExec::ExecToLog '${DSH_POWERSHELL} "$INSTDIR\${DSH_SCRIPT}" -Mode install'
  Pop $0

  ${If} $0 != "0"
    SetDetailsPrint both
    DetailPrint "Node 或 dsh 安装失败（退出码 $0）。"
    SetDetailsPrint lastused
    ${IfNot} ${Silent}
      MessageBox MB_OK|MB_ICONEXCLAMATION "dsh 没有安装成功。$\r$\n$\r$\n通常是网络或代理的问题 —— 安装过程需要从 nodejs.org 和 npm 下载。$\r$\n$\r$\n应用本身已经装好了，下次启动时它会再试一次；也可以换一个网络或代理后重新运行安装程序。"
    ${EndIf}
  ${EndIf}

  Pop $R5
  Pop $R0
  Pop $3
  Pop $2
  Pop $1
  Pop $0
!macroend

; The uninstaller runs the same script, and `$INSTDIR` will not be there when it
; does — `NSIS_HOOK_POSTUNINSTALL` runs after the app's own files are deleted.
; So the script is taken along now, into the temporary directory NSIS cleans up
; on its own.
;
; This hook runs ahead of the generated `CheckIfAppIsRunning`, so it may well be
; running for an uninstall the user is about to abort. Copying a file costs
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

  ; Nothing to ask about on a machine this app never installed anything on —
  ; `bootstrap.json` is written by the script the moment it installs something.
  ; The script is the final authority either way: it declines to remove a Node
  ; or a dsh that was already there when it arrived.
  ${IfNot} ${FileExists} "$LOCALAPPDATA\${BUNDLEID}\bootstrap.json"
    Goto uninstall_data
  ${EndIf}
  ${If} ${Silent}
    Goto uninstall_data
  ${EndIf}
  ${IfNot} ${FileExists} "$PLUGINSDIR\install-deps.ps1"
    DetailPrint "找不到卸载脚本，Node 和 dsh 保持原样。"
    Goto uninstall_data
  ${EndIf}

  StrCpy $R1 ""

  MessageBox MB_YESNO|MB_ICONQUESTION|MB_DEFBUTTON2 "是否同时卸载 dsh？$\r$\n$\r$\n选「否」会保留它，终端里的 dsh 命令仍然可用。" IDNO keep_dsh
  StrCpy $R1 "$R1 -RemoveDsh"
  keep_dsh:

  ; Only worth asking where there is a Node of ours to remove. The directory
  ; only exists because the script put it there.
  ${IfNot} ${FileExists} "$LOCALAPPDATA\${BUNDLEID}\node\node.exe"
    Goto keep_node
  ${EndIf}
  MessageBox MB_YESNO|MB_ICONQUESTION|MB_DEFBUTTON2 "是否同时卸载本应用安装的 Node.js？$\r$\n$\r$\n它装在 $LOCALAPPDATA\${BUNDLEID}\node，不影响你自己装的 Node。$\r$\n$\r$\n注意：dsh 依赖 Node 才能运行，删掉 Node 会连同 dsh 一起删除。" IDNO keep_node
  StrCpy $R1 "$R1 -RemoveNode"
  keep_node:

  ${If} $R1 != ""
    SetDetailsPrint both
    DetailPrint "正在清理…"
    SetDetailsPrint lastused
    nsExec::ExecToLog '${DSH_POWERSHELL} "$PLUGINSDIR\install-deps.ps1" -Mode uninstall$R1'
    Pop $0
    ${If} $0 != "0"
      DetailPrint "清理时出错（退出码 $0）。"
    ${EndIf}
  ${EndIf}

  uninstall_data:
  ; A Node of ours that is no longer there — removed just now, or taken out from
  ; under us by the template's "delete app data" checkbox, which has already had
  ; its turn at `%LOCALAPPDATA%\${BUNDLEID}` by the time this hook runs. Either
  ; way its PATH entry now points at nothing. A Node the user chose to keep
  ; still exists, and keeps its entry.
  ${IfNot} ${FileExists} "$LOCALAPPDATA\${BUNDLEID}\node\node.exe"
    !insertmacro DshPathDrop "Un" "$LOCALAPPDATA\${BUNDLEID}\node"
  ${EndIf}

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
  Pop $R5
  Pop $R4
  Pop $R1
  Pop $R0
  Pop $3
  Pop $2
  Pop $1
  Pop $0
!macroend
