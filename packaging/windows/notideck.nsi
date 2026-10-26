; SPDX-License-Identifier: GPL-3.0-or-later
; Notideck Windows installer (NSIS 3, MUI2, per-machine, x64).
; Built by scripts/build-windows.sh, which passes:
;   -DVERSION=1.0.0            product version
;   -DSTAGE_DIR=<dir>          staged folder (Notideck.exe, plugins/, ...)
;   -DOUT_FILE=<path>          installer output path
;   -DICON_FILE=<path>         notideck.ico
;   -DLICENSE_FILE=<path>      LICENSE

!ifndef VERSION
  !error "pass -DVERSION=x.y.z"
!endif
!ifndef STAGE_DIR
  !error "pass -DSTAGE_DIR=<staged folder>"
!endif
!ifndef OUT_FILE
  !define OUT_FILE "Notideck-${VERSION}-windows-x64-setup.exe"
!endif

Unicode true
SetCompressor /SOLID lzma
RequestExecutionLevel admin
ManifestSupportedOS all

!define APPNAME "Notideck"
!define UNINST_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\Notideck"

Name "${APPNAME} ${VERSION}"
OutFile "${OUT_FILE}"
InstallDir "$PROGRAMFILES64\Notideck"
InstallDirRegKey HKLM "Software\Notideck" "InstallDir"
BrandingText "Notideck ${VERSION}"

VIProductVersion "${VERSION}.0"
VIAddVersionKey "ProductName" "Notideck"
VIAddVersionKey "FileDescription" "Notideck setup"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "LegalCopyright" "GPL-3.0-or-later"

!include "MUI2.nsh"
!include "x64.nsh"
!include "LogicLib.nsh"
!include "FileFunc.nsh"
!include "nsDialogs.nsh"

!ifdef ICON_FILE
  !define MUI_ICON "${ICON_FILE}"
  !define MUI_UNICON "${ICON_FILE}"
!endif
!define MUI_ABORTWARNING

; --- install pages ---
!insertmacro MUI_PAGE_WELCOME
!ifdef LICENSE_FILE
  !insertmacro MUI_PAGE_LICENSE "${LICENSE_FILE}"
!endif
!insertmacro MUI_PAGE_COMPONENTS
!define MUI_PAGE_CUSTOMFUNCTION_LEAVE DirLeave
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!define MUI_FINISHPAGE_RUN
!define MUI_FINISHPAGE_RUN_TEXT "Run Notideck"
!define MUI_FINISHPAGE_RUN_FUNCTION LaunchUnelevated
!insertmacro MUI_PAGE_FINISH

; --- uninstall pages ---
!insertmacro MUI_UNPAGE_CONFIRM
UninstPage custom un.OptionsShow un.OptionsLeave
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

Var RemoveSettings
Var RemoveCheck

; --------------------------------------------------------------------------
Function .onInit
  ${IfNot} ${RunningX64}
    MessageBox MB_OK|MB_ICONSTOP "Notideck requires 64-bit Windows."
    Abort
  ${EndIf}
  SetRegView 64
  ; Upgrade: reuse the previous install dir (InstallDirRegKey does it) under 64-bit view.
  ReadRegStr $0 HKLM "Software\Notideck" "InstallDir"
  ${If} $0 != ""
    StrCpy $INSTDIR $0
  ${EndIf}
FunctionEnd

Function un.onInit
  SetRegView 64
FunctionEnd

; The uninstaller deletes the install dir recursively, so it must be a folder of its own:
; a chosen path that doesn't end in "\Notideck" gets "\Notideck" appended.
Function DirLeave
  StrCpy $0 $INSTDIR 9 -9
  ${If} $0 != "\Notideck"
    StrCpy $INSTDIR "$INSTDIR\Notideck"
  ${EndIf}
FunctionEnd

; Ask a running instance to exit (waits up to 10 s itself), then force-kill leftovers.
!macro StopNotideck EXEDIR
  ${If} ${FileExists} "${EXEDIR}\Notideck.exe"
    nsExec::Exec '"${EXEDIR}\Notideck.exe" --quit'
    Pop $0
  ${EndIf}
  nsExec::Exec '"$SYSDIR\taskkill.exe" /IM Notideck.exe /F'
  Pop $0
  ; The adb server Notideck started outlives it and locks adb.exe / AdbWin*.dll in the install dir.
  ${If} ${FileExists} "${EXEDIR}\adb.exe"
    nsExec::Exec '"${EXEDIR}\adb.exe" kill-server'
    Pop $0
  ${EndIf}
  Sleep 500
!macroend

Function LaunchUnelevated
  ; explorer.exe runs the target with the desktop user's (unelevated) token.
  Exec '"$WINDIR\explorer.exe" "$INSTDIR\Notideck.exe"'
FunctionEnd

