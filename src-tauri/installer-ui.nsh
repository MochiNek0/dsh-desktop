; The installer's own pages: the mark and the product name at the top of every
; one, and underneath it whatever that step needs -- where to install and a
; button, a progress bar, or two checkboxes and a button.
;
;   DshWelcomeCreate    in place of MUI2's welcome and directory pages
;   DshReinstallCreate  the template's "already installed" page, drawn by us
;                       but still driven by the template's own logic
;   DshProgressShow     MUI2's instfiles page, restyled rather than replaced:
;                       the sections run on that page and nowhere else
;   DshFinishCreate     in place of MUI2's finish page
;
; Built from nsDialogs and `System::Call` alone -- no plugin of our own. Each
; page is laid over the whole window rather than the usual inner rectangle,
; and everything MUI2 draws around a page (header, buttons, branding line) is
; hidden for the whole run. The uninstaller is untouched.
;
; `!include`d by `installer.nsi`, which declares the pages themselves, after it
; has defined `PRODUCTNAME` and `$PassiveMode`. The finish page calls the
; template's `CreateOrUpdateDesktopShortcut` and `RunMainBinary`.
;
; UTF-8 with a BOM, for the same reason as `installer-hooks.nsh`.

!include nsDialogs.nsh
!include WinMessages.nsh
!include FileFunc.nsh

; Tells the outer window to move to the next page -- what the Next button
; does, for pages whose Next button is hidden.
!define DSH_WM_NOTIFY_OUTER_NEXT 0x408

; Colours. `SetCtlColors` takes RRGGBB; the progress bar takes a COLORREF,
; which is the same colour with the bytes the other way round.
!define DSH_INK "1F2329"
!define DSH_INK_REF 0x0029231F
!define DSH_MUTED "8A8F99"
!define DSH_PAPER "FFFFFF"
!define DSH_TRACK_REF 0x00EDEAE8

; Read by the first page before any section runs. Under solid compression a
; `File` that is not reserved sits wherever the compiler first met it, and
; getting to it means decompressing the whole app first.
ReserveFile "${DSH_HOOKS_DIR}\installer\brand-100.bmp"
ReserveFile "${DSH_HOOKS_DIR}\installer\brand-125.bmp"
ReserveFile "${DSH_HOOKS_DIR}\installer\brand-150.bmp"
ReserveFile "${DSH_HOOKS_DIR}\installer\brand-175.bmp"
ReserveFile "${DSH_HOOKS_DIR}\installer\brand-200.bmp"

; The page being shown: an nsDialogs page, or MUI2's instfiles dialog.
Var DshPage
; Set up once for the whole run by `DshSetup`.
Var DshReady
Var DshDpi
Var DshBrand
Var DshBrandSize
Var DshFontName
Var DshFontBody
Var DshFontSmall
; On the pages that have them.
Var DshBrandCtl
Var DshNameCtl
Var DshDir
Var DshShortcut
Var DshRun

; Logical pixels (96 DPI) to physical ones for this window.
!macro DshPx out value
  IntOp ${out} ${value} * $DshDpi
  IntOp ${out} ${out} / 96
!macroend

; `out` = the width of `text` in the font `hwnd` is set in.
!macro DshTextWidth out hwnd text
  Push $8
  Push $9
  System::Call "user32::GetDC(p ${hwnd}) p .r8"
  SendMessage ${hwnd} ${WM_GETFONT} 0 0 $9
  System::Call "gdi32::SelectObject(p r8, p r9)"
  StrLen ${out} "${text}"
  System::Call "*(i 0, i 0) p .r9"
  System::Call "gdi32::GetTextExtentPoint32W(p r8, w '${text}', i ${out}, p r9)"
  System::Call "*$9(i .s)"
  Pop ${out}
  System::Free $9
  System::Call "user32::ReleaseDC(p ${hwnd}, p r8)"
  Pop $9
  Pop $8
!macroend

