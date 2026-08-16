; Installing and removing dsh itself, which the installer no longer carries.
;
; The app ships a Node runtime and npm and nothing else: dsh is 327 MB unpacked
; and compresses far better as an npm download than as part of this installer,
; so it is fetched here instead. That puts the wait where a user expects one —
; inside an installer's progress — rather than in front of a window they just
; opened.
;
; There is no second chance at first launch. If every registry below fails the
; install says so and asks the user to fix their network and run it again; an
; app that quietly finished installing without the thing it runs would be worse
; than one that says it did not.
;
; It also puts a `dsh` command on PATH, so the terminal gets the same dsh the
; app runs and the same updates — but only on a machine that has none of its
; own. See `DshShimWrite` for what that command is and `DshPathAdd` for how it
; gets there.
;
; Wired up by `tauri.conf.json` under `bundle.windows.nsis.installerHooks`.
; This file must stay UTF-8 with a BOM — the generated installer is built with
; `Unicode true`, and without the BOM NSIS reads the messages below as ANSI.

!include LogicLib.nsh
!include WinMessages.nsh
!include StrFunc.nsh

${Using:StrFunc} StrRep
${Using:StrFunc} UnStrRep

!define DSH_PACKAGE "@deepseek-ai/dsh"

; Tried in order, and only until one works. The first attempt passes no
; registry at all, so a user with an `.npmrc` — a private mirror, a corporate
; proxy — gets what they configured rather than having it overridden. The rest
; are public mirrors, for the case where the default is unreachable.
!define DSH_MIRROR_1 "https://registry.npmmirror.com/"
!define DSH_MIRROR_2 "https://mirrors.cloud.tencent.com/npm/"
!define DSH_MIRROR_3 "https://mirrors.huaweicloud.com/repository/npm/"

; One `npm install` attempt. `registry` is either empty, for whatever npm
; resolves on its own, or a `--registry=` argument.
;
; $R0 install directory   $R1 node.exe   $R2 npm-cli.js   $R3 done flag
!macro DshInstallFrom label registry
  ${If} $R3 == "0"
    DetailPrint "正在安装 dsh（${label}）…"
    nsExec::ExecToLog '"$R1" "$R2" install --omit=dev --no-audit --no-fund --loglevel=http ${registry} "${DSH_PACKAGE}@latest"'
    Pop $0
    ${If} $0 == "0"
      StrCpy $R3 "1"
      DetailPrint "dsh 安装完成（${label}）。"
    ${Else}
      DetailPrint "从 ${label} 安装失败（退出码 $0），换下一个源重试。"
    ${EndIf}
  ${EndIf}
!macroend

; The `dsh` command this app puts on PATH, in a directory of its own so that
; adding it to PATH adds nothing else — the bundled Node sits one level up, and
; putting *that* directory on PATH would shadow whatever `node` the user
; already has.
;
; Both shims work out their own location at runtime — `%~dp0`, `dirname $0` —
; rather than carrying `$INSTDIR` in their text. A batch file is bytes read in
; the console's code page, and the default install directory contains the
; user's name, which survives that trip only where the ANSI and OEM code pages
; agree. What is written instead is ASCII: `%LOCALAPPDATA%` and the bundle
; identifier, expanded by the shell out of an environment that is Unicode
; whatever the code page says.
!macro DshShimWrite dir
  CreateDirectory "${dir}"

  ; cmd.exe and PowerShell, which both find a bare `dsh` through PATHEXT.
  FileOpen $0 "${dir}\dsh.cmd" w
  FileWrite $0 "@echo off$\r$\n"
  FileWrite $0 "setlocal$\r$\n"
  ; dsh shells out to `node` for workers and plugin tooling; point those at the
  ; runtime we shipped, exactly as `Install::command` does in `src/dsh.rs`.
  FileWrite $0 "set $\"PATH=%~dp0..\resources\runtime;%PATH%$\"$\r$\n"
  FileWrite $0 "$\"%~dp0..\resources\runtime\node.exe$\" $\"%LOCALAPPDATA%\${BUNDLEID}\dsh\node_modules\@deepseek-ai\dsh\lib\bin.js$\" %*$\r$\n"
  FileClose $0

  ; Git Bash and anything else MSYS, which look for a name with no extension.
  ; `$$` is a literal `$`: none of these are NSIS variables.
  FileOpen $0 "${dir}\dsh" w
  FileWrite $0 "#!/bin/sh$\n"
  FileWrite $0 "here=$$(dirname $\"$$0$\")$\n"
  FileWrite $0 "export PATH=$\"$$here/../resources/runtime:$$PATH$\"$\n"
  FileWrite $0 "exec $\"$$here/../resources/runtime/node.exe$\" $\"$$LOCALAPPDATA/${BUNDLEID}/dsh/node_modules/@deepseek-ai/dsh/lib/bin.js$\" $\"$$@$\"$\n"
  FileClose $0

  DetailPrint "已安装 dsh 命令：${dir}\dsh.cmd"
