!define SYNAPSE_CPU_NAME "Synapse CPU"
!define SYNAPSE_CPU_BUNDLEID "com.synapse.voice.cpu"
!define SYNAPSE_CPU_UNINSTKEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\Synapse CPU"
!define SYNAPSE_CPU_PRODUCTKEY "Software\Synapse\Synapse CPU"
!define SYNAPSE_RUNKEY "Software\Microsoft\Windows\CurrentVersion\Run"
!define SYNAPSE_APPROVEDKEY "Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run"

Var SynapseCpuStartMenu
Var SynapseCpuDesktop
Var SynapseCpuAutostart

!macro NSIS_HOOK_PREINSTALL
  Push $0
  Push $1
  Push $2
  StrCpy $SynapseCpuStartMenu 0
  StrCpy $SynapseCpuDesktop 0
  StrCpy $SynapseCpuAutostart 0
  ReadRegStr $0 HKCU "${SYNAPSE_CPU_UNINSTKEY}" "UninstallString"
  ReadRegStr $1 HKCU "${SYNAPSE_CPU_UNINSTKEY}" "DisplayName"
  ${If} "$0$1" != ""
    DetailPrint "Removing ${SYNAPSE_CPU_NAME}"
    ${If} ${FileExists} "$SMPROGRAMS\${SYNAPSE_CPU_NAME}.lnk"
      StrCpy $SynapseCpuStartMenu 1
    ${EndIf}
    ${If} ${FileExists} "$DESKTOP\${SYNAPSE_CPU_NAME}.lnk"
      StrCpy $SynapseCpuDesktop 1
    ${EndIf}
    ReadRegStr $2 HKCU "${SYNAPSE_RUNKEY}" "${SYNAPSE_CPU_NAME}"
    ${If} $2 != ""
      StrCpy $SynapseCpuAutostart 1
    ${EndIf}
    ReadRegStr $1 HKCU "${SYNAPSE_CPU_PRODUCTKEY}" ""
    ${If} $1 == ""
      ReadRegStr $1 HKCU "${SYNAPSE_CPU_UNINSTKEY}" "InstallLocation"
      StrCpy $2 $1 1
      ${If} $2 == "$\""
        StrCpy $1 $1 "" 1
      ${EndIf}
      StrCpy $2 $1 1 -1
      ${If} $2 == "$\""
        StrCpy $1 $1 -1
      ${EndIf}
    ${EndIf}
    StrCpy $0 "not run"
    ${If} $1 != ""
    ${AndIf} ${FileExists} "$1\uninstall.exe"
      ClearErrors
      ExecWait '"$1\uninstall.exe" /S _?=$1' $0
      ${IfThen} ${Errors} ${|} StrCpy $0 2 ${|}
      ${If} $0 == 0
        Delete "$1\uninstall.exe"
        ${If} $1 != $INSTDIR
          RMDir "$1"
        ${EndIf}
      ${EndIf}
    ${EndIf}
    DetailPrint "${SYNAPSE_CPU_NAME} uninstaller result: $0"
    ${If} $0 == "not run"
    ${OrIf} $0 == 0
      DeleteRegValue HKCU "${SYNAPSE_RUNKEY}" "${SYNAPSE_CPU_NAME}"
      DeleteRegValue HKCU "${SYNAPSE_APPROVEDKEY}" "${SYNAPSE_CPU_NAME}"
      Delete "$SMPROGRAMS\${SYNAPSE_CPU_NAME}.lnk"
      Delete "$DESKTOP\${SYNAPSE_CPU_NAME}.lnk"
      DeleteRegKey HKCU "${SYNAPSE_CPU_UNINSTKEY}"
      DeleteRegKey HKCU "${SYNAPSE_CPU_PRODUCTKEY}"
    ${Else}
      DetailPrint "${SYNAPSE_CPU_NAME} could not be removed automatically; its registration, shortcuts and autostart are kept"
      StrCpy $SynapseCpuStartMenu 0
      StrCpy $SynapseCpuDesktop 0
      StrCpy $SynapseCpuAutostart 0
    ${EndIf}
  ${EndIf}
  Pop $2
  Pop $1
  Pop $0
!macroend

!macro NSIS_HOOK_POSTINSTALL
  Push $0
  Push $1
  Push $2
  Push $3
  Push $4
  Push $5
  ${If} ${FileExists} "$INSTDIR\${MAINBINARYNAME}.exe"
  ${AndIf} ${FileExists} "$INSTDIR\resources\binaries\llama-server.exe"
    DetailPrint "Removing the bundled local AI server left by older versions"
    RMDir /r "$INSTDIR\resources\binaries"
  ${EndIf}
  ${If} $SynapseCpuStartMenu = 1
    ${IfNot} ${FileExists} "$SMPROGRAMS\${PRODUCTNAME}.lnk"
      CreateShortcut "$SMPROGRAMS\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
      !insertmacro SetLnkAppUserModelId "$SMPROGRAMS\${PRODUCTNAME}.lnk"
    ${EndIf}
  ${EndIf}
  ${If} $SynapseCpuDesktop = 1
    ${IfNot} ${FileExists} "$DESKTOP\${PRODUCTNAME}.lnk"
      CreateShortcut "$DESKTOP\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
      !insertmacro SetLnkAppUserModelId "$DESKTOP\${PRODUCTNAME}.lnk"
    ${EndIf}
  ${EndIf}
  ${If} $SynapseCpuAutostart = 1
    ReadRegStr $0 HKCU "${SYNAPSE_RUNKEY}" "${PRODUCTNAME}"
    ${If} $0 == ""
      WriteRegStr HKCU "${SYNAPSE_RUNKEY}" "${PRODUCTNAME}" '"$INSTDIR\${MAINBINARYNAME}.exe" --autostart'
    ${EndIf}
  ${EndIf}
  ${If} ${FileExists} "$LOCALAPPDATA\${SYNAPSE_CPU_BUNDLEID}\updates\*.*"
    RMDir /r "$LOCALAPPDATA\${SYNAPSE_CPU_BUNDLEID}\updates"
  ${EndIf}
  ClearErrors
  FindFirst $0 $1 "$TEMP\${SYNAPSE_CPU_NAME}-*-updater-*"
  ${DoUntil} ${Errors}
    ${If} $1 != ""
    ${AndIf} ${FileExists} "$TEMP\$1\*.*"
      RMDir /r "$TEMP\$1"
    ${EndIf}
    ClearErrors
    FindNext $0 $1
  ${Loop}
  FindClose $0
  Pop $5
  Pop $4
  Pop $3
  Pop $2
  Pop $1
  Pop $0
!macroend
