; Installer hooks for the NSIS package.

!macro NSIS_HOOK_POSTUNINSTALL
  ; The studio keeps its models beside the executable, and the template's
  ; "delete application data" only knows about the profile: when it is ticked,
  ; the data folder goes too, then the now empty installation directory.
  ${If} $DeleteAppDataCheckboxState = 1
  ${AndIf} $UpdateMode <> 1
    RMDir /r "$INSTDIR\data"
  ${EndIf}
  RMDir "$INSTDIR"
!macroend
