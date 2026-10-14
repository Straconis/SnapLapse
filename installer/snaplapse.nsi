; SnapLapse Windows installer, built with NSIS (makensis) by build.sh.
; Installs SnapLapse.exe and Uninstall.exe side by side in Program Files\SnapLapse, adds a
; SnapLapse folder to the Start menu with shortcuts to both (and, optionally, a desktop shortcut),
; and registers SnapLapse in Windows' Installed apps list.
; Screenshots and the user's settings are never touched by the uninstaller.

Unicode true
!include "MUI2.nsh"
!include "x64.nsh"
!include "FileFunc.nsh"

!define APP "SnapLapse"
!ifndef VERSION
  !define VERSION "0.0.0"
!endif
!define PUBLISHER "Straconis"
!define URL "https://github.com/Straconis/SnapLapse"
!define UNINST_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP}"

Name "${APP}"
OutFile "..\dist\SnapLapse-${VERSION}-setup.exe"
InstallDir "$PROGRAMFILES64\${APP}"
InstallDirRegKey HKLM "Software\${APP}" "InstallDir"
RequestExecutionLevel admin
SetCompressor /SOLID lzma
BrandingText "${APP} ${VERSION}"

VIProductVersion "${VERSION}.0"
VIAddVersionKey "ProductName" "${APP}"
VIAddVersionKey "CompanyName" "${PUBLISHER}"
VIAddVersionKey "FileDescription" "${APP} Setup"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "LegalCopyright" "Copyright (c) 2026 ${PUBLISHER}. MIT License."

!define MUI_ICON "..\res\snaplapse.ico"
!define MUI_UNICON "..\res\snaplapse.ico"
!define MUI_ABORTWARNING
!define MUI_COMPONENTSPAGE_NODESC
!define MUI_FINISHPAGE_RUN "$INSTDIR\SnapLapse.exe"
!define MUI_FINISHPAGE_RUN_TEXT "Open SnapLapse"

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_LICENSE "..\LICENSE"
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

Function .onInit
  ${IfNot} ${RunningX64}
    MessageBox MB_ICONSTOP "SnapLapse needs 64-bit Windows 10 or 11."
    Abort
  ${EndIf}
  SetRegView 64
  SetShellVarContext all
FunctionEnd

Function un.onInit
  SetRegView 64
  SetShellVarContext all
FunctionEnd

Section "SnapLapse" SecApp
  SectionIn RO
  SetOutPath "$INSTDIR"
  File "..\dist\SnapLapse.exe"
  WriteUninstaller "$INSTDIR\Uninstall.exe"
  ; Version 0.5.0 put a loose shortcut in the Start menu. Replace it with a folder.
  Delete "$SMPROGRAMS\${APP}.lnk"
  CreateDirectory "$SMPROGRAMS\${APP}"
  CreateShortcut "$SMPROGRAMS\${APP}\${APP}.lnk" "$INSTDIR\SnapLapse.exe"
  CreateShortcut "$SMPROGRAMS\${APP}\Uninstall ${APP}.lnk" "$INSTDIR\Uninstall.exe"

  WriteRegStr HKLM "Software\${APP}" "InstallDir" "$INSTDIR"
  WriteRegStr HKLM "${UNINST_KEY}" "DisplayName" "${APP}"
  WriteRegStr HKLM "${UNINST_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKLM "${UNINST_KEY}" "Publisher" "${PUBLISHER}"
  WriteRegStr HKLM "${UNINST_KEY}" "URLInfoAbout" "${URL}"
  WriteRegStr HKLM "${UNINST_KEY}" "DisplayIcon" "$INSTDIR\SnapLapse.exe"
  WriteRegStr HKLM "${UNINST_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKLM "${UNINST_KEY}" "UninstallString" '"$INSTDIR\Uninstall.exe"'
  WriteRegStr HKLM "${UNINST_KEY}" "QuietUninstallString" '"$INSTDIR\Uninstall.exe" /S'
  WriteRegDWORD HKLM "${UNINST_KEY}" "NoModify" 1
  WriteRegDWORD HKLM "${UNINST_KEY}" "NoRepair" 1
  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  WriteRegDWORD HKLM "${UNINST_KEY}" "EstimatedSize" $0
SectionEnd

Section "Desktop shortcut" SecDesktop
  CreateShortcut "$DESKTOP\${APP}.lnk" "$INSTDIR\SnapLapse.exe"
SectionEnd

Section "Uninstall"
  Delete "$INSTDIR\SnapLapse.exe"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir "$INSTDIR"
  Delete "$SMPROGRAMS\${APP}\${APP}.lnk"
  Delete "$SMPROGRAMS\${APP}\Uninstall ${APP}.lnk"
  RMDir "$SMPROGRAMS\${APP}"
  Delete "$SMPROGRAMS\${APP}.lnk"
  Delete "$DESKTOP\${APP}.lnk"
  DeleteRegKey HKLM "${UNINST_KEY}"
  DeleteRegKey HKLM "Software\${APP}"
SectionEnd
