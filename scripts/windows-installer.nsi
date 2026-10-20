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

Section "Install"
  InitPluginsDir
  SetOutPath "$PLUGINSDIR"
  File /oname=daedalus.exe "${BINARY}"
  ${If} $ParentPid == ""
    nsExec::ExecToStack '"$PLUGINSDIR\daedalus.exe" install --yes'
  ${Else}
    nsExec::ExecToStack '"$PLUGINSDIR\daedalus.exe" install --yes --wait-pid $ParentPid'
  ${EndIf}
  Pop $0
  Pop $1
  ${If} $0 != 0
    MessageBox MB_OK|MB_ICONSTOP "Daedalus could not install for this user. Stop its Telegram service from Telegram settings, close all Daedalus windows and try again. The existing installation was not removed."
    Abort
  ${EndIf}

  SetOutPath "$PROFILE"
  CreateShortCut "$SMPROGRAMS\Daedalus.lnk" "$INSTDIR\bin\daedalus.exe" "" "$INSTDIR\bin\daedalus.exe"
  WriteUninstaller "$INSTDIR\Uninstall Daedalus.exe"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Daedalus" "DisplayName" "Daedalus"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Daedalus" "DisplayVersion" "${PRODUCT_VERSION}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Daedalus" "UninstallString" "$\"$INSTDIR\Uninstall Daedalus.exe$\""
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Daedalus" "NoModify" 1
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Daedalus" "NoRepair" 1
  ExecShell "open" "$INSTDIR\bin\daedalus.exe"
SectionEnd

Section "Uninstall"
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
