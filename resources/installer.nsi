; GameOptimizer NSIS 安装脚本（免管理员，安装到当前用户）
Unicode true
!include "MUI2.nsh"

Name "GameOptimizer"
OutFile "GameOptimizer-setup.exe"
InstallDir "$LOCALAPPDATA\GameOptimizer"
RequestExecutionLevel user

!define MUI_ABORTWARNING
!define MUI_FINISHPAGE_RUN "$INSTDIR\gopt_cli.exe"
!define MUI_FINISHPAGE_RUN_PARAMETERS "--version"
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_LANGUAGE "SimpChinese"

Section "Main"
  SetOutPath "$INSTDIR"
  File "build\gopt_cli.exe"
  File "build\gopt_verify.exe"
  File "release\GameOptimizer\BEFORE_USE_README.txt"

  WriteUninstaller "$INSTDIR\uninstall.exe"

  CreateDirectory "$SMPROGRAMS\GameOptimizer"
  CreateShortcut "$SMPROGRAMS\GameOptimizer\GameOptimizer.lnk" "$INSTDIR\gopt_cli.exe"
  CreateShortcut "$SMPROGRAMS\GameOptimizer\README.lnk" "$INSTDIR\BEFORE_USE_README.txt"
  CreateShortcut "$SMPROGRAMS\GameOptimizer\Uninstall.lnk" "$INSTDIR\uninstall.exe"

  WriteRegStr HKCU "Software\GameOptimizer" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\GameOptimizer" "DisplayName" "GameOptimizer"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\GameOptimizer" "DisplayVersion" "1.0.8"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\GameOptimizer" "Publisher" "GameOptimizer"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\GameOptimizer" "DisplayIcon" "$INSTDIR\gopt_cli.exe"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\GameOptimizer" "UninstallString" '"$INSTDIR\uninstall.exe"'
SectionEnd

Section "Uninstall"
  Delete "$INSTDIR\gopt_cli.exe"
  Delete "$INSTDIR\gopt_verify.exe"
  Delete "$INSTDIR\BEFORE_USE_README.txt"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"

  Delete "$SMPROGRAMS\GameOptimizer\GameOptimizer.lnk"
  Delete "$SMPROGRAMS\GameOptimizer\README.lnk"
  Delete "$SMPROGRAMS\GameOptimizer\Uninstall.lnk"
  RMDir "$SMPROGRAMS\GameOptimizer"

  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\GameOptimizer"
  DeleteRegKey HKCU "Software\GameOptimizer"
SectionEnd
