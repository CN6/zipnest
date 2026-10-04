; ZipNest 0.3.6 native installer (NSIS Unicode)
Unicode True
!include "MUI2.nsh"

!define APPNAME "ZipNest"
!define VERSION "0.3.6"
; This installer is a 32-bit process, so plain $PROGRAMFILES would resolve to
; "C:\Program Files (x86)" on 64-bit Windows. ZipNest ships as x64, so force
; the 64-bit location. InstallDirRegKey below still reads HKCU\...\ZipNest's
; InstallLocation, which is written from this same (now 64-bit) path.
!define INSTDIR "$PROGRAMFILES64\ZipNest"
!define UNINSTKEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\ZipNest"

; User-facing failures. Kept as defines so every site stays in sync. The text is
; plain: NSIS stores !define text verbatim, so a "$\r$\n" in here would show up
; in the message literally. Line breaks therefore live at the MessageBox calls.
!define ZN_RUNNING_MSG "ZipNest 仍在运行，请先关闭它再继续。"
!define ZN_RUNNING_MSG_EN "ZipNest is still running. Please close it, then run setup again."
!define ZN_WIN11_MSG "Win11 右键菜单注册失败（ZipNest 其余功能正常）。"
!define ZN_WIN11_MSG_EN "Windows 11 context menu registration failed. The rest of ZipNest works normally."

Name "${APPNAME}"
OutFile "ZipNest_0.3.6_x64-setup.exe"
InstallDir "${INSTDIR}"
InstallDirRegKey HKCU "${UNINSTKEY}" "InstallLocation"
RequestExecutionLevel admin
SetCompressor /SOLID lzma

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
; Offer to reopen ZipNest on the finish page (checked by default).
!define MUI_FINISHPAGE_RUN "$INSTDIR\zipnest.exe"
!define MUI_FINISHPAGE_RUN_TEXT "运行 ZipNest"
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "SimpChinese"

; Close a running ZipNest so its exe/dll are not locked while we overwrite
; them. Never force-kills first: ZipNest may be mid-extraction/mid-archive, so
; we ask it to close, wait, and only escalate to /F if it really will not go.
; On failure we tell the user and Abort instead of overwriting locked files.
;
; $0 = taskkill exit code (0 = a request was delivered / a kill succeeded;
;      128 = process not found, i.e. it is gone), $1/$2 = gentle retries,
;      $3 = forced-kill retries (allowed to wrap: we only care about parity).
!macro ZN_TRY_CLOSE_RUNNING pid_var label_prefix
  StrCpy ${pid_var} 0
  StrCpy $2 0
  StrCpy $3 0

  ; Phase 1: polite close request (no /F); N attempts, ~1s apart.
  ${label_prefix}_gentle:
    nsExec::ExecToLog '"$SYSDIR\taskkill.exe" /IM zipnest.exe'
    Pop $0
    IntCmp $0 0 ${label_prefix}_gentle_more ${label_prefix}_gentle_more ${label_prefix}_gone
  ${label_prefix}_gentle_more:
    Sleep 1000
    IntOp $2 $2 + 1
    IntCmp $2 10 ${label_prefix}_forced ${label_prefix}_gentle ${label_prefix}_gentle

  ; Phase 2: the app refused to close; force it, then re-check that it is gone.
  ${label_prefix}_forced:
    nsExec::ExecToLog '"$SYSDIR\taskkill.exe" /F /IM zipnest.exe'
    Pop $0
    ; 0 = killed, 128 = already gone, both mean we can carry on.
    IntCmp $0 0 ${label_prefix}_gone ${label_prefix}_force_more ${label_prefix}_force_more
  ${label_prefix}_force_more:
    Sleep 500
    IntOp $3 $3 + 1
    IntCmp $3 12 ${label_prefix}_abort ${label_prefix}_forced ${label_prefix}_forced

  ; Phase 3: still running -- refuse to overwrite its files.
  ${label_prefix}_abort:
    MessageBox MB_OK|MB_ICONSTOP "${ZN_RUNNING_MSG}$\r$\n$\r$\n${ZN_RUNNING_MSG_EN}"
    Abort
  ${label_prefix}_gone:
!macroend

Function CloseRunningZipNest
  !insertmacro ZN_TRY_CLOSE_RUNNING $1 zn_kill
FunctionEnd

; Same as above, for the uninstall section (which may only call un.* functions).
Function un.CloseRunningZipNest
  !insertmacro ZN_TRY_CLOSE_RUNNING $1 zn_ukill
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
  ; Certificate import / Add-AppxPackage failures used to be swallowed, so the
  ; user saw "installed" but had no context menu. Report, but do not abort:
  ; only the Win11 modern menu is missing.
  IntCmp $0 0 skip_register win11_register_failed skip_register
win11_register_failed:
  MessageBox MB_OK|MB_ICONEXCLAMATION "${ZN_WIN11_MSG}$\r$\n$\r$\n${ZN_WIN11_MSG_EN}"