; --------------------------------------------------------------------------
Section "Notideck (required)" SecMain
  SectionIn RO
  SetRegView 64
  SetOutPath "$INSTDIR"

  DetailPrint "Stopping any running Notideck..."
  !insertmacro StopNotideck "$INSTDIR"

  DetailPrint "Installing files..."
  File /r "${STAGE_DIR}\*.*"

  ; Registry, uninstaller
  WriteRegStr HKLM "Software\Notideck" "InstallDir" "$INSTDIR"
  WriteUninstaller "$INSTDIR\Uninstall.exe"

  WriteRegStr HKLM "${UNINST_KEY}" "DisplayName" "Notideck"
  WriteRegStr HKLM "${UNINST_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKLM "${UNINST_KEY}" "Publisher" "Notideck"
  WriteRegStr HKLM "${UNINST_KEY}" "DisplayIcon" "$INSTDIR\Notideck.exe,0"
  WriteRegStr HKLM "${UNINST_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKLM "${UNINST_KEY}" "UninstallString" '"$INSTDIR\Uninstall.exe"'
  WriteRegStr HKLM "${UNINST_KEY}" "QuietUninstallString" '"$INSTDIR\Uninstall.exe" /S'
  WriteRegDWORD HKLM "${UNINST_KEY}" "NoModify" 1
  WriteRegDWORD HKLM "${UNINST_KEY}" "NoRepair" 1
  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  IntFmt $0 "0x%08X" $0
  WriteRegDWORD HKLM "${UNINST_KEY}" "EstimatedSize" $0

  ; Start menu (all users)
  SetShellVarContext all
  CreateShortcut "$SMPROGRAMS\Notideck.lnk" "$INSTDIR\Notideck.exe" "" "$INSTDIR\Notideck.exe" 0

  ; Virtual camera (Windows 11 = build 22000+); failure is only a warning.
  ReadRegStr $3 HKLM "SOFTWARE\Microsoft\Windows NT\CurrentVersion" "CurrentBuildNumber"
  ${If} $3 == ""
    StrCpy $3 0
  ${EndIf}
  ${If} $3 >= 22000
    ${If} ${FileExists} "$INSTDIR\notideck_vcam.dll"
      DetailPrint "Registering the Notideck Camera..."
      nsExec::ExecToStack '"$INSTDIR\Notideck.exe" --install-vcam'
      Pop $0
      Pop $1
      ${If} $0 != 0
        DetailPrint "Camera registration failed (exit $0): $1"
        StrCpy $2 "Notideck Camera could not be registered (exit code $0)."
        ${If} $1 != ""
          StrCpy $2 "$2$\r$\n$\r$\n$1"
        ${EndIf}
        StrCpy $2 "$2$\r$\n$\r$\nNotideck still works. Retry from Notideck > Camera > Install webcam. See $LOCALAPPDATA\Notideck for logs."
        MessageBox MB_OK|MB_ICONEXCLAMATION "$2" /SD IDOK
      ${EndIf}
    ${EndIf}
  ${EndIf}
SectionEnd

Section "Desktop shortcut" SecDesktop
  SetShellVarContext all
  CreateShortcut "$DESKTOP\Notideck.lnk" "$INSTDIR\Notideck.exe" "" "$INSTDIR\Notideck.exe" 0
SectionEnd

!insertmacro MUI_FUNCTION_DESCRIPTION_BEGIN
  !insertmacro MUI_DESCRIPTION_TEXT ${SecMain} "Notideck, the bundled plugins, adb and the phone app (notideck-phone.apk)."
  !insertmacro MUI_DESCRIPTION_TEXT ${SecDesktop} "Create a shortcut on the desktop."
!insertmacro MUI_FUNCTION_DESCRIPTION_END

; --------------------------------------------------------------------------
; Uninstaller
Function un.OptionsShow
  !insertmacro MUI_HEADER_TEXT "Uninstall options" "Choose what to keep."
  nsDialogs::Create 1018
  Pop $0
  ${NSD_CreateCheckbox} 0 10u 100% 12u "Also remove my profiles, settings, paired phones and logs"
  Pop $RemoveCheck
  nsDialogs::Show
FunctionEnd

Function un.OptionsLeave
  ${NSD_GetState} $RemoveCheck $RemoveSettings
FunctionEnd

Section "Uninstall"
  SetRegView 64
  DetailPrint "Stopping Notideck..."
  !insertmacro StopNotideck "$INSTDIR"

  ${If} ${FileExists} "$INSTDIR\Notideck.exe"
    DetailPrint "Removing the Notideck Camera..."
    nsExec::ExecToStack '"$INSTDIR\Notideck.exe" --uninstall-vcam'
    Pop $0
    Pop $1
    ${If} $0 != 0
      DetailPrint "Camera removal reported exit $0 (ignored)."
    ${EndIf}
  ${EndIf}

  SetShellVarContext all
  Delete "$SMPROGRAMS\Notideck.lnk"
  Delete "$DESKTOP\Notideck.lnk"

  ; Autostart value of the user running the uninstaller (best effort).
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "Notideck"

  DeleteRegKey HKLM "${UNINST_KEY}"
  DeleteRegKey HKLM "Software\Notideck"

  ; Only wipe a dir that is Notideck's own (setup forces the "\Notideck" suffix); otherwise
  ; remove just what setup put there.
  Delete "$INSTDIR\Uninstall.exe"
  StrCpy $0 $INSTDIR 9 -9
  ${If} $0 == "\Notideck"
    RMDir /r "$INSTDIR"
  ${Else}
    Delete "$INSTDIR\Notideck.exe"
    Delete "$INSTDIR\notideck_vcam.dll"
    Delete "$INSTDIR\WebView2Loader.dll"
    Delete "$INSTDIR\adb.exe"
    Delete "$INSTDIR\AdbWinApi.dll"
    Delete "$INSTDIR\AdbWinUsbApi.dll"
    Delete "$INSTDIR\adb-NOTICE.txt"
    Delete "$INSTDIR\notideck-phone.apk"
    Delete "$INSTDIR\LICENSE"
    Delete "$INSTDIR\README.txt"
    RMDir /r "$INSTDIR\plugins"
    RMDir /r "$INSTDIR\VirtualCamera"
    RMDir "$INSTDIR"
  ${EndIf}

  ${If} $RemoveSettings == ${BST_CHECKED}
    SetShellVarContext current
    RMDir /r "$APPDATA\Notideck"
    RMDir /r "$LOCALAPPDATA\Notideck"
  ${EndIf}
SectionEnd
