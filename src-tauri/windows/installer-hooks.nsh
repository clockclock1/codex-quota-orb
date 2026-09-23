!macro NSIS_HOOK_POSTINSTALL
  ; Always create or refresh the desktop shortcut, including upgrade installs.
  CreateShortcut "$DESKTOP\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe" "" "$INSTDIR\${MAINBINARYNAME}.exe" 0 SW_SHOWNORMAL "" "${PRODUCTNAME}"
  !insertmacro SetLnkAppUserModelId "$DESKTOP\${PRODUCTNAME}.lnk"
!macroend
