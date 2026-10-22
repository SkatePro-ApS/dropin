; Installer hooks for Tauri's NSIS template (bundle.windows.nsis.installerHooks).
;
; The installer carries the dropin CLI next to the app (Tauri only bundles the app itself), and puts
; the install folder on the user's PATH, so `dropin` works in any new terminal.
; build.beforeBundleCommand builds target\release\dropin.exe before Tauri bundles.

!include "WordFunc.nsh"
!include "WinMessages.nsh"

!macro NSIS_HOOK_POSTINSTALL
  SetOutPath "$INSTDIR"
  ; A dropin command running now (one waiting for the updated app, say) keeps dropin.exe open, and
  ; Windows refuses to overwrite a running exe but lets it be renamed. So the old one moves aside;
  ; the next install deletes it.
  Delete "$INSTDIR\dropin.old.exe"
  Rename "$INSTDIR\dropin.exe" "$INSTDIR\dropin.old.exe"
  ClearErrors
  ; Tauri copies this file to target\release\nsis\x64, so the CLI is two folders up.
  File "${__FILEDIR__}\..\..\dropin.exe"

  ; Append $INSTDIR to the user's PATH unless it is there already. The value is read and written
  ; raw (REG_EXPAND_SZ), so entries like %USERPROFILE%\bin stay unexpanded.
  ReadRegStr $0 HKCU "Environment" "Path"
  ClearErrors
  ${WordFind} ";$0;" ";$INSTDIR;" "E*" $1
  ${If} ${Errors}
    ${If} $0 == ""
      StrCpy $0 "$INSTDIR"
    ${Else}
      StrCpy $0 "$0;$INSTDIR"
    ${EndIf}
    WriteRegExpandStr HKCU "Environment" "Path" "$0"
    SendMessage ${HWND_BROADCAST} ${WM_SETTINGCHANGE} 0 "STR:Environment" /TIMEOUT=5000
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  Delete "$INSTDIR\dropin.exe"
  Delete "$INSTDIR\dropin.old.exe"
  RMDir "$INSTDIR"

  ; Take $INSTDIR back off the user's PATH.
  ReadRegStr $0 HKCU "Environment" "Path"
  ${un.WordReplace} ";$0;" ";$INSTDIR;" ";" "+" $1
  StrCpy $1 $1 "" 1
  StrCpy $1 $1 -1
  ${If} $1 != $0
    WriteRegExpandStr HKCU "Environment" "Path" "$1"
    SendMessage ${HWND_BROADCAST} ${WM_SETTINGCHANGE} 0 "STR:Environment" /TIMEOUT=5000
  ${EndIf}
!macroend