; A plain child window of the current page. For the ones nobody clicks: a
; control nsDialogs did not create gets no nsDialogs callbacks, but it works
; on MUI2's instfiles dialog, where nsDialogs cannot create anything.
!macro DshStatic out style x y w h text
  System::Call 'user32::CreateWindowExW(i 0, w "STATIC", w "${text}", i ${style}, i ${x}, i ${y}, i ${w}, i ${h}, p $DshPage, p 0, p 0, p 0) p .s'
  Pop ${out}
!macroend

; Everything the pages share, done once for the whole run: DPI, the caption,
; the mark at the right scale, and the fonts. Fonts and the bitmap are kept
; for the life of the process rather than freed page by page.
Function DshSetup
  StrCmp $DshReady 1 done

  System::Call "user32::GetDpiForWindow(p $HWNDPARENT) i .s"
  Pop $DshDpi
  ${IfThen} $DshDpi = 0 ${|} StrCpy $DshDpi 96 ${|}

  ; Windows 11 draws the caption in the window's own white, with no title or
  ; icon in it, so it reads as part of the page instead of a bar across the
  ; top. Both calls fail quietly on Windows 10 and the stock caption stays.
  ; DWMWA_CAPTION_COLOR, as a COLORREF.
  System::Call "dwmapi::DwmSetWindowAttribute(p $HWNDPARENT, i 35, *i 0x00FFFFFF, i 4)"
  ; WTA_NONCLIENT: WTNCA_NODRAWCAPTION | WTNCA_NODRAWICON.
  System::Call "*(i 3, i 3) p .r0"
  System::Call "uxtheme::SetWindowThemeAttribute(p $HWNDPARENT, i 1, p r0, i 8)"
  System::Free $0

  ; The mark, drawn for the display scale nearest this window's DPI: a static
  ; control shows a bitmap at its own pixel size, and one scaled by
  ; `LoadImage` is visibly jagged.
  InitPluginsDir
  ${If} $DshDpi >= 180
    File "/oname=$PLUGINSDIR\dsh-brand.bmp" "${DSH_HOOKS_DIR}\installer\brand-200.bmp"
    StrCpy $DshBrandSize 240
  ${ElseIf} $DshDpi >= 156
    File "/oname=$PLUGINSDIR\dsh-brand.bmp" "${DSH_HOOKS_DIR}\installer\brand-175.bmp"
    StrCpy $DshBrandSize 210
  ${ElseIf} $DshDpi >= 132
    File "/oname=$PLUGINSDIR\dsh-brand.bmp" "${DSH_HOOKS_DIR}\installer\brand-150.bmp"
    StrCpy $DshBrandSize 180
  ${ElseIf} $DshDpi >= 108
    File "/oname=$PLUGINSDIR\dsh-brand.bmp" "${DSH_HOOKS_DIR}\installer\brand-125.bmp"
    StrCpy $DshBrandSize 150
  ${Else}
    File "/oname=$PLUGINSDIR\dsh-brand.bmp" "${DSH_HOOKS_DIR}\installer\brand-100.bmp"
    StrCpy $DshBrandSize 120
  ${EndIf}
  ; IMAGE_BITMAP, LR_LOADFROMFILE.
  System::Call 'user32::LoadImageW(p 0, w "$PLUGINSDIR\dsh-brand.bmp", i 0, i 0, i 0, i 0x10) p .s'
  Pop $DshBrand

  CreateFont $DshFontName "Segoe UI Semibold" 20
  CreateFont $DshFontBody "Microsoft YaHei UI" 10
  CreateFont $DshFontSmall "Segoe UI" 9

  StrCpy $DshReady 1
  done:
FunctionEnd

; Hide every visible control of the outer window except the current page.
Function DshHideSiblings
  StrCpy $0 0
  loop:
    FindWindow $0 "" "" $HWNDPARENT $0
    StrCmp $0 0 done
    StrCmp $0 $DshPage loop
    System::Call "user32::IsWindowVisible(p r0) i .r1"
    IntCmp $1 0 loop
    ShowWindow $0 ${SW_HIDE}
    Goto loop
  done:
