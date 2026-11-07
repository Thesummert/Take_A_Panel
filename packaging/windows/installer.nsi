Unicode true
RequestExecutionLevel user
Name "Take a Panel"
OutFile "${OUTPUT_FILE}"
InstallDir "$LOCALAPPDATA\Programs\Take a Panel"
Icon "${APP_ICON}"
UninstallIcon "${APP_ICON}"
SetCompressor /SOLID lzma

!include "MUI2.nsh"
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_LICENSE "${LICENSE_FILE}"
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

Section "Install"
    SetShellVarContext current
    SetOutPath "$INSTDIR"
    ClearErrors
    File /oname=take-a-panel.exe "${APP_BINARY}"
    IfErrors 0 +3
        MessageBox MB_ICONSTOP "Installation failed. Exit Take a Panel from its tray menu and try again."
        Abort
    File /oname=LICENSE "${LICENSE_FILE}"
    WriteUninstaller "$INSTDIR\Uninstall.exe"
    CreateDirectory "$SMPROGRAMS\Take a Panel"
    CreateShortcut "$SMPROGRAMS\Take a Panel\Take a Panel.lnk" "$INSTDIR\take-a-panel.exe"
    CreateShortcut "$SMPROGRAMS\Take a Panel\Uninstall Take a Panel.lnk" "$INSTDIR\Uninstall.exe"
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\TakeAPanel" "DisplayName" "Take a Panel"
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\TakeAPanel" "DisplayVersion" "${APP_VERSION}"
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\TakeAPanel" "DisplayIcon" "$INSTDIR\take-a-panel.exe"
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\TakeAPanel" "UninstallString" '"$INSTDIR\Uninstall.exe"'
    WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\TakeAPanel" "NoModify" 1
    WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\TakeAPanel" "NoRepair" 1
SectionEnd

Section "Uninstall"
    SetShellVarContext current
    ClearErrors
    Delete "$INSTDIR\take-a-panel.exe"
    IfErrors 0 +3
        MessageBox MB_ICONSTOP "Uninstall failed. Exit Take a Panel from its tray menu and try again."
        Abort
    ReadRegStr $0 HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "Take a Panel"
    StrCmp $0 '"$INSTDIR\take-a-panel.exe" --hidden' 0 +2
        DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "Take a Panel"
    Delete "$INSTDIR\LICENSE"
    Delete "$SMPROGRAMS\Take a Panel\Take a Panel.lnk"
    Delete "$SMPROGRAMS\Take a Panel\Uninstall Take a Panel.lnk"
    RMDir "$SMPROGRAMS\Take a Panel"
    DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\TakeAPanel"
    Delete "$INSTDIR\Uninstall.exe"
    RMDir "$INSTDIR"
SectionEnd
