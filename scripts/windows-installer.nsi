; Per-user Windows installer. The embedded Daedalus binary performs the
; owner/ACL checks and atomic copy; NSIS only supplies a one-click UI.
Unicode true
!include "LogicLib.nsh"
!include "FileFunc.nsh"
!insertmacro GetParameters
!insertmacro GetOptions
!ifndef PRODUCT_VERSION
  !error "PRODUCT_VERSION is required"
!endif
!ifndef BINARY
  !error "BINARY is required"
!endif
!ifndef OUTPUT
  !error "OUTPUT is required"
!endif
!ifndef ICON
  !error "ICON is required"
!endif

Name "Daedalus ${PRODUCT_VERSION}"
Icon "${ICON}"
UninstallIcon "${ICON}"
OutFile "${OUTPUT}"
InstallDir "$LOCALAPPDATA\Daedalus"
RequestExecutionLevel user
ShowInstDetails show
ShowUninstDetails show
Page instfiles
UninstPage instfiles
Var ParentPid

Function .onInit
  ${GetParameters} $R0
  ClearErrors
  ${GetOptions} $R0 "/PARENT_PID=" $ParentPid
  IfErrors 0 +2
    StrCpy $ParentPid ""
FunctionEnd

; In-app update passes /PARENT_PID=. Wait only; never terminate that process.
; Reject values signed 32-bit atoi cannot round-trip, so OpenProcess cannot
; attach to the wrong pid. An empty value is a manual setup run.
Function WaitForParentExit
  StrCmp $ParentPid "" pp_return
  StrCpy $R5 0
  StrCpy $R8 0
  StrLen $R6 $ParentPid
  IntCmp $R6 10 pp_digits pp_digits pp_bad
pp_digits:
  IntCmp $R5 $R6 pp_range pp_scan pp_range
pp_scan:
  StrCpy $R7 $ParentPid 1 $R5
  StrCpy $R4 "0123456789"
  StrCpy $R3 0
pp_find:
  StrCpy $R2 $R4 1 $R3
  StrCmp $R2 "" pp_bad
  StrCmp $R2 $R7 pp_found
  IntOp $R3 $R3 + 1
  Goto pp_find
pp_found:
  StrCmp $R7 "0" pp_next
  StrCpy $R8 1
pp_next:
  IntOp $R5 $R5 + 1
  Goto pp_digits
pp_range:
  StrCmp $R8 "1" 0 pp_bad
  IntCmp $R6 10 0 pp_wait pp_wait
  StrCpy $R5 0
  StrCpy $R4 "2147483647"
pp_bound:
  StrCpy $R7 $ParentPid 1 $R5
  StrCpy $R2 $R4 1 $R5
  IntCmp $R7 $R2 pp_bound_next pp_wait pp_bad
pp_bound_next:
  IntOp $R5 $R5 + 1
  IntCmp $R5 10 pp_wait pp_bound pp_wait
pp_wait:
  DetailPrint "Waiting for the previous Daedalus process to exit..."
  ; SYNCHRONIZE only. Do not request PROCESS_TERMINATE.
  System::Call 'kernel32::OpenProcess(i 1048576, i 0, i $ParentPid) i.R1 ?e'
  Pop $R2
  IntCmp $R1 0 0 pp_opened pp_opened
  ; ERROR_INVALID_PARAMETER: the launching process has already exited.
  IntCmp $R2 87 pp_return pp_bad_open pp_bad_open
pp_bad_open:
  MessageBox MB_OK|MB_ICONSTOP "Cannot wait for the previous Daedalus process. It was not terminated. No installation files or private configuration were deleted."
  Abort
pp_opened:
  System::Call 'kernel32::WaitForSingleObject(i R1, i 45000) i.R2'
  System::Call 'kernel32::CloseHandle(i R1)'
  IntCmp $R2 0 pp_return pp_wait_fail 0
  IntCmp $R2 258 pp_timeout pp_wait_fail pp_wait_fail
