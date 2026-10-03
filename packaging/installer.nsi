; ZipNest 0.3.4 native installer (NSIS Unicode)
Unicode True
!include "MUI2.nsh"

!define APPNAME "ZipNest"
!define VERSION "0.3.4"
!define INSTDIR "$PROGRAMFILES\ZipNest"
!define UNINSTKEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\ZipNest"

Name "${APPNAME}"
OutFile "ZipNest_0.3.4_x64-setup.exe"
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

; Force-close a running ZipNest so its files are not locked while we overwrite
; them (otherwise the user sees "cannot write file" and has to close it first).
Function CloseRunningZipNest
  StrCpy $1 0
  zn_kill_again:
    nsExec::ExecToLog '"$SYSDIR\taskkill.exe" /F /IM zipnest.exe'
    Pop $0
    IntCmp $0 0 zn_kill_wait zn_kill_done zn_kill_done
  zn_kill_wait:
    Sleep 400
    IntOp $1 $1 + 1
    IntCmp $1 12 zn_kill_done zn_kill_again zn_kill_done
  zn_kill_done:
FunctionEnd

; Same as above, for the uninstall section (which may only call un.* functions).
Function un.CloseRunningZipNest
  StrCpy $1 0
  zn_ukill_again:
    nsExec::ExecToLog '"$SYSDIR\taskkill.exe" /F /IM zipnest.exe'
    Pop $0
    IntCmp $0 0 zn_ukill_wait zn_ukill_done zn_ukill_done
  zn_ukill_wait:
    Sleep 400
    IntOp $1 $1 + 1
    IntCmp $1 12 zn_ukill_done zn_ukill_again zn_ukill_done
  zn_ukill_done:
FunctionEnd

Section "Install"
  ; Close a running ZipNest first, or its exe/dll are locked and writes fail.
  Call CloseRunningZipNest
  SetOutPath "$INSTDIR"
  File "..\dist-portable\ZipNest\zipnest.exe"
  SetOutPath "$INSTDIR\engines"
  File "..\dist-portable\ZipNest\engines\7z.dll"
  SetOutPath "$INSTDIR\engines\sfx"
  File "..\dist-portable\ZipNest\engines\sfx\7z.sfx"
  File "..\dist-portable\ZipNest\engines\sfx\7zCon.sfx"
  SetOutPath "$INSTDIR\licenses"
  File "..\dist-portable\ZipNest\licenses\7zip.txt"

  ; Windows 11 modern context menu: per-user signed sparse package.
  ; zipnest_shell.dll stays in the install dir (the package's external location);
  ; the signed Win11Shell\ZipNestShell.msix carries the manifest + assets.
  SetOutPath "$INSTDIR"
  File "..\dist-portable\ZipNest\zipnest_shell.dll"
  SetOutPath "$INSTDIR\Win11Shell"
  File "..\dist-portable\ZipNest\Win11Shell\ZipNestShell.msix"
  File "..\dist-portable\ZipNest\Win11Shell\ZipNestCodesign.cer"
  File "..\dist-portable\ZipNest\Win11Shell\install.ps1"
  File "..\dist-portable\ZipNest\Win11Shell\uninstall.ps1"

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
  CreateShortCut "$SMPROGRAMS\ZipNest\ZipNest.lnk" "$INSTDIR\zipnest.exe" "" "$INSTDIR\zipnest.exe" 0
  CreateShortCut "$SMPROGRAMS\ZipNest\卸载 ZipNest.lnk" "$INSTDIR\uninstall.exe" "" "$INSTDIR\uninstall.exe" 0
  ; Desktop shortcut
  CreateShortCut "$DESKTOP\ZipNest.lnk" "$INSTDIR\zipnest.exe" "" "$INSTDIR\zipnest.exe" 0
  ; Refresh the shell icon cache so the new shortcut icon shows immediately.
  System::Call "shell32::SHChangeNotify(i 0x08000000, i 0, i 0, i 0)"

  ; Register the Windows 11 context menu (per-user; ignored on Windows 10).
  Call RegisterWin11Shell
SectionEnd

; Run the packaged registration only on Windows 11 (build >= 22000). On older
; Windows the manifest is harmless but the modern menu does not exist, so skip.
Function RegisterWin11Shell
  ReadRegStr $0 HKLM "SOFTWARE\Microsoft\Windows NT\CurrentVersion" "CurrentBuildNumber"
  IntCmpU $0 22000 do_register skip_register do_register
do_register:
  nsExec::ExecToLog 'powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$INSTDIR\Win11Shell\install.ps1" -InstallDir "$INSTDIR" -PackageDir "$INSTDIR\Win11Shell"'
  Pop $0
skip_register:
FunctionEnd

; Remove the per-user package before its files are deleted.
Function un.UnregisterWin11Shell
  ReadRegStr $0 HKLM "SOFTWARE\Microsoft\Windows NT\CurrentVersion" "CurrentBuildNumber"
  IntCmpU $0 22000 do_unregister skip_unregister do_unregister
do_unregister:
  nsExec::ExecToLog 'powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$INSTDIR\Win11Shell\uninstall.ps1" -PackageDir "$INSTDIR\Win11Shell"'
  Pop $0
skip_unregister:
FunctionEnd

Section "Uninstall"
  ; Close a running ZipNest so its exe/dll can be deleted.
  Call un.CloseRunningZipNest
  ; Unregister the per-user package before deleting its manifest/files.
  Call un.UnregisterWin11Shell

  Delete "$INSTDIR\zipnest.exe"
  Delete "$INSTDIR\engines\7z.dll"
  Delete "$INSTDIR\engines\sfx\7z.sfx"
  Delete "$INSTDIR\engines\sfx\7zCon.sfx"
  Delete "$INSTDIR\licenses\7zip.txt"
  Delete "$INSTDIR\zipnest_shell.dll"
  Delete "$INSTDIR\Win11Shell\ZipNestShell.msix"
  Delete "$INSTDIR\Win11Shell\ZipNestCodesign.cer"
  Delete "$INSTDIR\Win11Shell\install.ps1"
  Delete "$INSTDIR\Win11Shell\uninstall.ps1"
  Delete "$INSTDIR\uninstall.exe"
  ; /r also clears the AppX registration metadata subfolder.
  RMDir /r "$INSTDIR\Win11Shell"
  RMDir "$INSTDIR\engines\sfx"
  RMDir "$INSTDIR\engines"
  RMDir "$INSTDIR\licenses"
  RMDir "$INSTDIR"
  Delete "$SMPROGRAMS\ZipNest\ZipNest.lnk"
  Delete "$SMPROGRAMS\ZipNest\卸载 ZipNest.lnk"
  RMDir "$SMPROGRAMS\ZipNest"
  Delete "$DESKTOP\ZipNest.lnk"
  DeleteRegKey HKCU "${UNINSTKEY}"
SectionEnd











