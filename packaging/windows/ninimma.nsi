; Ninimma modification: ordinary-user installer with Start menu shortcuts.
Unicode true
!include "MUI2.nsh"
Name "Ninimma"
OutFile "ninimma-windows-x64-setup.exe"
InstallDir "$LOCALAPPDATA\Programs\Ninimma"
RequestExecutionLevel user
Icon "assets\icons\ninimma.ico"
!define MUI_ABORTWARNING
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_LICENSE "LICENSE"
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"
Section "Ninimma"
  SetShellVarContext current
  SetOutPath "$INSTDIR"
  File "target\release\ninimma.exe"
  File "assets\icons\ninimma.ico"
  File "LICENSE"
  File "NOTICE"
  File /oname=LICENSE-lucide "packaging\icons\LICENSE-lucide"
  CreateDirectory "$SMPROGRAMS\Ninimma"
  CreateShortcut "$SMPROGRAMS\Ninimma\Ninimma.lnk" "$INSTDIR\ninimma.exe" "" "$INSTDIR\ninimma.ico"
  WriteUninstaller "$INSTDIR\Uninstall.exe"
  CreateShortcut "$SMPROGRAMS\Ninimma\Uninstall.lnk" "$INSTDIR\Uninstall.exe"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Ninimma" "DisplayName" "Ninimma"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Ninimma" "UninstallString" '$\"$INSTDIR\Uninstall.exe$\"'
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Ninimma" "DisplayIcon" "$INSTDIR\ninimma.ico"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Ninimma" "DisplayVersion" "0.1.0"
SectionEnd
Section "Uninstall"
  SetShellVarContext current
  Delete "$SMPROGRAMS\Ninimma\Ninimma.lnk"
  Delete "$SMPROGRAMS\Ninimma\Uninstall.lnk"
  RMDir "$SMPROGRAMS\Ninimma"
  Delete "$INSTDIR\ninimma.exe"
  Delete "$INSTDIR\ninimma.ico"
  Delete "$INSTDIR\LICENSE"
  Delete "$INSTDIR\NOTICE"
  Delete "$INSTDIR\LICENSE-lucide"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir "$INSTDIR"
  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Ninimma"
SectionEnd
