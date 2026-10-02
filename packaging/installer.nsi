; ZipNest 0.2.0 native installer (NSIS Unicode)
Unicode True
!include "MUI2.nsh"

!define APPNAME "ZipNest"
!define VERSION "0.2.0"
!define INSTDIR "$PROGRAMFILES\ZipNest"
!define UNINSTKEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\ZipNest"

Name "${APPNAME}"
OutFile "ZipNest_0.2.0_x64-setup.exe"
InstallDir "${INSTDIR}"
InstallDirRegKey HKCU "${UNINSTKEY}" "InstallLocation"
RequestExecutionLevel admin
SetCompressor /SOLID lzma

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "SimpChinese"

Section "Install"
  SetOutPath "$INSTDIR"
  File "..\dist-portable\ZipNest\zipnest.exe"
  SetOutPath "$INSTDIR\engines"
  File "..\dist-portable\ZipNest\engines\7z.dll"
  SetOutPath "$INSTDIR\engines\sfx"
  File "..\dist-portable\ZipNest\engines\sfx\7z.sfx"
  File "..\dist-portable\ZipNest\engines\sfx\7zCon.sfx"
  SetOutPath "$INSTDIR\licenses"
  File "..\dist-portable\ZipNest\licenses\7zip.txt"

  ; uninstaller
  WriteUninstaller "$INSTDIR\uninstall.exe"
  WriteRegStr HKCU "${UNINSTKEY}" "DisplayName" "ZipNest"
  WriteRegStr HKCU "${UNINSTKEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINSTKEY}" "Publisher" "ZipNest contributors"
  WriteRegStr HKCU "${UNINSTKEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINSTKEY}" "UninstallString" "$\"$INSTDIR\uninstall.exe$\""
  WriteRegDWORD HKCU "${UNINSTKEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINSTKEY}" "NoRepair" 1

  ; Start menu shortcut
  CreateDirectory "$SMPROGRAMS\ZipNest"
  CreateShortCut "$SMPROGRAMS\ZipNest\ZipNest.lnk" "$INSTDIR\zipnest.exe"
  CreateShortCut "$SMPROGRAMS\ZipNest\卸载 ZipNest.lnk" "$INSTDIR\uninstall.exe"
SectionEnd

Section "Uninstall"
  Delete "$INSTDIR\zipnest.exe"
  Delete "$INSTDIR\engines\7z.dll"
  Delete "$INSTDIR\engines\sfx\7z.sfx"
  Delete "$INSTDIR\engines\sfx\7zCon.sfx"
  Delete "$INSTDIR\licenses\7zip.txt"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR\engines\sfx"
  RMDir "$INSTDIR\engines"
  RMDir "$INSTDIR\licenses"
  RMDir "$INSTDIR"
  Delete "$SMPROGRAMS\ZipNest\ZipNest.lnk"
  Delete "$SMPROGRAMS\ZipNest\卸载 ZipNest.lnk"
  RMDir "$SMPROGRAMS\ZipNest"
  DeleteRegKey HKCU "${UNINSTKEY}"
SectionEnd