FunctionEnd

; Hiding once, as a page is created, is not enough on an nsDialogs page: NSIS
; makes the Next button visible again itself when `nsDialogs::Show` reports
; the page ready. It stays behind the page until something repaints it -- the
; mouse passing over it -- and then sits on top of the page, clickable. So the
; siblings are hidden a second time from the page's own message loop, once
; that has happened.
Function DshRehide
  ${NSD_KillTimer} DshRehide
  Call DshHideSiblings
FunctionEnd

; `nsDialogs::Show`, for our nsDialogs pages, with the second hiding above.
Function DshShow
  ${NSD_CreateTimer} DshRehide 1
  nsDialogs::Show
FunctionEnd

; Spread `$DshPage` over the whole window and draw what every page has at the
; top: the mark and the name.
;
; Leaves $4 x $5 as the page's size and $6 as where the page's own part
; starts. The column is laid out for the tallest page, the welcome page, and
; centred: mark 120, gap 14, name 36, gap 40, then 104 for the page's own.
; Every page starts from the same top so the mark never moves between them.
Function DshLayout
  Call DshSetup
  Call DshHideSiblings

  System::Call "*(i, i, i, i) p .r0"
  System::Call "user32::GetClientRect(p $HWNDPARENT, p r0)"
  System::Call "*$0(i, i, i .r4, i .r5)"
  System::Free $0
  ; SWP_NOZORDER | SWP_NOACTIVATE.
  System::Call "user32::SetWindowPos(p $DshPage, p 0, i 0, i 0, i r4, i r5, i 0x14)"
  SetCtlColors $DshPage "" ${DSH_PAPER}

  !insertmacro DshPx $6 314
  IntOp $6 $5 - $6
  IntOp $6 $6 / 2
  !insertmacro DshPx $7 8
  IntOp $6 $6 - $7

  ; WS_CHILD | WS_VISIBLE | SS_BITMAP. STM_SETIMAGE, IMAGE_BITMAP.
  IntOp $7 $4 - $DshBrandSize
  IntOp $7 $7 / 2
  !insertmacro DshStatic $DshBrandCtl 0x5000000E $7 $6 $DshBrandSize $DshBrandSize ""
  SendMessage $DshBrandCtl 0x172 0 $DshBrand
  IntOp $6 $6 + $DshBrandSize
  !insertmacro DshPx $7 14
  IntOp $6 $6 + $7

  ; WS_CHILD | WS_VISIBLE | SS_CENTER.
  !insertmacro DshPx $7 36
  !insertmacro DshStatic $DshNameCtl 0x50000001 0 $6 $4 $7 "${PRODUCTNAME}"
  SendMessage $DshNameCtl ${WM_SETFONT} $DshFontName 1
  SetCtlColors $DshNameCtl ${DSH_INK} ${DSH_PAPER}
  IntOp $6 $6 + $7
  !insertmacro DshPx $7 40
  IntOp $6 $6 + $7
FunctionEnd

; A checkbox or radio button at $6, sized to its text so a pair of them can be
; centred as a row by `DshChoiceRow`. `create` is the nsDialogs macro.
; $2 = its width, $7 = its height.
!macro DshChoice out create text
  !insertmacro DshPx $7 22
  ${${create}} 0 $6 100 $7 "${text}"
  Pop ${out}
  SendMessage ${out} ${WM_SETFONT} $DshFontBody 1
  SetCtlColors ${out} ${DSH_INK} ${DSH_PAPER}
  !insertmacro DshTextWidth $2 ${out} "${text}"
  ; The box and the gap before its text.
  !insertmacro DshPx $3 24
  IntOp $2 $2 + $3
!macroend