!macroend

!macro DshShimRemove dir
  Delete "${dir}\dsh.cmd"
  Delete "${dir}\dsh"
  RMDir "${dir}"
!macroend

; Tell everything already running that the environment changed. Without it the
; new PATH reaches nothing until the next sign-in.
!macro DshPathBroadcast
  SendMessage ${HWND_BROADCAST} ${WM_SETTINGCHANGE} 0 "STR:Environment" /TIMEOUT=5000
!macroend

; Put `dir` on the user's own PATH — `HKCU\Environment`, the short one, never
; the machine's. Written back as `REG_EXPAND_SZ`, which is what Windows' own
; environment editor writes and what keeps a `%USERPROFILE%` in somebody else's
; entry meaning what it says.
;
; $0 the PATH   $1 its length   $2 padded for the search   $3 what gets written
!macro DshPathAdd dir
  ReadRegStr $0 HKCU "Environment" "Path"

  ; A PATH that already ends in a separator would otherwise come back with an
  ; empty entry in the middle of it, and an empty entry is how PATH spells the
  ; current directory. Trailing, it means nothing and drops harmlessly.
  StrCpy $2 $0 "" -1
  ${If} $2 == ";"
    StrCpy $0 $0 -1
  ${EndIf}

  ; NSIS strings stop at 1024 characters, and `ReadRegStr` truncates to fit
  ; without saying so. Writing that back would take everything past the cut
  ; with it, so a PATH anywhere near the limit is left exactly as it is: the
  ; cost is a convenience the user can add by hand, and the alternative is
  ; destroying entries this installer has no business touching.
  StrLen $1 $0
  StrLen $2 "${dir}"
  IntOp $1 $1 + $2
  ${If} $1 >= 1000
    DetailPrint "PATH 已接近长度上限，不作改动；dsh 命令未加入 PATH。"
  ${Else}
    ; Padded at both ends so the first and last entries match like any other.
    StrCpy $2 ";$0;"
    ${StrRep} $3 $2 ";${dir};" ";"
    ${If} $3 == $2
      ${If} $0 == ""
        StrCpy $3 "${dir}"
      ${Else}
        StrCpy $3 "$0;${dir}"
      ${EndIf}
      WriteRegExpandStr HKCU "Environment" "Path" $3
      !insertmacro DshPathBroadcast
      DetailPrint "已把 dsh 命令加入 PATH。"
    ${EndIf}
  ${EndIf}
!macroend

