; A compile-only harness for `installer-hooks.nsh`.
;
; The hooks file is not a script on its own: it is `!include`d into the
; installer Tauri generates, and it leans on defines and variables that only
; exist there. Getting it wrong is expensive to find out — the mistake surfaces
; as a failed `npm run build` minutes into a bundle, or worse, as an installer
; that compiles and then misbehaves on a user's machine.
;
; So this stands in for that context: the same includes, the same defines, the
; same four hook macros invoked once each so their bodies are actually compiled
; rather than merely parsed — and, load-bearing, all of it in the same ORDER the
; template puts it in. The hooks are included before the defines there, so a
; harness that defines them first proves less than it appears to: every
; reference resolves here that would not resolve in a real build. It did once,
; and it cost a shipped defect; see the note above the `!include` below.
;
; Run it with:
;   makensis.exe /V2 hooks-syntax-check.nsi
;
; It writes a throwaway .exe to the temp directory and is never shipped.

Unicode true

; --- what the template includes before the hooks ----------------------------
!include MUI2.nsh
!include FileFunc.nsh
!include x64.nsh
!include WordFunc.nsh

; --- the hooks, where the template includes them ----------------------------
;
; Before the defines, not after. That is where the generated installer puts
; the `!include`: the last line above its own first `!define`. So anything the
; hooks file resolves at parse time -- the body of a Function, or a macro
; expanded into one -- sees none of the template's defines.
;
; This harness used to define them first. Every reference then appeared to
; resolve, and it hid a real defect for as long as it stood: the guard in
; `DshPreferRoomierDrive` read a registry key literally named
; ${MANUPRODUCTKEY} in every shipped installer, so it never found a previous
; install and never stopped one being moved to another drive. NSIS warned
; about it the whole time, in the real build, where nobody was reading. Keep
; this order.
!include "installer-hooks.nsh"

; --- what the template defines after including the hooks --------------------
!define MANUFACTURER "mochineko"
!define PRODUCTNAME "dsh-desktop"
!define BUNDLEID "ai.deepseek.dsh.desktop"
!define MANUKEY "Software\${MANUFACTURER}"
!define MANUPRODUCTKEY "${MANUKEY}\${PRODUCTNAME}"
!define MAINBINARYNAME "dsh-desktop"

; `$UpdateMode` is the template's, and the uninstall hook reads it.
Var UpdateMode

Name "${PRODUCTNAME}"
OutFile "$%TEMP%\dsh-hooks-syntax-check.exe"
InstallDir "$LOCALAPPDATA\${PRODUCTNAME}"
RequestExecutionLevel user

; The template inserts its pages before its languages, and MUI writes
; `.onGUIInit` — the thing that calls MUI_CUSTOMFUNCTION_GUIINIT, which the
; hooks file defines — while expanding these. So the order matters here as much
; as it does there.
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

Section Install
  !insertmacro NSIS_HOOK_POSTINSTALL
SectionEnd

Section Uninstall
  !insertmacro NSIS_HOOK_PREUNINSTALL
  !insertmacro NSIS_HOOK_POSTUNINSTALL
SectionEnd
