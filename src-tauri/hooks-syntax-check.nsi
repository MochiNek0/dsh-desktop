; A compile-only harness for `installer-hooks.nsh`.
;
; The hooks file is not a script on its own: it is `!include`d into the
; installer Tauri generates, and it leans on defines and variables that only
; exist there. Getting it wrong is expensive to find out — the mistake surfaces
; as a failed `npm run build` minutes into a bundle, or worse, as an installer
; that compiles and then misbehaves on a user's machine.
;
; So this stands in for that context: the same defines the template sets, the
; same includes it makes first, and the same four hook macros invoked once each
; so their bodies are actually compiled rather than merely parsed.
;
; Run it with:
;   makensis.exe /V2 hooks-syntax-check.nsi
;
; It writes a throwaway .exe to the temp directory and is never shipped.

Unicode true

; --- what the template defines before including the hooks -------------------
!define MANUFACTURER "mochineko"
!define PRODUCTNAME "dsh-desktop"
!define BUNDLEID "ai.deepseek.dsh.desktop"
!define MANUKEY "Software\${MANUFACTURER}"
!define MANUPRODUCTKEY "${MANUKEY}\${PRODUCTNAME}"
!define MAINBINARYNAME "dsh-desktop"

; --- what the template includes before the hooks ----------------------------
!include MUI2.nsh
!include FileFunc.nsh
!include x64.nsh
!include WordFunc.nsh

; `$UpdateMode` is the template's, and the uninstall hook reads it.
Var UpdateMode

Name "${PRODUCTNAME}"
OutFile "$%TEMP%\dsh-hooks-syntax-check.exe"
InstallDir "$LOCALAPPDATA\${PRODUCTNAME}"
RequestExecutionLevel user

!include "installer-hooks.nsh"

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