; Take `dir` back off the user's PATH. `un` is `Un` in the uninstaller, where
; StrFunc's copies of its functions answer to their own names.
!macro DshPathDrop un dir
  ReadRegStr $0 HKCU "Environment" "Path"

  StrLen $1 $0
  ${If} $1 >= 1000
    DetailPrint "PATH 已接近长度上限，不作改动。"
  ${Else}
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
      DetailPrint "已把 dsh 命令移出 PATH。"
    ${EndIf}
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTINSTALL
  Push $0
  Push $1
  Push $2
  Push $3
  Push $R0
  Push $R1
  Push $R2
  Push $R3
  Push $R4
  Push $R5

  ; The same directory `src-tauri/src/dsh.rs` resolves through Tauri's
  ; `app_local_data_dir()` — `%LOCALAPPDATA%\<identifier>`. The two have to agree.
  StrCpy $R0 "$LOCALAPPDATA\${BUNDLEID}\dsh"
  StrCpy $R1 "$INSTDIR\resources\runtime\node.exe"
  StrCpy $R2 "$INSTDIR\resources\runtime\node_modules\npm\bin\npm-cli.js"
  StrCpy $R3 "0"
  StrCpy $R5 "$INSTDIR\bin"

  ; A dsh the user installed themselves. `npm install -g` puts `dsh.cmd` on
  ; PATH, which is exactly what `server::default_bin` runs when the app finds no
  ; copy of its own — so testing for that one name is testing for the thing that
  ; would actually get used.
  ;
  ; It stays theirs: nothing below installs over it, the uninstaller leaves an
  ; install it did not make alone, and the update check only tells the user
  ; about a new version rather than reaching into someone else's npm prefix to
  ; apply it.
  ;
  ; The shim this installer writes is on PATH under that same name, and is not
  ; a dsh the user installed — so it does not count as one here. Without this
  ; the second run would find our own command and read it as somebody else's.
  SearchPath $R4 "dsh.cmd"
  ${If} $R4 == "$R5\dsh.cmd"
    StrCpy $R4 ""
  ${EndIf}

  ; An app update runs this installer again, over a machine that already has a
  ; dsh — and one that may be newer than what the registry called `latest` when
  ; this installer was built. Leave it alone; the app checks for updates itself.
  ${If} ${FileExists} "$R0\node_modules\@deepseek-ai\dsh\lib\bin.js"
    DetailPrint "dsh 已安装，跳过。"
    StrCpy $R3 "1"
  ${ElseIf} $R4 != ""
    ; Installing a second 327 MB tree next to a working dsh is 327 MB nobody
    ; asked for.
    DetailPrint "检测到系统里已有 dsh（$R4），跳过安装。"
    StrCpy $R3 "1"
  ${EndIf}

  ${If} $R3 == "0"
    SetDetailsPrint both
    DetailPrint "正在下载 dsh，约 185 MB，请耐心等待…"

    CreateDirectory "$R0"
    SetOutPath "$R0"

    !insertmacro DshInstallFrom "默认源" ""
    !insertmacro DshInstallFrom "npmmirror" "--registry=${DSH_MIRROR_1}"
    !insertmacro DshInstallFrom "腾讯云" "--registry=${DSH_MIRROR_2}"
    !insertmacro DshInstallFrom "华为云" "--registry=${DSH_MIRROR_3}"

    ; Back out of the install directory before anything might remove it, and so
    ; the uninstaller is not left with this as its working directory.
    SetOutPath "$INSTDIR"

    ${If} $R3 == "0"
      ; Whatever npm managed to unpack is an incomplete tree. The app tests for
      ; the entry point and would treat it as missing anyway, so this is only
      ; about not leaving a few hundred megabytes of it behind.
      RMDir /r "$R0"
      ${IfNot} ${Silent}
        MessageBox MB_OK|MB_ICONEXCLAMATION "dsh 下载失败。$\r$\n$\r$\n已尝试默认源和 npmmirror、腾讯云、华为云三个镜像，都没有成功，通常是网络或代理的问题。$\r$\n$\r$\n请换一个网络或代理后重新运行安装程序。应用本身已经装好，但在 dsh 装上之前无法使用。"
      ${EndIf}
    ${EndIf}

    SetDetailsPrint lastused
  ${EndIf}

  ; The terminal command, on the same terms as the install above: only where
  ; the machine has no dsh of its own, and only when there is one of ours for
  ; it to point at — an install that failed leaves nothing to run, and a `dsh`
  ; that exits with a module-not-found is worse than no `dsh` at all.
  ;
  ; The other direction matters too. A user who ran `npm install -g` after
  ; installing this app now has two commands answering to one name, and which
  ; one wins is down to the order of their PATH; theirs is the one they typed
  ; the command for, so ours gets out of the way.
  ${If} $R4 == ""
  ${AndIf} ${FileExists} "$R0\node_modules\@deepseek-ai\dsh\lib\bin.js"
    !insertmacro DshShimWrite "$R5"
    !insertmacro DshPathAdd "$R5"
  ${Else}
    !insertmacro DshShimRemove "$R5"
    !insertmacro DshPathDrop "" "$R5"
  ${EndIf}

  Pop $R5
  Pop $R4
  Pop $R3
  Pop $R2
  Pop $R1
  Pop $R0
  Pop $3
  Pop $2
  Pop $1
  Pop $0