; Centre two choices side by side at $6, `a` of width `wa` and `b` of `wb`,
; and move $6 past them.
!macro DshChoiceRow a wa b wb
  !insertmacro DshPx $9 32
  IntOp $8 ${wa} + ${wb}
  IntOp $8 $8 + $9
  IntOp $8 $4 - $8
  IntOp $8 $8 / 2
  System::Call "user32::MoveWindow(p ${a}, i r8, i r6, i ${wa}, i r7, i 1)"
  IntOp $8 $8 + ${wa}
  IntOp $8 $8 + $9
  System::Call "user32::MoveWindow(p ${b}, i r8, i r6, i ${wb}, i r7, i 1)"
  IntOp $6 $6 + $7
!macroend

; The page's one button, centred at $6: a label in the ink colour, rounded by
; a window region. A real push button cannot be filled with a colour without
; drawing it ourselves.
!macro DshButton text callback
  !insertmacro DshPx $2 160
  !insertmacro DshPx $7 44
  IntOp $3 $4 - $2
  IntOp $3 $3 / 2
  ${NSD_CreateLabel} $3 $6 $2 $7 "${text}"
  Pop $0
  ${NSD_AddStyle} $0 ${SS_CENTER}|${SS_CENTERIMAGE}
  CreateFont $1 "Microsoft YaHei UI" 11
  SendMessage $0 ${WM_SETFONT} $1 1
  SetCtlColors $0 ${DSH_PAPER} ${DSH_INK}
  !insertmacro DshPx $1 16
  IntOp $2 $2 + 1
  IntOp $7 $7 + 1
  System::Call "gdi32::CreateRoundRectRgn(i 0, i 0, i r2, i r7, i r1, i r1) p .r1"
  System::Call "user32::SetWindowRgn(p r0, p r1, i 1)"
  ${NSD_OnClick} $0 ${callback}
!macroend

; --------------------------------------------------------------- welcome --

; $0 = where the user wants it, with the product's own folder on the end
; unless they picked that folder itself -- which is what MUI2's directory page
; did, and what keeps "D:\" from becoming an install straight into a drive root.
Function DshWelcomePickDir
  Pop $0
  nsDialogs::SelectFolderDialog "选择安装位置" "$INSTDIR"
  Pop $0
  StrCmp $0 "error" done
  StrCpy $1 $0 1 -1
  StrCmp $1 "\" 0 +2
    StrCpy $0 $0 -1
  ${GetFileName} "$0" $1
  StrCmp $1 "${PRODUCTNAME}" +2
    StrCpy $0 "$0\${PRODUCTNAME}"
  StrCpy $INSTDIR $0
  ${NSD_SetText} $DshDir $INSTDIR
  done:
FunctionEnd

Function DshNext
  Pop $0
  SendMessage $HWNDPARENT ${DSH_WM_NOTIFY_OUTER_NEXT} 1 0
FunctionEnd

Function DshWelcomeCreate
  ${IfThen} $PassiveMode = 1 ${|} Abort ${|}

  nsDialogs::Create 1018
  Pop $DshPage
  ${IfThen} $DshPage == error ${|} Abort ${|}
  Call DshLayout

  ; Where it goes. A link, so it shows the hand, sized to its own text because
  ; nsDialogs draws a link's text from the left of its rectangle.
  !insertmacro DshPx $7 22
  ${NSD_CreateLink} 0 $6 $4 $7 "选择安装位置"
  Pop $0
  SendMessage $0 ${WM_SETFONT} $DshFontBody 1
  SetCtlColors $0 ${DSH_INK} ${DSH_PAPER}
  !insertmacro DshTextWidth $2 $0 "选择安装位置"
  IntOp $2 $2 + 4
  IntOp $3 $4 - $2
  IntOp $3 $3 / 2
  System::Call "user32::MoveWindow(p r0, i r3, i r6, i r2, i r7, i 1)"
  ${NSD_OnClick} $0 DshWelcomePickDir
  IntOp $6 $6 + $7

  !insertmacro DshPx $7 20
  ${NSD_CreateLabel} 0 $6 $4 $7 "$INSTDIR"
  Pop $DshDir
  ${NSD_AddStyle} $DshDir ${SS_CENTER}|${SS_PATHELLIPSIS}
  SendMessage $DshDir ${WM_SETFONT} $DshFontSmall 1
  SetCtlColors $DshDir ${DSH_MUTED} ${DSH_PAPER}
  IntOp $6 $6 + $7
  !insertmacro DshPx $7 18
  IntOp $6 $6 + $7

  !insertmacro DshButton "立即安装" DshNext

  Call DshShow
