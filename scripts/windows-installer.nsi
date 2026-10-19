; Per-user Windows installer. The embedded Daedalus binary performs the
; owner/ACL checks and atomic copy; NSIS only supplies a one-click UI.
Unicode true
!include "LogicLib.nsh"
!ifndef PRODUCT_VERSION
  !error "PRODUCT_VERSION is required"
!endif
!ifndef BINARY
  !error "BINARY is required"
!endif
!ifndef OUTPUT
  !error "OUTPUT is required"
!endif

Name "Daedalus ${PRODUCT_VERSION}"
OutFile "${OUTPUT}"
InstallDir "$LOCALAPPDATA\Daedalus"
RequestExecutionLevel user
ShowInstDetails show
ShowUninstDetails show
Page instfiles
UninstPage instfiles

Section "Install"
  InitPluginsDir
  SetOutPath "$PLUGINSDIR"
  File /oname=daedalus.exe "${BINARY}"
  nsExec::ExecToStack '"$PLUGINSDIR\daedalus.exe" install --yes'
  Pop $0
  Pop $1
  ${If} $0 != 0
    MessageBox MB_OK|MB_ICONSTOP "Daedalus could not install for this user. Close any running Daedalus window and try again. The existing installation was not removed."
    Abort
  ${EndIf}

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