!macroend

; Removing dsh runs last, not first.
;
; The generated script inserts `NSIS_HOOK_PREUNINSTALL` ahead of its own
; `CheckIfAppIsRunning`, so a hook there would delete the tree the running app
; is executing out of and only then offer to abort — leaving an installed app
; with no dsh. By the time this one runs, the app's own files are already gone
; and there is nothing left to change its mind about.
;
; It also runs after the template's `Delete app data` checkbox has had its turn
; at `$LOCALAPPDATA\${BUNDLEID}`, which is where the dsh tree lives. That is why
; the removal below is unconditional rather than tied to the checkbox: dsh is a
; program this app installed, not the user's data, and it should go whether or
; not they asked for their data to go with it.
!macro NSIS_HOOK_POSTUNINSTALL
  Push $0
  Push $1
  Push $2
  Push $3
  Push $R0
  Push $R4

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
  ; Neither is a moment to throw away a 327 MB dsh the reinstall would only have
  ; to fetch again — and certainly not one to ask about conversation history in
  ; the middle of an update.
  ${If} $UpdateMode = 1
  ${OrIf} $EXEDIR == $INSTDIR
    DetailPrint "这是更新或重装，保留已安装的 dsh。"
    Goto uninstall_done
  ${EndIf}

  ; The terminal command and the PATH entry that reaches it. The generated
  ; uninstaller knows nothing about either — it deletes the files it shipped and
  ; then tries `RMDir "$INSTDIR"`, which the shim directory has just made fail —
  ; so removing them, and then the directory they were holding open, is ours.
  !insertmacro DshShimRemove "$INSTDIR\bin"
  RMDir "$INSTDIR"
  !insertmacro DshPathDrop "Un" "$INSTDIR\bin"

  ; The same directory `src-tauri/src/dsh.rs` resolves through Tauri's
  ; `app_local_data_dir()` — `%LOCALAPPDATA%\<identifier>`. The two have to agree.
  StrCpy $R0 "$LOCALAPPDATA\${BUNDLEID}\dsh"

  ; dsh is a program this app installed, so it goes with it — including the
  ; trees a download in flight or a completed swap left under other names (see
  ; `staging_dir`, `partial_dir` and `swap` in `src-tauri/src/dsh.rs`).
  DetailPrint "正在删除 dsh…"
  RMDir /r "$R0"
  RMDir /r "$R0.next"
  RMDir /r "$R0.partial"
  RMDir /r "$R0.old"

  ; The update check's two notes to itself, on the same reasoning: written by
  ; this app about a dsh that is now gone, and meaningless without it. They sit
  ; beside the tree rather than inside it so that promoting a download does not
  ; take them with it — see `checked_file` and `skip_file` in `dsh.rs`.
  Delete "$LOCALAPPDATA\${BUNDLEID}\dsh-checked"
  Delete "$LOCALAPPDATA\${BUNDLEID}\dsh-skipped"

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
  Pop $R4
  Pop $R0
  Pop $3
  Pop $2
  Pop $1
  Pop $0
!macroend