FunctionEnd

; ------------------------------------------------------------- reinstall --

; The template's "already installed" page, shown when an earlier install is
; found. Only the drawing is ours. `PageReinstall` has already worked out
; what the two choices are -- their labels in $R2 and $R3, and in $R0 whether
; this is the same version (0), an upgrade (1) or a downgrade (-1) -- and it
; goes on to wire them up and show the page after this returns, so its leave
; function reads them exactly as it did.
;
; Leaves $R2 and $R3 as the two radio buttons, which is what `PageReinstall`
; expects of the stock layout this replaces. Touches no other $R register:
; `EarlyChecks` reads $R0 long after this page has gone.
;
; The message is ours rather than the template's `$R1`. Every one of those
; ends "click Next to continue" and runs to three lines, and this page has
; neither a Next button nor the room.
Function DshReinstallCreate
  nsDialogs::Create 1018
  Pop $DshPage
  Call DshLayout

  ${If} $R0 = 0
    StrCpy $0 "${PRODUCTNAME} ${VERSION} 已经安装"
  ${ElseIf} $R0 = 1
    StrCpy $0 "已安装旧版本，建议先卸载再安装"
  ${Else}
    StrCpy $0 "已安装更新的版本，不建议安装旧版本"
  ${EndIf}

  ; Closer under the name than the other pages' parts sit: this page has one
  ; row more than they do and the same room.
  !insertmacro DshPx $7 16
  IntOp $6 $6 - $7
  !insertmacro DshPx $7 22
  ${NSD_CreateLabel} 0 $6 $4 $7 $0
  Pop $0
  ${NSD_AddStyle} $0 ${SS_CENTER}
  SendMessage $0 ${WM_SETFONT} $DshFontBody 1
  SetCtlColors $0 ${DSH_MUTED} ${DSH_PAPER}
  IntOp $6 $6 + $7
  !insertmacro DshPx $7 14
  IntOp $6 $6 + $7

  !insertmacro DshChoice $0 NSD_CreateRadioButton $R2
  StrCpy $1 $2
  StrCpy $R2 $0
  !insertmacro DshChoice $0 NSD_CreateRadioButton $R3
  StrCpy $R3 $0
  !insertmacro DshChoiceRow $R2 $1 $R3 $2
  !insertmacro DshPx $7 22
  IntOp $6 $6 + $7

  !insertmacro DshButton "继续" DshNext
FunctionEnd

; -------------------------------------------------------------- progress --

