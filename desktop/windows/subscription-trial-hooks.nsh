; The experimental package must never install into an existing Zephium folder.
; Tauri's currentUser default is $LOCALAPPDATA\${PRODUCTNAME}. Keep this trial
; in that exact separate folder, even if /D or the directory picker is used.
; Do not include production installer-hooks.nsh: its uninstall hook removes
; the installed browser's protocol registrations.
!macro NSIS_HOOK_PREINSTALL
  StrCmp $INSTDIR "$LOCALAPPDATA\Zephium Subscription Trial" trial_directory_ok
  MessageBox MB_OK|MB_ICONSTOP "This trial must use its separate default folder. Your installed Zephium will not be changed."
  Abort
  trial_directory_ok:
  IfFileExists "$INSTDIR\zephium-desktop.exe" trial_existing_product
  IfFileExists "$INSTDIR\Zephium.exe" trial_existing_product trial_directory_safe
  trial_existing_product:
  MessageBox MB_OK|MB_ICONSTOP "An existing Zephium executable was found in the trial folder. Installation stopped without replacing it."
  Abort
  trial_directory_safe:
!macroend

; Remove only this trial's optional startup values. Never delete either parent
; key or the installed product's "Zephium" values, even during a trial upgrade.
!macro NSIS_HOOK_POSTUNINSTALL
  ${If} $UpdateMode <> 1
    DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "Zephium Subscription Trial"
    DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run" "Zephium Subscription Trial"
  ${EndIf}
!macroend

; No protocol, credential or profile cleanup. Tauri's ordinary uninstaller owns
; only this package's files, shortcuts and uninstall registration.