pp_timeout:
  MessageBox MB_OK|MB_ICONSTOP "The previous Daedalus process is still running. It was not terminated. Close it and retry. No installation files or private configuration were deleted."
  Abort
pp_wait_fail:
  MessageBox MB_OK|MB_ICONSTOP "Cannot confirm the previous Daedalus process exited. It was not terminated. No installation files or private configuration were deleted."
  Abort
pp_bad:
  MessageBox MB_OK|MB_ICONSTOP "The installer cannot safely wait for this parent process id. It was not terminated. Close Daedalus and run setup again. No installation files or private configuration were deleted."
  Abort
pp_return:
FunctionEnd

; Parent wait only. install --yes records which Telegram services are
; running, replaces the image, and starts those instances from the installed exe.
; Stopping here would drop that list and leave Telegram down.
Function ReleaseInstalledExecutable
  Call WaitForParentExit
FunctionEnd

Section "Install"
  InitPluginsDir
  SetOutPath "$PLUGINSDIR"
  File /oname=daedalus.exe "${BINARY}"
  Call ReleaseInstalledExecutable
  SetOutPath "$PROFILE"
  ${If} $ParentPid == ""
    nsExec::ExecToStack '"$PLUGINSDIR\daedalus.exe" install --yes'
  ${Else}
    nsExec::ExecToStack '"$PLUGINSDIR\daedalus.exe" install --yes --wait-pid $ParentPid'
  ${EndIf}
  Pop $0
  Pop $1
  StrCmp $0 "0" inst_copied
  DetailPrint "$1"
  MessageBox MB_OK|MB_ICONSTOP "Daedalus could not install for this user. No installation files or private configuration were deleted. Close any remaining Daedalus windows and retry.$\r$\n$\r$\n$1"
  Abort
inst_copied:
  CreateShortCut "$SMPROGRAMS\Daedalus.lnk" "$INSTDIR\bin\daedalus.exe" "" "$INSTDIR\bin\daedalus.exe"
  WriteUninstaller "$INSTDIR\Uninstall Daedalus.exe"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Daedalus" "DisplayName" "Daedalus"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Daedalus" "DisplayVersion" "${PRODUCT_VERSION}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Daedalus" "UninstallString" "$\"$INSTDIR\Uninstall Daedalus.exe$\""
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Daedalus" "NoModify" 1
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Daedalus" "NoRepair" 1
  IfSilent installed
    ExecShell "open" "$INSTDIR\bin\daedalus.exe"
  installed:
SectionEnd

Section "Uninstall"
  InitPluginsDir
  SetOutPath "$PLUGINSDIR"
  File /oname=daedalus-maintenance.exe "${BINARY}"
  ; Resolve configuration from the profile, not the plugin temporary directory.
  SetOutPath "$PROFILE"
  DetailPrint "Stopping the managed Telegram service gracefully..."
  ; Timeout kills this helper only. The helper does not kill Telegram.
  nsExec::ExecToStack /TIMEOUT=180000 '"$PLUGINSDIR\daedalus-maintenance.exe" uninstall --prepare --yes'
  Pop $0
  Pop $1
  ${If} $0 != 0
    DetailPrint "$1"
    MessageBox MB_OK|MB_ICONSTOP "Daedalus could not finish a safe shutdown. Close its other windows and retry after Telegram stops. No installation files or private configuration were removed.$\r$\n$\r$\n$1"
    Abort
  ${EndIf}
  SetOutPath "$PROFILE"
  ClearErrors
  Delete "$INSTDIR\bin\daedalus.exe"
  IfErrors 0 +3
    MessageBox MB_OK|MB_ICONSTOP "Close Daedalus before uninstalling it. Your configuration has not been removed."
    Abort
  Delete "$SMPROGRAMS\Daedalus.lnk"
  Delete "$INSTDIR\install.json"
  Delete "$INSTDIR\Uninstall Daedalus.exe"
  RMDir "$INSTDIR\bin"
  RMDir "$INSTDIR"
  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Daedalus"
SectionEnd