skip_register:
FunctionEnd

; Remove the per-user package before its files are deleted.
Function un.UnregisterWin11Shell
  ReadRegStr $0 HKLM "SOFTWARE\Microsoft\Windows NT\CurrentVersion" "CurrentBuildNumber"
  IntCmpU $0 22000 do_unregister skip_unregister do_unregister
do_unregister:
  nsExec::ExecToLog 'powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$INSTDIR\Win11Shell\uninstall.ps1" -PackageDir "$INSTDIR\Win11Shell"'
  Pop $0
  ; Common on upgrades from an older release: uninstall.ps1 was never shipped,
  ; so the script cannot run. Files are still removed below, so just report it.
  IntCmp $0 0 skip_unregister win11_unregister_failed skip_unregister
win11_unregister_failed:
  DetailPrint "Win11 右键菜单卸载脚本执行失败（将直接删除文件）"
  MessageBox MB_OK|MB_ICONEXCLAMATION "${ZN_WIN11_MSG}$\r$\n$\r$\n${ZN_WIN11_MSG_EN}"
skip_unregister:
FunctionEnd

Section "Uninstall"
  ; Close a running ZipNest so its exe/dll can be deleted.
  Call un.CloseRunningZipNest
  ; Unregister the per-user package before deleting its manifest/files.
  Call un.UnregisterWin11Shell

  ; Drop the per-user shell integration while zipnest.exe still exists on disk.
  ; The app writes HKCU\Software\Classes\ZipNest.<ext> (9 extensions),
  ; HKCU\Software\Classes\*\shell\ZipNest, ...\Directory\shell\ZipNest,
  ; ...\Directory\Background\shell\ZipNest and the extensions' default values;
  ; --unregister-shell removes exactly those. HKCU\Software\Classes is not
  ; WOW64-redirected, so the 32-bit installer needs no SetRegView here.
  nsExec::ExecToLog '"$INSTDIR\zipnest.exe" --unregister-shell'
  Pop $0
  IntCmp $0 0 zn_shell_unregistered zn_shell_unregister_failed zn_shell_unregister_failed
zn_shell_unregister_failed:
  ; Never abort the uninstall over this: the files below are still removed.
  DetailPrint "右键菜单/文件关联清理失败（将记录残留项）"
zn_shell_unregistered:

  ; Program files. ClearErrors/IfErrors records whether any file survived
  ; (e.g. a running instance or AV lock) so we can skip the recursive cleanup.
  ClearErrors
  Delete /REBOOTOK "$INSTDIR\zipnest.exe"
  Delete /REBOOTOK "$INSTDIR\uninstall.exe"
  Delete /REBOOTOK "$INSTDIR\zipnest_shell.dll"
  Delete /REBOOTOK "$INSTDIR\engines\7z.dll"
  Delete /REBOOTOK "$INSTDIR\engines\sfx\7z.sfx"
  Delete /REBOOTOK "$INSTDIR\engines\sfx\7zCon.sfx"
  Delete /REBOOTOK "$INSTDIR\licenses\7zip.txt"
  Delete /REBOOTOK "$INSTDIR\Win11Shell\ZipNestShell.msix"
  Delete /REBOOTOK "$INSTDIR\Win11Shell\ZipNestCodesign.cer"
  Delete /REBOOTOK "$INSTDIR\Win11Shell\install.ps1"
  Delete /REBOOTOK "$INSTDIR\Win11Shell\uninstall.ps1"
  IfErrors zn_uninstall_keep_dirs zn_uninstall_drop_dirs

zn_uninstall_keep_dirs:
  ; A file is still there (locked / pending reboot). Keep every directory so
  ; the leftover file is not orphaned in a deleted tree.
  Goto zn_uninstall_dirs_done

zn_uninstall_drop_dirs:
  ; Only the program's own subfolders are removed recursively: the app lets the
  ; user drop their own files next to these, and the recursive form is scoped to
  ; these directories, never to $INSTDIR itself.
  RMDir /r "$INSTDIR\Win11Shell"   ; includes the AppX registration metadata
  RMDir /r "$INSTDIR\engines"
  RMDir /r "$INSTDIR\licenses"
  ; $INSTDIR itself is removed only when nothing but the app's own files is
  ; left in it, so a user's own file dropped in the install root survives.
  RMDir "$INSTDIR"
zn_uninstall_dirs_done:

  ; Runtime state owned by the app: settings.json (and any other per-user file
  ; the app created under that folder).
  RMDir /r "$APPDATA\ZipNest"

  Delete "$SMPROGRAMS\ZipNest\ZipNest.lnk"
  Delete "$SMPROGRAMS\ZipNest\卸载 ZipNest.lnk"
  RMDir "$SMPROGRAMS\ZipNest"
  Delete "$DESKTOP\ZipNest.lnk"
  DeleteRegKey HKCU "${UNINSTKEY}"
SectionEnd