; MUI2's instfiles page, as a MUI_PAGE_CUSTOMFUNCTION_SHOW. Its controls are
; NSIS's own and keep working as they do -- the bar still fills, the status
; line still follows `DetailPrint` -- they are only moved and restyled.
;
; The log goes, with the button that opens it. A section that aborts leaves
; its reason on the status line, and the caption's close button is the way
; out. NSIS offers no callback at the moment of failure to bring the log back
; in: `.onInstFailed` only runs once the user has already closed the window.
Function DshProgressShow
  FindWindow $DshPage "#32770" "" $HWNDPARENT
  Call DshLayout

  ; "Show details" and the log it opens.
  GetDlgItem $0 $DshPage 1027
  ShowWindow $0 ${SW_HIDE}
  GetDlgItem $0 $DshPage 1016
  ShowWindow $0 ${SW_HIDE}

  ; $1 margin, $2 width, $3 top, $7 height of the bar.
  !insertmacro DshPx $1 72
  IntOp $2 $1 * 2
  IntOp $2 $4 - $2
  !insertmacro DshPx $3 30
  IntOp $3 $6 + $3
  !insertmacro DshPx $7 6

  ; The bar, flat. Its colours only apply without a visual style, and the
  ; sunken edge goes with it; SWP_FRAMECHANGED makes the new frame count.
  GetDlgItem $0 $DshPage 1004
  System::Call 'uxtheme::SetWindowTheme(p r0, w " ", w " ")'
  System::Call "user32::SetWindowLongW(p r0, i -20, i 0)"
  System::Call "user32::GetWindowLongW(p r0, i -16) i .r8"
  IntOp $9 0x00800000 ~
  IntOp $8 $8 & $9
  System::Call "user32::SetWindowLongW(p r0, i -16, i r8)"
  SendMessage $0 0x409 0 ${DSH_INK_REF}
  SendMessage $0 0x2001 0 ${DSH_TRACK_REF}
  System::Call "user32::SetWindowPos(p r0, p 0, i r1, i r3, i r2, i r7, i 0x34)"
  IntOp $8 $2 + 1
  IntOp $9 $7 + 1
  System::Call "gdi32::CreateRoundRectRgn(i 0, i 0, i r8, i r9, i r7, i r7) p .r8"
  System::Call "user32::SetWindowRgn(p r0, p r8, i 1)"

  ; The status line under it, centred, with a long path cut at the end.
  IntOp $3 $3 + $7
  !insertmacro DshPx $7 18
  IntOp $3 $3 + $7
  !insertmacro DshPx $7 22
  GetDlgItem $0 $DshPage 1006
  System::Call "user32::GetWindowLongW(p r0, i -16) i .r8"
  IntOp $9 0x1F ~
  IntOp $8 $8 & $9
  ; SS_CENTER | SS_ENDELLIPSIS.
  IntOp $8 $8 | 0x4001
  System::Call "user32::SetWindowLongW(p r0, i -16, i r8)"
  SendMessage $0 ${WM_SETFONT} $DshFontBody 1
  SetCtlColors $0 ${DSH_INK} ${DSH_PAPER}
  System::Call "user32::SetWindowPos(p r0, p 0, i r1, i r3, i r2, i r7, i 0x34)"
FunctionEnd

; ---------------------------------------------------------------- finish --

; Create the desktop shortcut and start the app, each if its box is ticked,
; and close. The shortcut is the finish page's job in the stock template too:
; MUI2's "show readme" checkbox, repurposed.
Function DshFinishDone
  Pop $0
  ${NSD_GetState} $DshShortcut $0
  ${If} $0 == ${BST_CHECKED}
    Call CreateOrUpdateDesktopShortcut
  ${EndIf}
  ${NSD_GetState} $DshRun $0
  ${If} $0 == ${BST_CHECKED}
    Call RunMainBinary
  ${EndIf}
  SendMessage $HWNDPARENT ${DSH_WM_NOTIFY_OUTER_NEXT} 1 0
FunctionEnd


Function DshFinishCreate
  ${IfThen} $PassiveMode = 1 ${|} Abort ${|}

  nsDialogs::Create 1018
  Pop $DshPage
  ${IfThen} $DshPage == error ${|} Abort ${|}
  Call DshLayout

  !insertmacro DshChoice $DshShortcut NSD_CreateCheckbox "创建桌面快捷方式"
  StrCpy $0 $2
  !insertmacro DshChoice $DshRun NSD_CreateCheckbox "立即启动"
  StrCpy $1 $2
  ${NSD_Check} $DshShortcut
  ${NSD_Check} $DshRun
  !insertmacro DshChoiceRow $DshShortcut $0 $DshRun $1
  !insertmacro DshPx $7 38
  IntOp $6 $6 + $7

  !insertmacro DshButton "完成" DshFinishDone

  Call DshShow
FunctionEnd
